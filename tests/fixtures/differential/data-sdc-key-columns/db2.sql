CREATE TABLE dbo.T (Id int NOT NULL CONSTRAINT PK_T PRIMARY KEY, Code varchar(10) NOT NULL CONSTRAINT UQ_T_Code UNIQUE, Name varchar(20) NULL, Note varchar(20) NULL)
GO
INSERT dbo.T VALUES (10, 'A', 'x', 'other'), (20, 'B', 'changed', 'n2'), (30, 'D', 'w', 'n4')
