CREATE XML SCHEMA COLLECTION dbo.Doc AS N'<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><xs:element name="doc" type="xs:string"/></xs:schema>'
GO
CREATE TABLE dbo.T (Id int NOT NULL CONSTRAINT PK_T PRIMARY KEY, X xml(dbo.Doc) NULL)
