CREATE VIEW order_totals AS
WITH item_totals AS (
  SELECT oi.order_id AS order_id, SUM(oi.quantity * p.price_cents) AS cents
  FROM order_items AS oi
  JOIN products p ON p.sku = oi.sku
  GROUP BY oi.order_id
)
SELECT o.id, u.email, t.cents
FROM orders o
JOIN users u ON u.id = o.user_id
LEFT JOIN item_totals t ON t.order_id = o.id
WHERE o.status <> 'new';

CREATE VIEW IF NOT EXISTS user_emails (uid, address) AS SELECT id, email FROM users;
