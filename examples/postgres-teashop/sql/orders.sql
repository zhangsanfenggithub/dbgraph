SELECT o.id, c.email, p.status
FROM orders o
JOIN customers c ON c.id = o.customer_id
JOIN payments p ON p.order_id = o.id
WHERE o.status = 'paid';

UPDATE orders
SET status = 'cancelled'
WHERE id = $1;
