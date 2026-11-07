CREATE TABLE dbo.K (Id int NOT NULL CONSTRAINT PK_K PRIMARY KEY, A int NULL, Twice AS (A * 2), RV rowversion)
GO
INSERT dbo.K (Id, A) VALUES (1, 5), (3, 3)
