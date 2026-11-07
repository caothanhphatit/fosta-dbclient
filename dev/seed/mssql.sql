-- Dummy ERP data for SQL Server. Run by dev/seed-mssql.sh once the server is up.
IF DB_ID('ERP') IS NULL CREATE DATABASE ERP;
GO
USE ERP;
GO
IF SCHEMA_ID('sales') IS NULL EXEC('CREATE SCHEMA sales');
GO

CREATE TABLE dbo.Warehouses (
  WarehouseId INT PRIMARY KEY,
  Name NVARCHAR(60) NOT NULL,
  City NVARCHAR(60) NOT NULL
);
INSERT INTO dbo.Warehouses VALUES (1, N'North Depot', N'Hanoi'), (2, N'South Depot', N'Ho Chi Minh City'), (3, N'Delta Depot', N'Can Tho');

CREATE TABLE dbo.Items (
  ItemId INT IDENTITY(1, 1) PRIMARY KEY,
  Code VARCHAR(20) NOT NULL UNIQUE,
  Name NVARCHAR(120) NOT NULL,
  Unit NVARCHAR(10) NOT NULL,
  UnitPrice DECIMAL(12, 2) NOT NULL,
  IsActive BIT NOT NULL DEFAULT 1,
  CreatedAt DATETIME2 NOT NULL
);

WITH n AS (SELECT TOP (600) ROW_NUMBER() OVER (ORDER BY (SELECT NULL)) AS i FROM sys.all_objects a CROSS JOIN sys.all_objects b)
INSERT INTO dbo.Items (Code, Name, Unit, UnitPrice, IsActive, CreatedAt)
SELECT
  CONCAT('IT', RIGHT(CONCAT('0000', i), 4)),
  CONCAT(CHOOSE(1 + i % 6, N'Steel plate', N'PVC pipe', N'Cement', N'Anti-rust paint', N'Bolt', N'Cable'), N' grade ', 1 + i % 9),
  CHOOSE(1 + i % 4, N'kg', N'm', N'bag', N'pcs'),
  CAST(12000 + (i * 7919) % 480000 AS DECIMAL(12, 2)),
  CASE WHEN i % 17 = 0 THEN 0 ELSE 1 END,
  DATEADD(MINUTE, i * 413, '2025-01-01')
FROM n;

CREATE TABLE dbo.Stock (
  WarehouseId INT NOT NULL REFERENCES dbo.Warehouses (WarehouseId),
  ItemId INT NOT NULL REFERENCES dbo.Items (ItemId),
  Quantity DECIMAL(12, 2) NOT NULL,
  UpdatedAt DATETIME2 NOT NULL,
  CONSTRAINT PK_Stock PRIMARY KEY (WarehouseId, ItemId)
);

INSERT INTO dbo.Stock (WarehouseId, ItemId, Quantity, UpdatedAt)
SELECT w.WarehouseId, i.ItemId, (i.ItemId * 37 + w.WarehouseId * 11) % 900, DATEADD(HOUR, -i.ItemId % 200, '2026-11-01')
FROM dbo.Warehouses w CROSS JOIN dbo.Items i
WHERE (i.ItemId + w.WarehouseId) % 3 <> 0;

CREATE TABLE sales.Customers (
  CustomerId INT IDENTITY(1, 1) PRIMARY KEY,
  Name NVARCHAR(120) NOT NULL,
  TaxCode VARCHAR(14) NULL,
  Province NVARCHAR(60) NOT NULL
);

WITH n AS (SELECT TOP (300) ROW_NUMBER() OVER (ORDER BY (SELECT NULL)) AS i FROM sys.all_objects)
INSERT INTO sales.Customers (Name, TaxCode, Province)
SELECT
  CONCAT(CHOOSE(1 + i % 7, N'Northwind', N'Bluewater', N'Silverline', N'Redwood', N'Greenfield', N'Brightstar', N'Ironbridge'), N' ', CHOOSE(1 + i % 5, N'Trading', N'Construction', N'Logistics', N'Supply', N'Services'), N' ', i),
  CASE WHEN i % 6 = 0 THEN NULL ELSE CONCAT('03', RIGHT(CONCAT('00000000', i * 7717), 8)) END,
  CHOOSE(1 + i % 5, N'Hanoi', N'Ho Chi Minh City', N'Da Nang', N'Can Tho', N'Hai Phong')
FROM n;

CREATE TABLE sales.Invoices (
  InvoiceId INT IDENTITY(1, 1) PRIMARY KEY,
  CustomerId INT NOT NULL REFERENCES sales.Customers (CustomerId),
  InvoiceNo VARCHAR(20) NOT NULL UNIQUE,
  IssuedAt DATETIME2 NOT NULL,
  Status VARCHAR(12) NOT NULL,
  Amount DECIMAL(14, 2) NOT NULL
);

WITH n AS (SELECT TOP (5000) ROW_NUMBER() OVER (ORDER BY (SELECT NULL)) AS i FROM sys.all_objects a CROSS JOIN sys.all_objects b)
INSERT INTO sales.Invoices (CustomerId, InvoiceNo, IssuedAt, Status, Amount)
SELECT
  1 + (i * 31) % 300,
  CONCAT('HD', 2025 + i % 2, '-', RIGHT(CONCAT('000000', i), 6)),
  DATEADD(MINUTE, i * 157, '2025-01-01'),
  CHOOSE(1 + i % 4, 'draft', 'issued', 'paid', 'void'),
  CAST(500000 + (i * 104729) % 95000000 AS DECIMAL(14, 2))
FROM n;
GO

CREATE VIEW sales.vMonthlyRevenue AS
SELECT FORMAT(IssuedAt, 'yyyy-MM') AS Month, COUNT(*) AS Invoices, SUM(Amount) AS Revenue
FROM sales.Invoices
WHERE Status IN ('issued', 'paid')
GROUP BY FORMAT(IssuedAt, 'yyyy-MM');
GO
