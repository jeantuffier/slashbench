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
