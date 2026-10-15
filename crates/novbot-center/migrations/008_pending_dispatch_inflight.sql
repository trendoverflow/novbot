-- At-least-once queued dispatch. NULL in_flight_at means available.
-- Db::migrate applies these ALTERs only when in_flight_at is missing
-- (startup re-runs every file, and a second ALTER would fail).
-- Two statements: MySQL rejects ADD COLUMN ... ADD INDEX in one prepared ALTER.
ALTER TABLE pending_dispatches
  ADD COLUMN in_flight_at DATETIME(3) NULL,
  ADD COLUMN attempt_count INT NOT NULL DEFAULT 0;

ALTER TABLE pending_dispatches
  ADD INDEX idx_pending_dispatch_inflight (node_id, in_flight_at);
