#define _GNU_SOURCE
#include <dlfcn.h>
#include <sys/socket.h>
#include <stdio.h>
#include <stdlib.h>

/* Intercepts listen(2) and forces a minimum backlog, overriding whatever
 * the calling application requests. Exists because Rust's std::net::
 * TcpListener::bind() (used internally by Hyper 0.14 / Rocket 0.5.1)
 * hardcodes backlog=128 on Linux, confirmed empirically not to respect
 * the OS's higher somaxconn — and Rocket 0.5.1's public API has no hook
 * to inject a custom pre-bound listener (that lands in a later major
 * version). This works at the libc level instead, requiring zero changes
 * to the Rust service itself. See CLAUDE.md progress log, Aug 17. */
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
