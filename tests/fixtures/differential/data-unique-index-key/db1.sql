CREATE TABLE dbo.U (Code varchar(10) NOT NULL, Name nvarchar(50) NULL)
GO
CREATE UNIQUE INDEX UX_U_Code ON dbo.U (Code)
GO
INSERT dbo.U VALUES ('a', N'one'), ('b', N'two')
