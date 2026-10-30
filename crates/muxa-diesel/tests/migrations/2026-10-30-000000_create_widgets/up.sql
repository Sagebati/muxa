CREATE TABLE widgets (id BIGSERIAL PRIMARY KEY, name TEXT NOT NULL);
-- Hold the transaction open so concurrent runners overlap.
SELECT pg_sleep(0.3);
