-- Core schema for the sample shop.
PRAGMA foreign_keys = ON;

CREATE TABLE users (
  id INTEGER PRIMARY KEY,
  email TEXT NOT NULL UNIQUE COLLATE NOCASE,
  display_name VARCHAR(80) DEFAULT 'anonymous',
  legacy_flag INTEGER NOT NULL DEFAULT 0 CHECK (legacy_flag IN (0, 1)),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CONSTRAINT users_email_len CHECK (length(email) > 3)
);

CREATE TABLE IF NOT EXISTS products (
  sku TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  price_cents INTEGER NOT NULL CHECK (price_cents >= 0),
  price_dollars REAL GENERATED ALWAYS AS (price_cents / 100.0) STORED,
  tags TEXT
) STRICT;

CREATE TABLE orders (
  id INTEGER PRIMARY KEY,
  user_id INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  placed_at TEXT NOT NULL DEFAULT (datetime('now')),
  status TEXT NOT NULL DEFAULT 'new',
  CHECK (status IN ('new', 'paid', 'shipped'))
);

CREATE TABLE order_items (
  order_id INTEGER NOT NULL,
  sku TEXT NOT NULL,
  quantity INTEGER NOT NULL DEFAULT 1,
  PRIMARY KEY (order_id, sku),
  FOREIGN KEY (order_id) REFERENCES orders (id),
  FOREIGN KEY (sku) REFERENCES products (sku) ON UPDATE CASCADE
) WITHOUT ROWID;

CREATE TABLE audit_log (
  order_id INTEGER,
  old_status TEXT,
  new_status TEXT,
  at TEXT DEFAULT CURRENT_TIMESTAMP
);
