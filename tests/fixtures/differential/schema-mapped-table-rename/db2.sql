CREATE TABLE dbo.Customer (Id int NOT NULL CONSTRAINT PK_Client PRIMARY KEY, Name nvarchar(100) NOT NULL)
GO
INSERT dbo.Customer VALUES (1, N'kept')
