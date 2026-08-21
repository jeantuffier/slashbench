# Three bugs behind Slashbench's real numbers

*A postmortem from building [Slashbench](./CLAUDE.md), a reproducible benchmark comparing Rust/JVM/JS backend cloud costs, for the JavaZone 2026 talk "Rust will slash your backend costs."*

The first two of these bugs made two Rust services look four times slower than they really were. The third was the opposite kind of surprise: a bug that had been sitting, unnoticed, in every one of the six services from day one — one that made *every* stack fail identically, for a reason that had nothing to do with language or runtime at all, and that only a longer test could ever have exposed.

## Context

Slashbench measures the minimum memory footprint six backend stacks (Rust/Rocket, Rust/Actix-web, Java/Spring Boot, Kotlin/Spring Boot, Node/Hono, Bun/Hono) need to sustain a shared target load. Before that measurement means anything, the benchmark first has to find that shared target load — a `dry-run` step that ramps each stack's *uncapped* throughput up a ladder (50, 100, 200, 400, 800, 1600, 3200 req/s) until its SLA (p99 < 200ms, error rate < 1%) breaks, takes the weakest stack's ceiling, and uses 60% of it as the load every other test runs at.

Infrastructure: two Scaleway `STANDARD3-X4C-16G` VMs (4 dedicated vCPUs, 16 GB RAM, Debian 12) in `fr-par-1` — one running the target container under test, one running the k6 load generator, connected over a real network (not loopback) with the orchestrator CLI driving Docker on the target via `DOCKER_HOST=ssh://`. Each rate step in the ladder gets a discarded warm-up run, then three measured repeats, with the **median** p99/error-rate deciding pass or fail — rigor added specifically because an earlier single-shot version of this test swung by 8x between two otherwise-identical runs on real cloud infrastructure.

## The result that shouldn't have happened

The first real run of this ladder, on real hardware, produced this:

| Stack | Ceiling |
|---|---|
| Rocket (Rust) | **400 req/s** |
| Actix-web (Rust) | **400 req/s** |
| Spring Boot (Java) | 1600 req/s |
| Spring Boot (Kotlin) | 1600 req/s |
| Hono (Node) | 1600 req/s |
| Hono (Bun) | 1600 req/s |

Both Rust stacks capped at exactly a quarter of what every JVM and JS stack reached. That's not a small effect, and it's backwards from this benchmark's entire premise. It would have been easy to write this down as "Rust doesn't win on raw throughput, only memory" and move on — the two stacks capping at the *identical* number looked like confirmation of one shared explanation.

It wasn't. These turned out to be two completely unrelated bugs, in two different libraries, with two different root causes and two very different (and very short) fixes. Finding that out took two separate investigations, each one built on ruling out plausible-sounding explanations with direct evidence rather than accepting them on reasoning alone.

---

## Investigation #1: Rocket's hardcoded TCP backlog

### The first (wrong) hypothesis: connection pool size

The initial theory was database connection pool size. Slashbench's fairness rule already required all six stacks to use raw drivers instead of an ORM specifically so language/runtime differences wouldn't be confounded by driver overhead — but no one had extended that "matched across all six" principle to pool *size*. Checking the defaults: sqlx's `PgPool::connect()`, Spring's HikariCP, and postgres.js all nominally default to about 10 connections. Coincidentally uniform, but never explicitly set anywhere.

Fix attempted: explicitly set `max_connections(20)` (or the equivalent) identically across all six stacks — sqlx's `PgPoolOptions`, Spring's `spring.datasource.hikari.maximum-pool-size`, postgres.js's `max`. Rebuilt, redeployed, retested.

**Result: zero effect.** Identical failure at rate=800, byte-for-byte the same symptom. This ruled out pool size immediately and cleanly — worth noting because pool size turns out to matter a great deal for the *second* bug in this story, just not this one.

### Testing connection reuse directly (Mode A vs Mode B)

The failure pattern was specific: k6 logged `dial: i/o timeout` errors, and a test configured for 10 seconds was taking 40+ seconds to complete. That's the signature of connections failing to *establish*, not of slow request processing.

The next hypothesis was that a burst of new connections at test start was overwhelming something. This is testable directly: k6's HTTP client reuses a TCP connection across a VU's iterations by default ("Mode A"); passing `--no-connection-reuse` forces a brand-new connection for every single request ("Mode B"). The prediction: if the problem were too many connections opening in a tight window, forcing *more* connection churn (Mode B) should make it *worse*, not better.

The result was the opposite of the prediction. At the failing rate (800 req/s):

- **Mode B** (`--no-connection-reuse`): 0% errors, 799.68/800 req/s achieved, p99 = 7.45ms.
- **Mode A** (default reuse): the same `dial: i/o timeout` failures as before.

More connections were *fine*. Fewer, reused ones were failing. This ruled out "too many simultaneous new connections" and pointed somewhere more specific: something about *reused* connections, or about the volume of connections needed to sustain the target rate when the executor has to compensate for stuck ones.

A follow-up test using k6's `rampingArrivalRate` executor (reaching 800 req/s gradually over 30 seconds, then holding, instead of jumping straight there) came back *worse* — 18-20% errors instead of 3-6%, and the `iteration_duration` p90 also started hitting a 30-second ceiling (previously only p95/p99 did). A longer test at high load made things worse, not better. This ruled out "it's just an initial burst" too.

### Packet-level diagnosis

At this point the only way forward was looking at the actual packets. `tcpdump` on the target VM during a live failure window, focused first on `FIN`/`RST` flags to check whether either side was prematurely closing connections:

- Every `FIN` packet was client-initiated (from the load generator), and they all shared the *exact same microsecond timestamp* — that's k6's own end-of-test teardown closing every VU's connection at once, not a mid-test problem.
- Only 2 `RST` packets in the entire capture. Negligible.

Neither side was closing connections early. So the next check was `SYN` packets specifically — tracing one connection attempt that never completed, on a single source port, end to end:

```
11:19:15.136200  SYN  seq=1449653343  (initial attempt)
11:19:16.142173  SYN  seq=1449653343  (+1.0s — retry 1)
11:19:18.162182  SYN  seq=1449653343  (+2.0s — retry 2)
11:19:22.190216  SYN  seq=1449653343  (+4.0s — retry 3)
11:19:30.382300  SYN  seq=1449653343  (+8.2s — retry 4)
```

Same sequence number retried five times with classic Linux exponential backoff (1s, 2s, 4s, 8s), and **not one SYN-ACK or RST in response, ever**. That's not a rejection — a rejection sends a RST immediately. This is the specific, well-known signature of a full TCP listen backlog: the kernel silently drops excess incoming SYNs rather than acknowledging or resetting them, relying on the client's own retry timer.

### Finding where the backlog was actually small

The obvious next check — `net.core.somaxconn` and `net.ipv4.tcp_max_syn_backlog` on the target VM — came back generous: 4096 and 1024 respectively. These are host-wide settings, and the other five stacks reached 1600 req/s on that same host, so a small OS-level ceiling wasn't the direct explanation (though it's part of the story below).

Checking the actual listening socket with `ss -tanl` on the host produced a red herring: `Send-Q=4096` for port 8080. That looked like the backlog was already fine. It wasn't measuring the right thing — `docker-proxy` processes were running for the published port, and the host-level socket being inspected was `docker-proxy`'s own forwarding listener, not Rocket's. The actual data path, confirmed separately, runs through kernel-level iptables DNAT — `docker-proxy` mostly just holds the port open.

Rocket's real listener lives inside the container's own network namespace. Entering it directly settled the question:

```bash
PID=$(docker inspect -f '{{.State.Pid}}' slashbench-rocket-1)
sudo nsenter -t $PID -n ss -tanl
```

```
LISTEN  0  128  0.0.0.0:8080  0.0.0.0:*
```

**128.** Not 4096. That's the real number.

### Root cause: Rust's standard library hardcodes it

Rocket 0.5.1 uses Hyper 0.14.32 for its HTTP server. Fetching Hyper 0.14's actual source for its listener setup:

```rust
pub(super) fn new(addr: &SocketAddr) -> crate::Result<Self> {
    let std_listener = StdTcpListener::bind(addr).map_err(crate::Error::new_listen)?;
    AddrIncoming::from_std(std_listener)
}
```

Plain `std::net::TcpListener::bind()`, no backlog parameter passed anywhere. And Rust's standard library, on Linux, hardcodes that backlog to 128 — a long-tracked issue, [rust-lang/rust#55614](https://github.com/rust-lang/rust/issues/55614) ("Hardcoded 128 backlog in TCPListener").

The issue is marked *closed*, with the stated resolution being that Rust now passes `-1` to `listen()` on Linux, which the kernel interprets as "use the platform maximum" (`somaxconn`) rather than a hardcoded number. Taking that at face value would have been a mistake — the empirical measurement above, on the current toolchain, said otherwise. Rather than trust a multi-year-old issue thread's summary, the fix was verified directly: a **minimal, isolated Rust program**, no Hyper, no Rocket, just `TcpListener::bind()`, built with the exact same `rust:1-slim` image and run on the same VM:

```rust
fn main() {
    let _listener = std::net::TcpListener::bind("0.0.0.0:9999").unwrap();
    std::thread::sleep(std::time::Duration::from_secs(30));
}
```

```
$ rustc --version
rustc 1.97.1 (8bab26f4f 2026-07-14)

$ ss -tanl
LISTEN  0  128  0.0.0.0:9999  0.0.0.0:*
```

Still 128, on the current stable compiler, months after the issue was closed. Whatever the fix covers, it doesn't cover this exact path on this exact platform in practice — a good reminder that a closed GitHub issue describes an intention, not necessarily the observable behavior of the toolchain in your hands. (Two other tempting theories were checked and ruled out along the way: that `somaxconn` might differ *inside* the container's network namespace versus the host — it didn't, both read 4096 — and that Rocket's own config might expose a way to raise this. It doesn't: a `Listener` trait for injecting a custom pre-bound socket is a real, documented Rocket feature, but per [Rocket's own v0.5 announcement](https://rocket.rs/news/2023-11-17-version-0.5/) it's explicitly scoped to "the next major release after v0.5" — not present in the 0.5.1 this project is pinned to.)

### The fix: intercepting `listen()` at the libc level

With no way to raise the backlog through Rust, Hyper, or Rocket's own APIs, the fix works one layer lower — `LD_PRELOAD`, a standard Linux mechanism for transparently overriding library calls without touching or recompiling the target binary. A ~20-line C shared library intercepts `listen(2)` itself:

```c
#define _GNU_SOURCE
#include <dlfcn.h>
#include <sys/socket.h>
#include <stdio.h>
#include <stdlib.h>

int listen(int sockfd, int backlog) {
    static int (*real_listen)(int, int) = NULL;
    if (!real_listen) {
        real_listen = dlsym(RTLD_NEXT, "listen");
    }
    int min_backlog = 4096;
    const char *env = getenv("LISTEN_OVERRIDE_BACKLOG");
    if (env) {
        min_backlog = atoi(env);
    }
    int forced = backlog > min_backlog ? backlog : min_backlog;
    fprintf(stderr, "[listen_override] listen(fd=%d, requested=%d) -> forcing backlog=%d\n", sockfd, backlog, forced);
    return real_listen(sockfd, forced);
}
```

Whatever backlog the application asks for, this substitutes `max(requested, 4096)` before calling the real `listen()`. The application never knows the difference. Wired into the Docker build as an extra stage (compiled with `gcc`, copied into the final image) and activated via one environment variable:

```dockerfile
FROM gcc:latest AS shim-builder
WORKDIR /shim
COPY listen_override.c ./
RUN gcc -shared -fPIC -o listen_override.so listen_override.c -ldl
```

```yaml
environment:
  LD_PRELOAD: /app/listen_override.so
```

Verified first on the isolated minimal test binary (128 → 4096, confirmed via the same `nsenter` + `ss` technique), then on the real Rocket container:

```
[listen_override] listen(fd=13, requested=128) -> forcing backlog=4096
```

```
LISTEN  0  4096  0.0.0.0:8080  0.0.0.0:*
```

### Result

Re-run through the full three-repeat, median-based dry-run methodology: Rocket passed cleanly through **1600 req/s** (0% errors, p99 in the 7–22ms range across repeats), only failing at 3200. A genuine 4x improvement, landing exactly at parity with every JVM and JS stack — not a workaround that papers over a slow number, an actual fix for an actual bug.

---

## Investigation #2: Actix-web's keep-alive timeout

### Same symptom, assumed same cause — wrong assumption

The same `LD_PRELOAD` shim was applied to Actix-web too (it also has a fixed backlog — 1024 by default, per its own `HttpServer::backlog()` documentation, still capped well below what's needed). Confirmed working the same way: `nsenter` showed the real internal socket backlog raised from 1024 to 4096.

Re-running the full dry-run: Rocket now reached 1600 as expected. **Actix-web was still stuck at exactly 400.** The identical fix, confirmed active on the identical class of bug, had zero effect. That's the strongest possible signal that despite the identical-looking symptom, this was a genuinely different problem — and it was tempting, after the Rocket investigation, to assume it must be some variant of the same backlog story. It wasn't.

### Reproducing it directly

Running the same rate=800 test against Actix-web with full diagnostics showed something categorically different from Rocket's `dial: i/o timeout` errors:

```
checks_failed......: 0.00%   0 out of 16001
http_req_duration..: avg=876ms  p90=5.74ms  p95=22.29ms  p99=22.49s
```

**Zero errors.** Every single request eventually succeeded. But the p99 latency was 22.49 *seconds* — four orders of magnitude past the p95. Not connection failures; something was making a small fraction of requests take catastrophically long while the rest were fine.

k6's per-phase sub-metrics pinned down where: `http_req_connecting` and `http_req_blocked` (the connection-establishment phases) stayed fast throughout — max around 21–23ms, negligible next to 23 seconds. `http_req_waiting` — time spent waiting for the server's response after the request was already fully sent — had a p99 of **22,880ms**. The connection was opening fine. The *application* was the one taking 23 seconds to answer, for a subset of requests.

### Three more hypotheses, three more falsifications

**Worker count.** Actix-web defaults to `workers = num_cpus::get()` (4 here), each an independent single-threaded executor — architecturally different from Rocket/Hyper's single shared multi-threaded runtime. The theory: under high concurrency, work could queue up badly within one overloaded worker's single thread while other workers sat idle. Tested by explicitly setting `.workers(64)`.

Result: *worse*. Errors appeared that weren't there before (3.21% failed), and the stall, if anything, got more pronounced. Reverted. Whatever this is, adding more independent workers didn't help and may have added more surface area for it.

As a sanity check against the possibility that this was some artifact of the test tooling rather than Actix-web itself, the identical test was run against Node-hono (a known-good stack): it reached 798.9/800 req/s with only 89 max VUs needed — nothing like Actix's 600+. Confirmed Actix-specific, not a k6 quirk.

**Database contention.** A live `pg_stat_activity` monitor, sampling every second for the full duration of a failing test:

```sql
SELECT pid, state, wait_event_type, wait_event, query, EXTRACT(EPOCH FROM (now() - query_start)) AS secs
FROM pg_stat_activity WHERE datname='slashbench' AND state != 'idle';
```

25 samples across a complete failing run. **Zero Actix-web activity in any of them.** If a request were stuck waiting on a slow query or a lock, Postgres would show it as `active` with a wait event for the whole stuck duration — a 22-second stall should be trivially easy to catch even at one-second sampling resolution. Seeing nothing meant the stuck requests never reached Postgres at all — ruling out slow queries and lock contention, and pointing at something upstream of the database entirely (most plausibly: waiting to acquire a pooled connection, or stuck somewhere even earlier).

That reasoning motivated the next test directly: connection pool exhaustion. `max_connections` raised from 20 to 200 (10x). Result: **zero effect**, p99 still 22.77s. Reverted back to 20 to keep the six stacks matched. (Little's Law made this result unsurprising in hindsight: at ~1–2ms per query, even 20 connections should sustain far more than 800 req/s — the bottleneck was never pool capacity.)

**Slow DNS on lazily-created connections.** Sqlx's pool creates connections on demand by default, not upfront — and a *real*, previously-observed hard crash earlier in this project (`"failed to lookup address information: Temporary failure in name resolution"`, resolving the `postgres` hostname via Docker's embedded DNS) made "an occasional slow-but-not-failed DNS lookup during on-demand connection creation" a plausible mechanism. Tested by adding `.min_connections(20)`, forcing every pool connection to be created eagerly at startup, before any load hits the service.

Result: zero effect. Reverted.

### A retransmission count that lied

With three hypotheses down, the packet-level check returned — this time not for `SYN`/`FIN`/`RST` flags (already known to be clean from the Rocket investigation's technique), but a full capture analyzed with `tshark`'s dedicated retransmission detector:

```bash
tshark -r capture.pcap -Y "tcp.analysis.retransmission and tcp.len > 0"
```

The first pass came back alarming: **31,911 real data retransmissions**, sustained at roughly 1,600 per second for the entire 20-second test, continuing into the 23–29 second tail that matched the observed stall duration almost perfectly. It looked like conclusive proof of serious packet loss.

It wasn't real. The count (16,001 in one direction, 15,901 in the other) was suspiciously close to the *total request count* — actual packet loss affects a fraction of traffic, not nearly all of it. This was the same capture artifact already learned about during the Rocket investigation, just forgotten under pressure: capturing on `-i any` in a Docker environment catches the *same logical packet* multiple times as it crosses separate virtual interfaces (the physical NIC, the bridge, the container's veth pair). `tshark`'s retransmission heuristic was seeing near-duplicate captures of single packets and flagging them as retransmissions.

The fix was to capture on exactly one interface. Finding the *right* one required matching the container's own view of its network interface to the host's side of the veth pair:

```bash
# Inside the container's network namespace:
$ ip -o link show
2: eth0@if14: ...

# On the host, match ifindex 14 to its veth name:
$ for f in /sys/class/net/veth*/ifindex; do echo "$(dirname $f) -> $(cat $f)"; done
veth3ed8d5d -> 5
vetha252005 -> 14   # <- this one
```

Recapturing on `vetha252005` alone, during a freshly reproduced failure: **1 real retransmission out of 59,173 packets.** Essentially zero. No meaningful network-level loss. The alarming number had been entirely a measurement artifact.

### Isolating it directly: the Mode A/B test, finally run against the right stack

Every capacity- and network-level theory had failed. What hadn't been tried yet — an oversight, in hindsight, given it was the single most informative test in the *entire* Rocket investigation — was running the same connection-reuse experiment against Actix-web specifically. All the earlier Mode A/B testing had only ever been run against Rocket.

```bash
k6 run --no-connection-reuse -e RATE=800 -e DURATION=20s loadtest/script.js
```

```
http_req_failed: 0.00%  0 out of 16000
http_reqs:       16000  799.79/s
http_req_duration: p90=5.24ms  p95=6.37ms  p99=8.49ms  (max=29.74ms)
vus: max=3
```

Completely clean. Full target rate achieved, latency healthy across every percentile, and only **3** virtual users needed to sustain it — matching what Little's Law predicts given real per-request latency (versus the 600+ VUs the default (reused-connection) test needed to limp to a third of that throughput). This was the decisive result: the problem was specifically about *reusing* connections, isolated cleanly from admission, capacity, workers, and the database.

### Root cause: a known actix-web keep-alive race

Actix-web's `HttpServer` defaults to a 5-second keep-alive timeout. A long-standing, exactly-on-point GitHub issue — [actix/actix-web#1759](https://github.com/actix/actix-web/issues/1759), "Actix losing track of client connections" — describes the mechanism: a client sends a request on a connection it believes is still open, at almost the exact moment the server decides that connection has been idle long enough and tears it down. The request is silently lost in that race. The client has no way to know the server closed the connection at that instant, so it has to detect the failure and recover — a fresh connection, a retry — before the request eventually completes. That recovery path is where the ~22 seconds came from: not a network delay, a client-side failure-detection-and-retry cost, paid only by the unlucky fraction of requests that happened to race the keep-alive boundary.

### The fix

```rust
HttpServer::new(move || { /* ... */ })
    .keep_alive(std::time::Duration::from_secs(75))
    .bind(("0.0.0.0", 8080))?
    .run()
```

One line. Extends the keep-alive window well past anything the benchmark's connection-reuse pattern would need, eliminating the race without disabling keep-alive entirely.

### Result

Reproduced clean twice at rate=800 (p99=8.99ms, then p99=8.42ms on a repeat run — 0% errors both times, against a previously *consistent* ~22.5-second stall), and clean again at rate=1600 (p99=22.04ms, 0% errors). Both Rust stacks now have real, understood, permanently fixed root causes — two different bugs, in two different libraries, that happened to produce the same-looking number.

---

## Investigation #3: the query that had been there all along

### An overnight run that should have "just worked"

Months after the backlog and keep-alive fixes above, with both Rust stacks finally at parity, the project hit a data-integrity bug: the raw result files were append-only and accumulating data from every run ever made, silently blending old and new measurements together. That got fixed properly (every row tagged with a run ID, the report filtering to the freshest one, and a new step that zips a run's results into one dated archive). With a clean slate and the fix in place, the plan was simple: kick off the real, overnight, three-repeat, 15-minute-soak official benchmark — the actual dataset the talk would be built on — and check on it in the morning.

Checking on it in the morning revealed it wasn't running anymore. It had stopped on its own, about 7.5 hours in, having gotten through barely more than the first stage. **Every one of the six services had failed soak confirmation, at every memory size tested, all the way up to the largest one on the ladder.** That included Rocket and Actix-web — the same two services from Investigations #1 and #2, now thoroughly fixed and, until this exact night, completely reliable. Something had broken *everything*, uniformly, for the first time in the project's history.

### The first real clue: a suspiciously precise clock

Every failure had the same shape: canary request latency climbing smoothly over tens of seconds, memory climbing right alongside it, then two back-to-back timeouts and a declared death. Laid out against the timeline of when each stack's soak attempt had actually started, one pattern jumped out immediately: **every single death, across all six services and every memory size, regardless of what time of night that specific attempt happened to start, landed in the same narrow band — roughly 600 to 860 seconds after the soak began.** Ten to fourteen minutes in, every time, independent of wall-clock time.

That's a very specific kind of clue. A cause tied to a *time of day* (a cron job, a scheduled backup, a maintenance window) would scatter unpredictably across attempts that started at wildly different hours. A cause tied to *elapsed time since the test itself began* wouldn't — and that's what this was. It also explained, immediately, why nothing like this had ever shown up before: every previous soak test in this project's history had run for 10 minutes or less. Tonight's tests ran 15. The bug had probably always been reachable — nothing had ever run long enough to reach it.

### Nine reproductions, and a long list of things it wasn't

Reproducing it was straightforward and unpleasant: pick the simplest case (Rocket, at the largest memory size, the one with the most headroom), run the exact same 15-minute soak in isolation, and watch. It failed on the first attempt, at 811 seconds. It failed on the second, at 841. The third, 839. It kept failing, in that same ten-to-fourteen-minute window, nine times out of nine — a reproduction rate good enough to build real, layered, live monitoring around, adding one more angle of measurement each time and letting the next run either confirm or kill the current best guess.

Nine live reproductions later, in roughly this order, all of the following had been measured directly and ruled out — not reasoned away, actually checked with numbers:

- **Postgres's checkpoint schedule.** Checkpoints fire on a strict 30-minute wall-clock cadence — confirmed straight from the container's own logs. Writing a small script to line every death up against every checkpoint's start/end window showed most deaths had no checkpoint activity anywhere near them at all. If anything, this made the "elapsed time, not wall clock" clue *more* convincing, not less — a wall-clock-driven cause should have shown *some* alignment with a fixed 30-minute grid, and it didn't.
- **Autovacuum.** A plausible next guess, given the workload is nothing but inserts into a table that starts at 100,000 rows and roughly quadruples by the time of death — a textbook trigger for Postgres's insert-driven autovacuum. `pg_stat_activity`, polled every 8 seconds for the full 14-minute window, showed **zero** active autovacuum workers at any point, including right through the death itself.
- **Postgres's own connection count and query latency.** Flat at 21 connections the entire time; the longest actively-running query never exceeded about 90 milliseconds — even as the client's own perceived response time climbed past two full seconds. Postgres was never slow to *answer* anything.
- **Rocket's own socket and file-descriptor counts, inside the container.** Flat, unmoving, for the entire run, right up until the moment it died.
- **Load-generator ephemeral port exhaustion.** If anything, TIME_WAIT sockets were *lower* near the death than at the start of the test — the opposite of what a port-exhaustion story would predict.
- **CPU throttling**, checked directly via the container's own `cpu.stat` (`nr_throttled`, `throttled_usec`): zero throttling events, the entire run. Rocket's own CPU usage never broke 8% of its one allocated core.
- **The target host's connection-tracking table** (`nf_conntrack`): 106 entries out of a 262,144 ceiling. Nowhere close.
- **The load generator's own CPU and memory**, and the load-testing process's own resource use: essentially idle the whole time — load average under 0.5 on a four-core machine, all the way through the failure window.
- **Disk I/O on the database's own host**, sampled every 5 seconds: write latency under a millisecond and disk utilization under a third, for the entire run, including the exact moment everything else fell apart. Writes only stopped because the app had already died — not the other way around.
- **Real network packet loss**, on *both* legs of the path (load-generator to application, and separately application to database) — full packet captures, each analyzed for actual retransmissions, zero-window events, and duplicate acknowledgements. Zero matches, on either leg, anywhere in either capture.
- **A blocking call buried somewhere in the application's own async code** (the kind of bug that can freeze an entire Tokio worker thread) — read through the whole handler; nothing there.

Ten plausible mechanisms, each one checked with a real measurement rather than argued from a hunch, and every single one came back clean.

### The signal that had been available the entire time

What finally broke it open wasn't a new tool — it was pointing an already-used one (a plain `top`/`load average` sample, taken every 8 seconds) at a machine nobody had suspected yet: the database's own host, specifically its aggregate load, rather than any of the per-query or per-connection numbers already checked.

It climbed. Not spiked, not fluctuated — climbed, smoothly and continuously, from 0.6 at the very start of a 15-minute run to over 16 right before the death, on a machine with exactly four real CPU cores. It crossed the four-core saturation line around the eight-to-nine-minute mark and kept climbing from there. Every per-query and per-connection number checked earlier had stayed healthy precisely *because* none of them measured aggregate, cumulative demand across the whole host — a load average is the one number that does, and it was the one number nobody had looked at yet.

### Root cause: a pagination total that everyone happened to write the same way

The application's list endpoint returns items *and* a `total` count, for pagination — completely standard API design, and part of this benchmark's own shared specification, independently implemented six times. Every one of those six implementations, in six different languages, computed that total the same obvious way:

```sql
SELECT COUNT(*) FROM items
```

Postgres has no fast path for this. Because of MVCC, an *exact* row count can only come from actually visiting rows — its cost is proportional to how many rows exist, not a constant. Under this benchmark's own workload — continuous inserts, no deletes, every single test starting from a 100,000-row baseline and growing to 300,000 or more *during that one test* — that per-request query gets measurably more expensive as the test itself runs. At 20% of the traffic mix hitting this endpoint, and the shared load fixed at 1200 requests per second, that's 240 of these count queries landing on the database every second, against a table that keeps getting bigger for as long as the test continues. The database's total CPU demand from this alone grows for the whole life of the test, and it crosses what four cores can sustain at almost exactly the same *relative* point every time — because it's driven by how many rows have accumulated since the last reseed, not by the time of day. That's the entire explanation for the tight, wall-clock-independent clustering that was the very first clue, and for why extending the soak from 10 minutes to 15 was exactly enough to walk into it for the first time.

### The fix, and a gotcha almost missed

The fix, applied identically to all six services: swap the exact count for Postgres's own planner estimate —

```sql
SELECT reltuples::bigint FROM pg_class WHERE oid = 'items'::regclass
```

— a number Postgres already keeps in its table metadata, costing nothing to read regardless of table size, in exchange for being approximate rather than exact. Entirely acceptable for a pagination total; nobody needs "exactly 300,417 items," they need "about 300,000."

One thing almost slipped through: this benchmark reseeds its dataset before every test by truncating the table and reinserting from scratch, and `TRUNCATE` doesn't just reset that row-count estimate to zero — checked directly, it sets it to **-1**, Postgres's own sentinel value for "this table has never been analyzed." Every test would have reported a `total` of negative one, for its entire duration, if this had shipped without also adding one `ANALYZE items;` right after the reseed's insert. That one-line addition gives the estimate a real, correct starting point (verified: exactly 100,000, immediately after a reseed) every time, and it's fine for the estimate to drift slightly stale as the table grows over the following fifteen minutes — that's the entire, deliberate trade this fix makes.

Verified locally against all six services before touching any real infrastructure — the same standing discipline this project has followed since Investigation #1's very first fix.

---

## What this was actually like to debug

A few things worth carrying into how this gets told, if it becomes a talk or an article:

**The identical symptom was a trap.** Both stacks capping at exactly 400 req/s looked, very strongly, like one shared explanation. It wasn't — and the giveaway was that fixing one (confirmed via the same diagnostic technique, `nsenter` into the container's network namespace) had *zero* effect on the other. When a fix that should generalize doesn't move the second data point at all, that's not evidence the fix is wrong — it's evidence you're looking at two different bugs.

**Every hypothesis got tested, not argued.** Pool size, worker count, ramping vs. abrupt load, DB pool size *again* (for a different stack, since the first "pool size doesn't matter" conclusion didn't automatically transfer), pre-warming connections, packet-level retransmission — each of these was a genuinely plausible mechanism, and each one got a real experiment with a clear pass/fail prediction before being discarded. Several of the "obviously it's this" theories (admission burst, worker starvation) turned out backwards: forcing *more* connection churn made Rocket's problem disappear; adding *more* workers made Actix-web's problem worse.

**The same measurement mistake happened twice.** Capturing packets on every interface (`-i any`) instead of the one that actually matters produced a misleading result *twice*, once in each investigation — first as a merely-confusing `ss` reading (fixed by checking inside the container's namespace via `nsenter`), then as a genuinely alarming, nearly-conclusive-looking 31,911-retransmission count that was actually a duplicate-capture artifact. The lesson generalizes past this specific tool: in a Docker/virtualized-network environment, always ask *which* interface you're actually looking at before trusting what you see on it.

**A closed GitHub issue is not the same as observed behavior.** The Rust backlog issue being marked "resolved" was worth checking, not trusting — and checking it directly (a from-scratch, isolated, minimally-reproducing test program on the exact toolchain in use) took ten minutes and produced a different answer than the issue thread implied. This is a generally reusable habit: when a fix is claimed upstream, verify it in your own environment before building on top of the claim.

**Both real fixes were tiny.** A ~20-line `LD_PRELOAD` shim, and a single `.keep_alive()` call. The diagnostic distance between "these are both stuck at 400 req/s" and "here are two one-line fixes" was two multi-hour investigations involving packet captures, network namespace inspection, live database monitoring, and reading Rust's standard library source and an HTTP framework's issue tracker. That gap — small fix, large investigation — is normal for this class of bug, and worth saying out loud rather than presenting the final diff as if it were obvious from the start.

**A parameter change can be a discovery tool, not just a test setting.** Investigation #3 only happened because the soak duration was extended from 10 minutes to 15 — a change made for better statistical confidence, with no expectation it would reveal a bug. It's worth treating "what happens if I let this run longer / push this harder / scale this bigger than my normal test does" as a real diagnostic technique in its own right, not just a robustness nice-to-have — some bugs are only reachable past a threshold nobody had a reason to cross before.

**The identical symptom across all six services, this time, meant exactly what it looked like.** Investigations #1 and #2 warned against assuming a shared symptom implies a shared cause. Investigation #3 is the other side of that same coin: when six *independently written* services in six different languages fail in the exact same way, at the exact same relative time, the far more likely explanation is a cause they all genuinely share — in this case, a shared specification that six people (or six sessions of one) happened to implement identically. Telling these two situations apart matters, and the tell is in the *mechanism*, not just the symptom: two Rust services sharing a symptom for unrelated reasons still had to be independently verified with a fix that moved one number without moving the other; six services sharing both a symptom and its exact timing pointed at something upstream of all of them.

**The most useful measurement was the most obvious one, aimed somewhere new.** A plain load-average sample was never a sophisticated tool — the reason it took nine reproductions to reach for it was that it got pointed at the database's host only after a long list of more specific, more targeted measurements (query duration, connection state, disk I/O, packet captures) had already come back clean. In hindsight, aggregate host-level load is worth checking early, specifically *because* it's the one number that reflects cumulative, system-wide demand rather than any single request, connection, or query in isolation — the kind of thing that stays invisible to measurements scoped too narrowly to see it.
