-- A database DDL trigger and bound defaults and rules, for writing scripts folders.
CREATE DEFAULT dbo.Zero AS 0;
GO
CREATE RULE dbo.Positive AS @value >= 0;
GO
CREATE TABLE dbo.Account (Id int NOT NULL PRIMARY KEY, Balance int NULL);
GO
EXEC sp_bindefault N'dbo.Zero', N'dbo.Account.Balance';
EXEC sp_bindrule N'dbo.Positive', N'dbo.Account.Balance';
GO
CREATE TRIGGER LogDdl ON DATABASE FOR CREATE_TABLE AS
BEGIN
    SET NOCOUNT ON;
END;
GO
DISABLE TRIGGER LogDdl ON DATABASE;
GO
