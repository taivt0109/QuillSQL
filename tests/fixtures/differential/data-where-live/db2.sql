CREATE TABLE dbo.Orders (Id int NOT NULL CONSTRAINT PK_Orders PRIMARY KEY, Region char(2) NOT NULL, Amount int NOT NULL)
GO
INSERT dbo.Orders VALUES (1, 'EU', 11), (3, 'US', 99), (5, 'US', 50), (6, 'EU', 60)
