CREATE INDEX idx_orders_user ON orders (user_id, placed_at DESC);
CREATE UNIQUE INDEX IF NOT EXISTS idx_users_email_lower ON users (lower(email));
CREATE INDEX idx_orders_open ON orders (status) WHERE status <> 'shipped';
DROP INDEX IF EXISTS idx_old;
DROP TRIGGER IF EXISTS main.orders_old_trigger;
DROP VIEW v_gone;
DROP TABLE IF EXISTS temp_stuff;
