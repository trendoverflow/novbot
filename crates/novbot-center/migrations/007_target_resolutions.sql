-- Shared target snapshots (T1.4 / T2.3). Idempotent: Db::migrate re-runs every file.
-- Do not put a semicolon inside a statement. split_sql splits on semicolons.

CREATE TABLE IF NOT EXISTS target_resolutions (
  id CHAR(36) NOT NULL,
  kind VARCHAR(32) NOT NULL,
  ref_id VARCHAR(64) NOT NULL,
  selector VARCHAR(1024) NULL,
  picked_node_ids_json LONGTEXT NULL,
  excluded_node_ids_json LONGTEXT NOT NULL,
  node_ids_json LONGTEXT NOT NULL,
  node_count INT NOT NULL,
  prod_node_ids_json LONGTEXT NOT NULL,
  prod_confirmed TINYINT(1) NOT NULL DEFAULT 0,
  resolution_sha256 CHAR(64) NOT NULL,
  resolved_at DATETIME(3) NOT NULL,
  supersedes_id CHAR(36) NULL,
  actor VARCHAR(160) NOT NULL,
  PRIMARY KEY (id),
  INDEX idx_target_resolutions_ref (kind, ref_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE IF NOT EXISTS dispatch_operations (
  id CHAR(36) NOT NULL,
  spec_id VARCHAR(128) NOT NULL,
  params_json LONGTEXT NOT NULL,
  resolution_id CHAR(36) NOT NULL,
  actor VARCHAR(160) NOT NULL,
  created_at DATETIME(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
  PRIMARY KEY (id),
  CONSTRAINT fk_dispatch_operations_resolution FOREIGN KEY (resolution_id) REFERENCES target_resolutions (id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE IF NOT EXISTS dispatch_operation_runs (
  operation_id CHAR(36) NOT NULL,
  node_id VARCHAR(128) NOT NULL,
  run_id VARCHAR(64) NOT NULL,
  delivered VARCHAR(16) NOT NULL,
  error VARCHAR(255) NULL,
  PRIMARY KEY (operation_id, node_id),
  CONSTRAINT fk_dispatch_runs_operation FOREIGN KEY (operation_id) REFERENCES dispatch_operations (id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE IF NOT EXISTS target_schedules (
  id CHAR(36) NOT NULL,
  spec_id VARCHAR(128) NOT NULL,
  interval_secs BIGINT NULL,
  cron VARCHAR(128) NULL,
  enabled TINYINT(1) NOT NULL DEFAULT 1,
  resolution_id CHAR(36) NOT NULL,
  `follow` TINYINT(1) NOT NULL DEFAULT 0,
  created_by VARCHAR(160) NOT NULL,
  created_at DATETIME(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
  updated_at DATETIME(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3) ON UPDATE CURRENT_TIMESTAMP(3),
  PRIMARY KEY (id),
  CONSTRAINT fk_target_schedules_resolution FOREIGN KEY (resolution_id) REFERENCES target_resolutions (id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE IF NOT EXISTS target_schedule_nodes (
  schedule_id CHAR(36) NOT NULL,
  node_id VARCHAR(128) NOT NULL,
  state VARCHAR(32) NOT NULL,
  updated_at DATETIME(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3) ON UPDATE CURRENT_TIMESTAMP(3),
  PRIMARY KEY (schedule_id, node_id),
  CONSTRAINT fk_target_schedule_nodes_schedule FOREIGN KEY (schedule_id) REFERENCES target_schedules (id) ON DELETE CASCADE,
  CONSTRAINT fk_target_schedule_nodes_node FOREIGN KEY (node_id) REFERENCES nodes (node_id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
