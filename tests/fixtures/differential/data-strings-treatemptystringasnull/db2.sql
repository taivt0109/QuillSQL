CREATE TABLE dbo.S (Id int NOT NULL CONSTRAINT PK_S PRIMARY KEY, V varchar(20) NULL, N nvarchar(20) NULL)
GO
INSERT dbo.S VALUES (1, '', N'x'), (2, 'a', N'abc'), (3, 'b', N'q'), (4, NULL, N'')
