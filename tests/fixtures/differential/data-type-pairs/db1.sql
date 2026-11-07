CREATE TABLE dbo.P (Id int NOT NULL CONSTRAINT PK_P PRIMARY KEY, A int NULL, B varchar(20) NULL, C datetime NULL, D decimal(10,2) NULL)
GO
INSERT dbo.P VALUES (1, 5, 'x', '2024-01-01T10:00:00', 1.50), (2, 6, 'y', '2024-01-02', 2.25)
