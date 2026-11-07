CREATE ROLE AppRole
GO
CREATE TYPE dbo.Phone FROM varchar(20) NULL
GO
CREATE TYPE dbo.Code FROM int NULL
GO
CREATE RULE dbo.Positive AS @v > 0
GO
EXEC sp_bindrule N'dbo.Positive', N'dbo.Code'
GO
GRANT REFERENCES ON TYPE::dbo.Phone TO AppRole
