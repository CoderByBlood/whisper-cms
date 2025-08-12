CREATE TABLE IF NOT EXISTS site (
  id         INTEGER PRIMARY KEY CHECK (id = 1),
  name       TEXT NOT NULL,
  base_url   TEXT NOT NULL,
  timezone   TEXT NOT NULL,
  created_at TEXT NOT NULL
);