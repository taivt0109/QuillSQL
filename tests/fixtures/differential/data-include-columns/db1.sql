CREATE TABLE dbo.E (Id int NOT NULL CONSTRAINT PK_E PRIMARY KEY, Name nvarchar(50) NULL, Modified datetime2 NULL)
GO
INSERT dbo.E VALUES (1, N'a', '2024-01-01'), (2, N'b', '2024-01-02')
