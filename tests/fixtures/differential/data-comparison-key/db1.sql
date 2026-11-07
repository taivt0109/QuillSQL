CREATE TABLE dbo.C (Id int NOT NULL CONSTRAINT PK_C PRIMARY KEY, Code varchar(10) NOT NULL, Name nvarchar(50) NULL)
GO
CREATE UNIQUE INDEX UX_C_Code ON dbo.C (Code)
GO
INSERT dbo.C VALUES (1, 'a', N'one'), (2, 'b', N'two')
