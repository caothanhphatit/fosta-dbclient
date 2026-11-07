-- Dummy shop data for MariaDB 11.8. Generated with the SEQUENCE engine so the
-- file stays small; values are deterministic so screenshots stay stable.

CREATE DATABASE IF NOT EXISTS shop CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci;
USE shop;

CREATE TABLE categories (
  id INT PRIMARY KEY,
  name VARCHAR(60) NOT NULL,
  slug VARCHAR(60) NOT NULL UNIQUE
) COMMENT 'Product categories';

INSERT INTO categories (id, name, slug) VALUES
  (1, 'Desks', 'desks'), (2, 'Chairs', 'chairs'), (3, 'Lighting', 'lighting'),
  (4, 'Sofas', 'sofas'), (5, 'Storage', 'storage'), (6, 'Rugs', 'rugs'),
  (7, 'Decor', 'decor'), (8, 'Outdoor', 'outdoor');

CREATE TABLE customers (
  id INT AUTO_INCREMENT PRIMARY KEY,
  full_name VARCHAR(120) NOT NULL,
  email VARCHAR(160) NOT NULL UNIQUE,
  city VARCHAR(60) NOT NULL,
  tier ENUM('bronze', 'silver', 'gold') NOT NULL DEFAULT 'bronze',
  created_at DATETIME NOT NULL
);

INSERT INTO customers (full_name, email, city, tier, created_at)
SELECT
  CONCAT(ELT(1 + seq % 10, 'An', 'Binh', 'Chi', 'Dung', 'Giang', 'Hoa', 'Khanh', 'Linh', 'Minh', 'Nam'), ' ',
         ELT(1 + (seq DIV 10) % 8, 'Nguyen', 'Tran', 'Le', 'Pham', 'Hoang', 'Vo', 'Dang', 'Bui')),
  CONCAT('customer', seq, '@example.com'),
  ELT(1 + seq % 6, 'Ha Noi', 'Ho Chi Minh', 'Da Nang', 'Can Tho', 'Hai Phong', 'Hue'),
  ELT(1 + (seq * 7) % 10 DIV 4, 'bronze', 'silver', 'gold'),
  TIMESTAMP('2025-01-01') + INTERVAL (seq * 37) % 650 DAY + INTERVAL (seq * 13) % 86400 SECOND
FROM seq_1_to_2000;

CREATE TABLE products (
  id INT AUTO_INCREMENT PRIMARY KEY,
  sku VARCHAR(32) NOT NULL UNIQUE,
  name VARCHAR(120) NOT NULL,
  category_id INT NOT NULL,
  price DECIMAL(10, 2) NOT NULL,
  stock INT NULL,
  updated_at DATETIME NOT NULL,
  embedding VECTOR(4) NOT NULL,
  CONSTRAINT fk_products_category FOREIGN KEY (category_id) REFERENCES categories (id),
  VECTOR INDEX idx_embedding (embedding) M=6 DISTANCE=cosine
);

INSERT INTO products (sku, name, category_id, price, stock, updated_at, embedding)
SELECT
  CONCAT('SKU-', LPAD(seq, 5, '0')),
  CONCAT(ELT(1 + seq % 8, 'Walnut', 'Oak', 'Steel', 'Linen', 'Glass', 'Brass', 'Wool', 'Pine'), ' ',
         ELT(1 + (seq DIV 8) % 8, 'desk', 'chair', 'lamp', 'sofa', 'shelf', 'rug', 'vase', 'bench'), ' ', seq),
  1 + (seq DIV 8) % 8,
  ROUND(19.5 + (seq % 37) * 4.25 + (seq % 5) * 0.99, 2),
  IF(seq % 9 = 0, NULL, (seq * 7) % 120),
  TIMESTAMP('2026-10-01') + INTERVAL (seq * 17) % 40 DAY + INTERVAL seq % 3600 SECOND,
  VEC_FromText(CONCAT('[', ((seq % 10) / 10), ',', (((seq * 3) % 10) / 10), ',', (((seq * 7) % 10) / 10), ',', (((seq * 9) % 10) / 10 + 0.05), ']'))
FROM seq_1_to_1240;

CREATE TABLE orders (
  id INT AUTO_INCREMENT PRIMARY KEY,
  customer_id INT NOT NULL,
  status ENUM('pending', 'paid', 'shipped', 'cancelled') NOT NULL,
  ordered_at DATETIME NOT NULL,
  total DECIMAL(12, 2) NOT NULL DEFAULT 0,
  CONSTRAINT fk_orders_customer FOREIGN KEY (customer_id) REFERENCES customers (id),
  KEY idx_orders_status (status, ordered_at)
);

INSERT INTO orders (customer_id, status, ordered_at)
SELECT
  1 + (seq * 31) % 2000,
  ELT(1 + (seq * 11) % 20 DIV 5, 'pending', 'paid', 'shipped', 'cancelled'),
  TIMESTAMP('2025-06-01') + INTERVAL (seq * 53) % 520 DAY + INTERVAL (seq * 97) % 86400 SECOND
FROM seq_1_to_8000;

CREATE TABLE order_items (
  id INT AUTO_INCREMENT PRIMARY KEY,
  order_id INT NOT NULL,
  product_id INT NOT NULL,
  quantity INT NOT NULL,
  unit_price DECIMAL(10, 2) NOT NULL,
  CONSTRAINT fk_items_order FOREIGN KEY (order_id) REFERENCES orders (id),
  CONSTRAINT fk_items_product FOREIGN KEY (product_id) REFERENCES products (id)
);

INSERT INTO order_items (order_id, product_id, quantity, unit_price)
SELECT o.id, p.id, 1 + (o.id + line.seq) % 4, p.price
FROM orders o
JOIN seq_0_to_2 line ON line.seq < 1 + o.id % 3
JOIN products p ON p.id = 1 + (o.id * 13 + line.seq * 101) % 1240;

UPDATE orders o
JOIN (SELECT order_id, SUM(quantity * unit_price) AS total FROM order_items GROUP BY order_id) t ON t.order_id = o.id
SET o.total = t.total;

CREATE VIEW v_daily_sales AS
SELECT DATE(ordered_at) AS day, COUNT(*) AS orders, SUM(total) AS revenue
FROM orders
WHERE status IN ('paid', 'shipped')
GROUP BY DATE(ordered_at);
