-- The objects of db1 without explicit owners, table options, typed or principal extended
-- properties, trigger settings, type bindings and grants, history index, public/guest grants
-- and fixed-role members.
CREATE USER Owner1 WITHOUT LOGIN;
GO
CREATE USER Reader WITHOUT LOGIN;
GO
CREATE ROLE Auditors;
GO
CREATE SCHEMA Sales;
GO
CREATE DEFAULT dbo.Zero AS 0;
GO
CREATE RULE dbo.Positive AS @value >= 0;
GO
CREATE TYPE dbo.Amount FROM int NULL;
GO
CREATE TYPE dbo.Phone FROM varchar(20) NULL;
GO
CREATE TYPE dbo.Lines AS TABLE (Line int NOT NULL, Note nvarchar(40) NULL);
GO
CREATE TABLE Sales.Doc (Id int NOT NULL CONSTRAINT PK_Doc PRIMARY KEY, Body nvarchar(max) NULL, Total dbo.Amount NULL);
GO
CREATE TRIGGER Sales.Doc_Ins ON Sales.Doc AFTER INSERT AS SELECT 1;
GO
CREATE VIEW Sales.DocView AS SELECT Id, Body FROM Sales.Doc;
GO
CREATE PROCEDURE Sales.GetDoc @Id int AS SELECT Body FROM Sales.Doc WHERE Id = @Id;
GO
CREATE FUNCTION Sales.Twice (@x int) RETURNS int AS BEGIN RETURN @x * 2; END;
GO
CREATE SYNONYM Sales.Docs FOR Sales.Doc;
GO
EXEC sp_addextendedproperty N'Ver', N'3', 'SCHEMA', N'Sales', 'TABLE', N'Doc';
