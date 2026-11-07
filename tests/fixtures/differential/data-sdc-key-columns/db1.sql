CREATE TABLE dbo.T (Id int NOT NULL CONSTRAINT PK_T PRIMARY KEY, Code varchar(10) NOT NULL CONSTRAINT UQ_T_Code UNIQUE, Name varchar(20) NULL, Note varchar(20) NULL)
GO
INSERT dbo.T VALUES (1, 'A', 'x', 'n1'), (2, 'B', 'y', 'n2'), (3, 'C', 'z', 'n3')
