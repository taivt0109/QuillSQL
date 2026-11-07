CREATE TABLE dbo.Parent (Id int NOT NULL CONSTRAINT PK_Parent PRIMARY KEY, Name nvarchar(20) NULL)
GO
CREATE TABLE dbo.Child (Id int NOT NULL CONSTRAINT PK_Child PRIMARY KEY, ParentId int NOT NULL CONSTRAINT FK_Child_Parent REFERENCES dbo.Parent (Id))
GO
INSERT dbo.Parent VALUES (1, N'a'), (3, N'c')
INSERT dbo.Child VALUES (10, 1), (30, 3)
GO
CREATE INDEX IX_Child_Parent ON dbo.Child (ParentId)
