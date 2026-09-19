-- MySQL 8.4 + sqlx: JSON columns do not decode as Rust String.
-- Convert to LONGTEXT for out-of-the-box String bind/decode (idempotent ALTERs).
ALTER TABLE nodes MODIFY COLUMN labels_json LONGTEXT NOT NULL;
ALTER TABLE node_configs MODIFY COLUMN specs_json LONGTEXT NOT NULL;
ALTER TABLE node_configs MODIFY COLUMN schedules_json LONGTEXT NOT NULL;
ALTER TABLE results MODIFY COLUMN payload_json LONGTEXT NOT NULL;
ALTER TABLE pending_dispatches MODIFY COLUMN params_json LONGTEXT NOT NULL;
