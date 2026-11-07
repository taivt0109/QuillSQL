CREATE TABLE dbo.E (Id int NOT NULL CONSTRAINT PK_E PRIMARY KEY, Name nvarchar(50) NULL, Modified datetime2 NULL)
GO
INSERT dbo.E VALUES (1, N'a', '2025-05-05'), (2, N'x', '2025-05-05')
