-- Anonymous usage reports from Plonix installs (see functions/api/usage.js and docs/privacy.md).
-- Run once against the D1 database bound as USAGE: see site/README.md.

CREATE TABLE IF NOT EXISTS usage_reports (
  id          INTEGER PRIMARY KEY,
  day         TEXT    NOT NULL,  -- UTC date the report arrived, YYYY-MM-DD
  received_at INTEGER NOT NULL,  -- unix seconds
  install_id  TEXT    NOT NULL,  -- random, made by the install; not tied to a person or machine
  version     TEXT    NOT NULL,
  os          TEXT    NOT NULL,
  os_version  TEXT    NOT NULL,
  arch        TEXT    NOT NULL,
  counts      TEXT    NOT NULL,  -- JSON object: feature name -> times used since the last report
  UNIQUE (day, install_id)
);

CREATE INDEX IF NOT EXISTS usage_reports_day ON usage_reports (day);
