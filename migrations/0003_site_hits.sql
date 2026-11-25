-- Website page counter (functions/api/hit.js). Hourly totals only: no ids, no IPs, no headers.
-- The function also creates this table on its first hit, so running this by hand is optional.
CREATE TABLE IF NOT EXISTS site_hits (
  day TEXT NOT NULL, hour INTEGER NOT NULL, kind TEXT NOT NULL, page TEXT NOT NULL, source TEXT NOT NULL, country TEXT NOT NULL,
  n INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (day, hour, kind, page, source, country)
);
