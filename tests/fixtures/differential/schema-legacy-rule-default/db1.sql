CREATE RULE dbo.Positive AS @v > 0
GO
CREATE DEFAULT dbo.Zero AS 0
GO
CREATE TABLE dbo.T (Id int NOT NULL CONSTRAINT PK_T PRIMARY KEY, A int NULL)
GO
EXEC sp_bindrule 'dbo.Positive', 'dbo.T.A'
GO
EXEC sp_bindefault 'dbo.Zero', 'dbo.T.A'
