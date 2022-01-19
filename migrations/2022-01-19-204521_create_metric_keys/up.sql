DROP TABLE metrics;
CREATE TABLE IF NOT EXISTS metrics (
                                       id integer NOT NULL primary key autoincrement,
                                       timestamp real NOT NULL,
                                       metric_key_id integer NOT NULL,
                                       value integer NOT NULL
);
CREATE INDEX IF NOT EXISTS metrics_timestamp_idx ON metrics (timestamp);
CREATE INDEX IF NOT EXISTS metrics_key_id_idx ON metrics (metric_key_id);

CREATE TABLE IF NOT EXISTS metric_keys (
                                       id integer NOT NULL primary key autoincrement,
                                       key text NOT NULL,
                                       unit text,
                                       description text
);
CREATE INDEX IF NOT EXISTS metrics_keys_key_idx ON metric_keys (key);
