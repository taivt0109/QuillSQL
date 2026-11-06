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
-- Users without CONNECT, with CONNECT denied, and with it to pass on.
CREATE USER NoConnect WITHOUT LOGIN;
REVOKE CONNECT FROM NoConnect;
CREATE USER Denied WITHOUT LOGIN;
DENY CONNECT TO Denied;
CREATE USER Granting WITHOUT LOGIN;
GRANT CONNECT TO Granting WITH GRANT OPTION;
GO
