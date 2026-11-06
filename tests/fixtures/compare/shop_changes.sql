-- Applied on top of shop.sql to make the "target" of the comparison tests.
DROP INDEX IX_Customer_Name ON dbo.Customer;
GO
ALTER TABLE dbo.Customer ALTER COLUMN Email varchar(250) NULL;
GO
ALTER TABLE dbo.Customer ADD Country nchar(2) NULL;
GO
CREATE NONCLUSTERED INDEX IX_Customer_Name ON dbo.Customer (Name DESC) INCLUDE (Email, Phone) WHERE Email IS NOT NULL WITH (FILLFACTOR = 80);
GO
ALTER TABLE dbo.Customer DROP CONSTRAINT CK_Customer_Balance;
GO
ALTER TABLE dbo.Customer ADD CONSTRAINT CK_Customer_Balance CHECK (Balance >= -100);
GO
-- Same view, laid out differently: not a difference under IgnoreWhiteSpace.
DROP INDEX IX_CustomerTotals ON Sales.CustomerTotals;
DROP VIEW Sales.CustomerTotals;
GO
CREATE VIEW Sales.CustomerTotals
WITH SCHEMABINDING
AS
SELECT   o.CustomerId,
         COUNT_BIG(*) AS Orders,
         SUM(o.Total) AS Total
FROM     Sales.[Order] AS o
GROUP BY o.CustomerId;
GO
CREATE UNIQUE CLUSTERED INDEX IX_CustomerTotals ON Sales.CustomerTotals (CustomerId);
GO
ALTER PROCEDURE Sales.GetOrders @CustomerId int
WITH EXECUTE AS OWNER
AS
SELECT OrderId, Total FROM Sales.[Order] WHERE CustomerId = @CustomerId;
GO
CREATE TABLE dbo.Product (ProductId int NOT NULL PRIMARY KEY, Name nvarchar(50) NOT NULL);
GO
DROP SYNONYM dbo.Orders;
GO
EXEC sp_updateextendedproperty N'MS_Description', N'Customers', N'SCHEMA', N'dbo', N'TABLE', N'Customer';
GO
EXEC sp_rename N'Sales.OrderLine.Qty', N'Quantity', N'COLUMN';
GO
GRANT SELECT ON dbo.Customer TO app;
REVOKE EXECUTE ON Sales.GetOrders FROM app;
CREATE ROLE Auditors;
ALTER ROLE db_datareader DROP MEMBER app;
ALTER USER app WITH DEFAULT_SCHEMA = dbo;
GO
