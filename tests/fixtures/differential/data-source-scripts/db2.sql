CREATE TABLE dbo.T (Id int NOT NULL CONSTRAINT PK_T PRIMARY KEY, Name nvarchar(50) NULL, Amount decimal(10,2) NULL)
GO
INSERT dbo.T VALUES (1, N'same', 1.00), (2, N'old', 2.50), (4, N'only in 2', 4.00)
GO
CREATE TABLE dbo.I (Id int IDENTITY(1,1) NOT NULL CONSTRAINT PK_I PRIMARY KEY, Name nvarchar(20) NULL)
GO
INSERT dbo.I (Name) VALUES (N'a')
