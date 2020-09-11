CREATE TABLE IF NOT EXISTS metrics (
    id integer NOT NULL primary key autoincrement,
    timestamp real NOT NULL,
    key text NOT NULL,
    value integer NOT NULL
);
CREATE INDEX IF NOT EXISTS metrics_timestamp_idx ON metrics (timestamp);
CREATE INDEX IF NOT EXISTS metrics_key_idx ON metrics (key);
