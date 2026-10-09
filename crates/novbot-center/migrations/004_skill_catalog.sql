-- Skills Hub catalog and package bytes (SH-1).
-- Built-in skills (host_info, echo, env_get) are not inserted here.
CREATE TABLE IF NOT EXISTS skills (
  id BIGINT AUTO_INCREMENT PRIMARY KEY,
  name VARCHAR(63) NOT NULL,
  display_name VARCHAR(255) NOT NULL,
  description LONGTEXT NULL,
  source VARCHAR(32) NOT NULL,
  publisher VARCHAR(255) NULL,
  tags_json LONGTEXT NULL,
  created_at DATETIME(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
  created_by VARCHAR(255) NULL,
  UNIQUE KEY uq_skills_name (name)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE IF NOT EXISTS skill_versions (
  id BIGINT AUTO_INCREMENT PRIMARY KEY,
  skill_id BIGINT NOT NULL,
  version VARCHAR(128) NOT NULL,
  semver_major INT NOT NULL,
  semver_minor INT NOT NULL,
  semver_patch INT NOT NULL,
  prerelease VARCHAR(128) NULL,
  sha256 CHAR(64) NOT NULL,
  size_bytes BIGINT NOT NULL,
  content_type VARCHAR(128) NOT NULL,
  abi VARCHAR(64) NOT NULL,
  platforms_json LONGTEXT NULL,
  min_node_version VARCHAR(64) NULL,
  manifest_toml LONGTEXT NOT NULL,
  manifest_json LONGTEXT NOT NULL,
  params_schema_json LONGTEXT NOT NULL,
  output_schema_json LONGTEXT NULL,
  capabilities_json LONGTEXT NOT NULL,
  capabilities_sha256 CHAR(64) NOT NULL,
  compliance_json LONGTEXT NULL,
  signature_status VARCHAR(32) NOT NULL DEFAULT 'none',
  status VARCHAR(32) NOT NULL DEFAULT 'published',
  uploaded_at DATETIME(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
  UNIQUE KEY uq_skill_versions_skill_version (skill_id, version),
  UNIQUE KEY uq_skill_versions_sha256 (sha256),
  INDEX idx_skill_versions_skill (skill_id),
  CONSTRAINT fk_skill_versions_skill FOREIGN KEY (skill_id) REFERENCES skills (id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE IF NOT EXISTS skill_artifacts (
  sha256 CHAR(64) NOT NULL,
  size_bytes BIGINT NOT NULL,
  data LONGBLOB NOT NULL,
  created_at DATETIME(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
  PRIMARY KEY (sha256)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
