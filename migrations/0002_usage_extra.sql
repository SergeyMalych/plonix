-- Schema 2 usage reports: active minutes per screen, search term kinds, the kind of work, the window look,
-- size ranges and well-known rejected domains, as one JSON object (see functions/api/usage.js and docs/privacy.md).
-- Run once after 0001_usage.sql against the D1 database bound as USAGE: see site/README.md.

ALTER TABLE usage_reports ADD COLUMN extra TEXT;  -- NULL for schema 1 reports
