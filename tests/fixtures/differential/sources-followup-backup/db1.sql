-- The model fields added after the sources package: explicit owners, table options, typed
-- extended property values, trigger settings, type bindings and permissions, history tables,
-- principal extended properties, public/guest permissions and fixed-role members. Without the
-- sequence, security policy, temporal table and table-type column property: SQL Compare does
-- not read them from backups.
CREATE USER Owner1 WITHOUT LOGIN;
GO
CREATE USER Reader WITHOUT LOGIN;
GO
CREATE ROLE Auditors;
GO
CREATE SCHEMA Sales;
GO
ALTER ROLE db_datareader ADD MEMBER Reader;
GO
GRANT VIEW DEFINITION TO public;
GO
GRANT CONNECT TO guest;
GO
CREATE DEFAULT dbo.Zero AS 0;
GO
CREATE RULE dbo.Positive AS @value >= 0;
GO
CREATE TYPE dbo.Amount FROM int NULL;
GO
EXEC sp_bindefault N'dbo.Zero', N'dbo.Amount';
GO
EXEC sp_bindrule N'dbo.Positive', N'dbo.Amount';
GO
CREATE TYPE dbo.Phone FROM varchar(20) NULL;
GO
GRANT REFERENCES ON TYPE::dbo.Phone TO Auditors;
GO
CREATE TYPE dbo.Lines AS TABLE (Line int NOT NULL, Note nvarchar(40) NULL);
GO
ALTER AUTHORIZATION ON TYPE::dbo.Lines TO Owner1;
GO
CREATE TABLE Sales.Doc (Id int NOT NULL CONSTRAINT PK_Doc PRIMARY KEY, Body nvarchar(max) NULL, Total dbo.Amount NULL);
GO
EXEC sp_tableoption N'Sales.Doc', 'large value types out of row', 1;
GO
ALTER AUTHORIZATION ON OBJECT::Sales.Doc TO Owner1;
GO
SET ANSI_NULLS OFF;
GO
SET QUOTED_IDENTIFIER OFF;
GO
CREATE TRIGGER Sales.Doc_Ins ON Sales.Doc AFTER INSERT AS SELECT 1;
GO
SET ANSI_NULLS ON;
GO
SET QUOTED_IDENTIFIER ON;
GO
CREATE VIEW Sales.DocView AS SELECT Id, Body FROM Sales.Doc;
GO
ALTER AUTHORIZATION ON OBJECT::Sales.DocView TO Owner1;
GO
CREATE PROCEDURE Sales.GetDoc @Id int AS SELECT Body FROM Sales.Doc WHERE Id = @Id;
GO
ALTER AUTHORIZATION ON OBJECT::Sales.GetDoc TO Owner1;
GO
CREATE FUNCTION Sales.Twice (@x int) RETURNS int AS BEGIN RETURN @x * 2; END;
GO
ALTER AUTHORIZATION ON OBJECT::Sales.Twice TO Owner1;
GO
CREATE SYNONYM Sales.Docs FOR Sales.Doc;
GO
ALTER AUTHORIZATION ON OBJECT::Sales.Docs TO Owner1;
GO
EXEC sp_addextendedproperty N'Ver', 3, 'SCHEMA', N'Sales', 'TABLE', N'Doc';
GO
EXEC sp_addextendedproperty N'Stamp', 0x0A0B, 'SCHEMA', N'Sales', 'TABLE', N'Doc', 'COLUMN', N'Body';
GO
EXEC sp_addextendedproperty N'MS_Description', N'docs', 'SCHEMA', N'Sales', 'VIEW', N'DocView', 'COLUMN', N'Id';
GO
EXEC sp_addextendedproperty N'MS_Description', N'owner', 'USER', N'Owner1';
GO
EXEC sp_addextendedproperty N'MS_Description', N'audit', 'USER', N'Auditors';
GO
EXEC sp_addextendedproperty N'Rank', 2, 'SCHEMA', N'dbo', 'TYPE', N'Phone';
