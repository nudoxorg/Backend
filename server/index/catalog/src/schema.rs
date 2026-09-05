//! Durable catalog table and index declarations.

pub(super) const INITIALIZE: &str = "CREATE TABLE IF NOT EXISTS catalog_history (
 sequence INTEGER PRIMARY KEY AUTOINCREMENT,
 ecosystem TEXT NOT NULL, package TEXT NOT NULL, version TEXT NOT NULL,
 generation BLOB NOT NULL, snapshot BLOB NOT NULL, publication BLOB NOT NULL,
 image BLOB NOT NULL, image_offset INTEGER NOT NULL, image_length INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS catalog_entities (
 sequence INTEGER NOT NULL REFERENCES catalog_history(sequence),
 ordinal INTEGER NOT NULL, family BLOB NOT NULL, variant BLOB NOT NULL,
 image BLOB NOT NULL, PRIMARY KEY(sequence, ordinal)
);
CREATE TABLE IF NOT EXISTS catalog_current (
 ecosystem TEXT NOT NULL, package TEXT NOT NULL, version TEXT NOT NULL,
 sequence INTEGER NOT NULL REFERENCES catalog_history(sequence),
 PRIMARY KEY(ecosystem, package, version)
);
CREATE TABLE IF NOT EXISTS catalog_checkpoint (
 singleton INTEGER PRIMARY KEY CHECK(singleton=1),
 sequence INTEGER NOT NULL, page INTEGER NOT NULL
);
INSERT OR IGNORE INTO catalog_checkpoint(singleton,sequence,page) VALUES (1,0,0);
CREATE INDEX IF NOT EXISTS catalog_lookup
 ON catalog_history(ecosystem, package, version, sequence);";
