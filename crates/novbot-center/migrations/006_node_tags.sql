-- Node tags (T1.4 / T1.5). Idempotent: Db::migrate re-runs every file.
-- Do not put a semicolon inside a statement. split_sql splits on semicolons.

CREATE TABLE IF NOT EXISTS node_tags (
  node_id        VARCHAR(128) NOT NULL,
  source         ENUM('system','node','center') NOT NULL,
  tag_key        VARCHAR(255) CHARACTER SET utf8mb4 COLLATE utf8mb4_bin NOT NULL,
  tag_value      VARCHAR(255) CHARACTER SET utf8mb4 COLLATE utf8mb4_bin NOT NULL,
  valid          TINYINT(1) NOT NULL DEFAULT 1,
  invalid_reason VARCHAR(32) NULL,
  updated_at     DATETIME(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3) ON UPDATE CURRENT_TIMESTAMP(3),
  updated_by     VARCHAR(160) NOT NULL,
  PRIMARY KEY (node_id, source, tag_key),
  INDEX idx_node_tags_kv (tag_key, tag_value),
  CONSTRAINT fk_node_tags_node FOREIGN KEY (node_id) REFERENCES nodes(node_id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE OR REPLACE VIEW node_tags_effective AS
SELECT t.node_id, t.tag_key, t.tag_value, t.source,
       nt.tag_value AS node_value,
       (nt.tag_value IS NOT NULL) AS overridden
FROM node_tags t
LEFT JOIN node_tags nt
  ON t.source = 'center' AND nt.node_id = t.node_id
 AND nt.tag_key = t.tag_key AND nt.source = 'node' AND nt.valid = 1
WHERE t.valid = 1
  AND NOT (t.source = 'node' AND EXISTS (
      SELECT 1 FROM node_tags c
      WHERE c.node_id = t.node_id AND c.tag_key = t.tag_key AND c.source = 'center'));
