SELECT u.id, u.email, count(o.id) AS n_orders
FROM users AS u LEFT JOIN orders AS o ON o.user_id = u.id
WHERE u.legacy_flag = 0
GROUP BY u.id
HAVING n_orders > 1
ORDER BY n_orders DESC, u.email
LIMIT 10;

SELECT * FROM users WHERE id = ?1 AND email = :email;

INSERT INTO users (email, display_name) VALUES ('a@example.com', 'A') RETURNING id, created_at;

INSERT INTO products (sku, name, price_cents) VALUES ('x', 'X', 100)
ON CONFLICT (sku) DO UPDATE SET name = excluded.name, price_cents = excluded.price_cents
WHERE products.price_cents <> excluded.price_cents;

UPDATE orders SET status = 'paid'
WHERE user_id IN (SELECT id FROM users WHERE legacy_flag = 1)
RETURNING id, status;

UPDATE order_items SET quantity = quantity + 1 FROM products AS p
WHERE p.sku = order_items.sku AND p.price_cents > 0;

DELETE FROM orders
WHERE placed_at < date('now', '-1 year')
  AND NOT EXISTS (SELECT 1 FROM order_items AS oi WHERE oi.order_id = orders.id);

REPLACE INTO products (sku, name, price_cents)
SELECT sku, name, price_cents FROM products WHERE tags LIKE '%x%';

SELECT jv.value FROM users, json_each(users.display_name) AS jv;

SELECT (SELECT max(price_cents) FROM products WHERE products.sku = oi.sku) AS best
FROM order_items AS oi UNION SELECT 0 ORDER BY 1;
