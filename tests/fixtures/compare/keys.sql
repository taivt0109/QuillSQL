-- Tables whose keys are hard to order the same way on the client as on the server.
CREATE TABLE dbo.KeyText (Code nvarchar(20) COLLATE Latin1_General_CI_AS NOT NULL PRIMARY KEY, V int NULL);
CREATE TABLE dbo.KeyVarchar (Code varchar(20) COLLATE SQL_Latin1_General_CP1_CI_AS NOT NULL PRIMARY KEY, V int NULL);
CREATE TABLE dbo.KeyGuid (Id uniqueidentifier NOT NULL PRIMARY KEY, V int NULL);
CREATE TABLE dbo.KeyMixed (A varchar(10) NOT NULL, B datetimeoffset(3) NOT NULL, C varbinary(8) NOT NULL, V decimal(10, 3) NULL, PRIMARY KEY (A, B, C));
GO
INSERT INTO dbo.KeyText (Code, V) VALUES (N'a', 1), (N'B', 2), (N'ä', 3), (N'Z', 4), (N'_x', 5), (N' lead', 6), (N'ß', 7), (N'中', 8), (N'😀', 9), (N'a-b', 10), (N'ab', 11), (N'Ac ', 12), (N'0', 13), (N'é', 14), (N'E', 15);
INSERT INTO dbo.KeyVarchar (Code, V) VALUES ('a', 1), ('B', 2), ('Z', 4), ('_x', 5), (' lead', 6), ('a-b', 10), ('ab', 11), ('0', 13), ('~', 16), ('[', 17);
INSERT INTO dbo.KeyGuid (Id, V) SELECT TOP (500) NEWID(), ROW_NUMBER() OVER (ORDER BY (SELECT 1)) FROM sys.all_objects;
INSERT INTO dbo.KeyMixed (A, B, C, V) VALUES
    ('x', '2024-01-01T10:00:00+07:00', 0x01, 1.5),
    ('x', '2024-01-01T04:00:00+00:00', 0x0100, 2),
    ('x', '2024-01-01T03:30:00-01:00', 0x02, 3.25),
    ('y', '1999-12-31T23:59:59.999+14:00', 0x, NULL),
    ('y', '2000-01-01T00:00:00+00:00', 0xFF, -4);
GO
