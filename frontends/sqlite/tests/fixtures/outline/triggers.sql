CREATE TRIGGER orders_touch AFTER UPDATE OF status ON orders
FOR EACH ROW WHEN NEW.status <> OLD.status
BEGIN
  INSERT INTO audit_log (order_id, old_status, new_status) VALUES (OLD.id, OLD.status, NEW.status);
  UPDATE orders SET placed_at = datetime('now') WHERE id = NEW.id AND placed_at IS NULL;
END;

CREATE TRIGGER IF NOT EXISTS orders_cleanup BEFORE DELETE ON orders
BEGIN
  DELETE FROM order_items WHERE order_id = OLD.id;
END;
