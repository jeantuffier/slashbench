-- Idempotent truncate + reseed, run between benchmark runs to give every
-- stack an identical starting dataset (see CLAUDE.md "Fairness & environment rules").
TRUNCATE TABLE items RESTART IDENTITY;

INSERT INTO items (name, description, price_cents, quantity, created_at, updated_at)
SELECT
    'Item ' || i,
    'Seed description for item ' || i,
    (100 + (i % 9000)),
    (1 + (i % 500)),
    now() - (i || ' seconds')::interval,
    now() - (i || ' seconds')::interval
FROM generate_series(1, 100000) AS i;

-- TRUNCATE resets pg_stat/pg_class row-count estimates to "never analyzed"
-- (reltuples = -1), not just to 0 — confirmed directly against this exact
-- sequence. Every service now reads reltuples as an approximate pagination
-- total instead of COUNT(*) (see services/rocket/src/main.rs), so each
-- reseed needs one real ANALYZE to give that estimate a valid starting
-- point; it's fine for it to drift slightly stale over the run itself.
ANALYZE items;
