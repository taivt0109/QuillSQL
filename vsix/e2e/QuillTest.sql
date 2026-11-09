IF DB_ID('QuillTest') IS NOT NULL BEGIN ALTER DATABASE QuillTest SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE QuillTest; END
GO
CREATE DATABASE QuillTest;
GO
USE QuillTest;
GO
CREATE SCHEMA sales;
GO
CREATE TABLE dbo.Customers (CustomerId INT IDENTITY(1, 1) NOT NULL CONSTRAINT PK_Customers PRIMARY KEY, Name NVARCHAR(100) NULL, Created DATETIME2 NOT NULL CONSTRAINT DF_Customers_Created DEFAULT SYSDATETIME());
CREATE TABLE sales.Orders (OrderId INT NOT NULL CONSTRAINT PK_Orders PRIMARY KEY, CustomerId INT NOT NULL CONSTRAINT FK_Orders_Customers REFERENCES dbo.Customers (CustomerId), Total MONEY NOT NULL CONSTRAINT CK_Orders_Total CHECK (Total >= 0));
CREATE INDEX IX_Orders_CustomerId ON sales.Orders (CustomerId) INCLUDE (Total);
GO
CREATE PARTITION FUNCTION pfOrderYear (date) AS RANGE RIGHT FOR VALUES ('2024-01-01', '2025-01-01');
CREATE PARTITION SCHEME psOrderYear AS PARTITION pfOrderYear ALL TO ([PRIMARY]);
GO
CREATE TABLE sales.OrderArchive (OrderId INT NOT NULL, OrderDate DATE NOT NULL, Total MONEY NOT NULL, CONSTRAINT PK_OrderArchive PRIMARY KEY CLUSTERED (OrderId, OrderDate) ON psOrderYear (OrderDate)) ON psOrderYear (OrderDate);
GO
INSERT INTO dbo.Customers (Name) VALUES (N'Ann'), (N'Bob'), (N'Cid');
INSERT INTO sales.Orders (OrderId, CustomerId, Total) VALUES (1, 1, 10), (2, 1, 20), (3, 2, 30);
INSERT INTO sales.OrderArchive (OrderId, OrderDate, Total) VALUES (1, '2023-05-01', 1), (2, '2024-02-01', 2), (3, '2024-03-01', 3), (4, '2025-06-01', 4), (5, '2025-07-01', 5), (6, '2025-08-01', 6);
GO
CREATE PROCEDURE dbo.usp_GetOrders @CustomerId INT, @Max INT = 10
AS
BEGIN
    SELECT TOP (@Max) o.OrderId, o.Total FROM sales.Orders AS o WHERE o.CustomerId = @CustomerId;
END
GO
