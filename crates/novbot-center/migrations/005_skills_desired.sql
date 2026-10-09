-- Skills Hub desired state (SH-2). Forward-only. Do not edit earlier migrations.
-- Built-in skills are not rows in `skills` and are not written into `node_skills_desired`.

CREATE TABLE IF NOT EXISTS skill_bundles (
  id VARCHAR(64) PRIMARY KEY,
  name VARCHAR(255) NOT NULL,
  description LONGTEXT NULL,
  builtin TINYINT(1) NOT NULL DEFAULT 0,
  created_at DATETIME(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE IF NOT EXISTS skill_bundle_items (
  bundle_id VARCHAR(64) NOT NULL,
  skill_name VARCHAR(63) NOT NULL,
  version_req VARCHAR(64) NOT NULL,
  PRIMARY KEY (bundle_id, skill_name),
  CONSTRAINT fk_skill_bundle_items_bundle FOREIGN KEY (bundle_id) REFERENCES skill_bundles (id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE IF NOT EXISTS node_skill_sets (
  node_id VARCHAR(128) PRIMARY KEY,
  generation BIGINT NOT NULL,
  updated_at DATETIME(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3) ON UPDATE CURRENT_TIMESTAMP(3),
  CONSTRAINT fk_node_skill_sets_node FOREIGN KEY (node_id) REFERENCES nodes (node_id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE IF NOT EXISTS node_skills_desired (
  node_id VARCHAR(128) NOT NULL,
  skill_name VARCHAR(63) NOT NULL,
  version VARCHAR(128) NOT NULL,
  sha256 CHAR(64) NOT NULL,
  operation_id VARCHAR(64) NULL,
  requested_by VARCHAR(255) NULL,
  requested_at DATETIME(3) NOT NULL,
  PRIMARY KEY (node_id, skill_name),
  CONSTRAINT fk_node_skills_desired_node FOREIGN KEY (node_id) REFERENCES nodes (node_id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE IF NOT EXISTS node_skills_actual (
  node_id VARCHAR(128) NOT NULL,
  skill_name VARCHAR(63) NOT NULL,
  version VARCHAR(128) NOT NULL,
  sha256 CHAR(64) NOT NULL,
  state VARCHAR(32) NOT NULL,
  reason VARCHAR(64) NULL,
  reason_detail LONGTEXT NULL,
  attempts INT NOT NULL DEFAULT 0,
  previous_json LONGTEXT NULL,
  applied_generation BIGINT NOT NULL DEFAULT 0,
  reported_at DATETIME(3) NULL,
  PRIMARY KEY (node_id, skill_name),
  CONSTRAINT fk_node_skills_actual_node FOREIGN KEY (node_id) REFERENCES nodes (node_id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE IF NOT EXISTS skill_operations (
  id CHAR(36) PRIMARY KEY,
  `type` VARCHAR(32) NOT NULL,
  skill_name VARCHAR(63) NOT NULL,
  version VARCHAR(128) NULL,
  targets_json LONGTEXT NOT NULL,
  capabilities_sha256 CHAR(64) NULL,
  force_flag TINYINT(1) NOT NULL DEFAULT 0,
  actor VARCHAR(255) NOT NULL,
  created_at DATETIME(3) NOT NULL,
  INDEX idx_skill_operations_created (created_at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE IF NOT EXISTS audit_events (
  id BIGINT AUTO_INCREMENT PRIMARY KEY,
  `at` DATETIME(3) NOT NULL,
  actor_type VARCHAR(32) NOT NULL,
  actor_id VARCHAR(255) NOT NULL,
  action VARCHAR(64) NOT NULL,
  target_type VARCHAR(32) NOT NULL,
  target_id VARCHAR(255) NOT NULL,
  node_id VARCHAR(128) NULL,
  detail_json LONGTEXT NOT NULL,
  prev_hash CHAR(64) NULL,
  `hash` CHAR(64) NOT NULL,
  INDEX idx_audit_events_at (`at`),
  INDEX idx_audit_events_action (action),
  INDEX idx_audit_events_node (node_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

INSERT IGNORE INTO skill_bundles (id, name, description, builtin)
VALUES
  ('host-basics', 'Host basics', 'host_info + echo', 1),
  ('env-sample', 'Env sample', 'env_get only', 1);

INSERT IGNORE INTO skill_bundle_items (bundle_id, skill_name, version_req)
VALUES
  ('host-basics', 'host_info', '*'),
  ('host-basics', 'echo', '*'),
  ('env-sample', 'env_get', '*');
