-- A small database touching most object kinds the catalog reader models.
CREATE SCHEMA Sales AUTHORIZATION dbo;
GO
CREATE TYPE dbo.Phone FROM varchar(20) NOT NULL;
GO
CREATE TYPE dbo.IdList AS TABLE (Id int NOT NULL PRIMARY KEY, Note nvarchar(50) NULL CHECK (Note <> N''));
GO
CREATE SEQUENCE dbo.OrderNumbers AS bigint START WITH 1000 INCREMENT BY 1 CACHE 50;
GO
CREATE TABLE dbo.Customer (
    CustomerId int IDENTITY(1, 1) NOT NULL CONSTRAINT PK_Customer PRIMARY KEY,
    Name nvarchar(100) COLLATE Latin1_General_CI_AS NOT NULL,
    Phone dbo.Phone NULL,
    Email varchar(200) NULL,
    Balance decimal(18, 2) NOT NULL CONSTRAINT DF_Customer_Balance DEFAULT (0),
    CreatedAt datetime2(3) NOT NULL DEFAULT (SYSUTCDATETIME()),
    Notes nvarchar(max) NULL,
    NameLength AS (LEN(Name)) PERSISTED,
    CONSTRAINT UQ_Customer_Email UNIQUE (Email),
    CONSTRAINT CK_Customer_Balance CHECK (Balance >= 0)
);
GO
CREATE NONCLUSTERED INDEX IX_Customer_Name ON dbo.Customer (Name DESC) INCLUDE (Email) WHERE Email IS NOT NULL WITH (FILLFACTOR = 90);
GO
CREATE TABLE Sales.[Order] (
    OrderId bigint NOT NULL DEFAULT (NEXT VALUE FOR dbo.OrderNumbers),
    CustomerId int NOT NULL,
    Total money NOT NULL,
    Status char(1) NOT NULL CHECK (Status IN ('N', 'P', 'S')),
    CONSTRAINT PK_Order PRIMARY KEY CLUSTERED (OrderId),
    CONSTRAINT FK_Order_Customer FOREIGN KEY (CustomerId) REFERENCES dbo.Customer (CustomerId) ON DELETE CASCADE
) WITH (DATA_COMPRESSION = PAGE);
GO
CREATE TABLE Sales.OrderLine (
    OrderId bigint NOT NULL,
    LineNumber int NOT NULL,
    Qty int NOT NULL,
    CONSTRAINT PK_OrderLine PRIMARY KEY (OrderId, LineNumber),
    FOREIGN KEY (OrderId) REFERENCES Sales.[Order] (OrderId)
);
GO
ALTER TABLE Sales.OrderLine NOCHECK CONSTRAINT ALL;
GO
CREATE VIEW Sales.CustomerTotals WITH SCHEMABINDING AS
SELECT o.CustomerId, COUNT_BIG(*) AS Orders, SUM(o.Total) AS Total
FROM Sales.[Order] AS o GROUP BY o.CustomerId;
GO
CREATE UNIQUE CLUSTERED INDEX IX_CustomerTotals ON Sales.CustomerTotals (CustomerId);
GO
CREATE FUNCTION dbo.FullName (@id int) RETURNS nvarchar(100) AS
BEGIN
    RETURN (SELECT Name FROM dbo.Customer WHERE CustomerId = @id);
END;
GO
CREATE PROCEDURE Sales.GetOrders @CustomerId int
WITH EXECUTE AS OWNER
AS
SELECT * FROM Sales.[Order] WHERE CustomerId = @CustomerId;
GO
CREATE TRIGGER Sales.Order_Audit ON Sales.[Order] AFTER INSERT, UPDATE AS
BEGIN
    SET NOCOUNT ON;
END;
GO
CREATE SYNONYM dbo.Orders FOR Sales.[Order];
GO
EXEC sp_addextendedproperty N'MS_Description', N'People who buy things', N'SCHEMA', N'dbo', N'TABLE', N'Customer';
EXEC sp_addextendedproperty N'MS_Description', N'Display name', N'SCHEMA', N'dbo', N'TABLE', N'Customer', N'COLUMN', N'Name';
EXEC sp_addextendedproperty N'Owner', N'Sales team', N'SCHEMA', N'Sales', N'TABLE', N'Order', N'CONSTRAINT', N'PK_Order';
GO
