-- Objects beyond tables and modules, for tests/objects_live.rs ({CLR} is replaced by the
-- assembly's hex).
CREATE ASSEMBLY QuillClr FROM {CLR} WITH PERMISSION_SET = SAFE;
GO
CREATE FUNCTION dbo.AddOne(@value int) RETURNS int AS EXTERNAL NAME QuillClr.QuillFunctions.AddOne;
GO
CREATE PROCEDURE dbo.Hello @name nvarchar(50) AS EXTERNAL NAME QuillClr.QuillFunctions.Hello;
GO
CREATE AGGREGATE dbo.CountAll(@value int) RETURNS int EXTERNAL NAME QuillClr.CountAll;
GO
CREATE TYPE dbo.Point EXTERNAL NAME QuillClr.Point;
GO
CREATE XML SCHEMA COLLECTION dbo.OrderSchema AS N'<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><xs:element name="order" type="xs:string"/></xs:schema>';
GO
CREATE DEFAULT dbo.Zero AS 0;
GO
CREATE RULE dbo.Positive AS @v >= 0;
GO
CREATE TYPE dbo.Amount FROM decimal(10, 2) NULL;
GO
CREATE PROCEDURE dbo.UseAmount @a dbo.Amount AS SELECT @a AS a;
GO
CREATE PROCEDURE dbo.UseDoc @d xml(dbo.OrderSchema) AS SELECT @d AS d;
GO
CREATE PARTITION FUNCTION pfYears (int) AS RANGE RIGHT FOR VALUES (2020, 2021, 2022);
GO
CREATE PARTITION SCHEME psYears AS PARTITION pfYears ALL TO ([PRIMARY]);
GO
CREATE TABLE dbo.Orders (
    Id int NOT NULL CONSTRAINT PK_Orders PRIMARY KEY,
    Yr int NOT NULL,
    Total dbo.Amount,
    Doc xml(dbo.OrderSchema) NULL,
    Qty int NULL,
    TenantId int NOT NULL CONSTRAINT DF_Orders_Tenant DEFAULT 1,
    Secret nvarchar(50) NULL
);
GO
EXEC sp_bindefault 'dbo.Zero', 'dbo.Orders.Qty';
EXEC sp_bindrule 'dbo.Positive', 'dbo.Orders.Qty';
GO
CREATE STATISTICS ST_Orders_Yr ON dbo.Orders (Yr, Qty) WITH NORECOMPUTE;
GO
ADD SENSITIVITY CLASSIFICATION TO dbo.Orders.Secret WITH (LABEL = 'Confidential', INFORMATION_TYPE = 'Financial', RANK = HIGH);
GO
CREATE TABLE dbo.Parted (Id int NOT NULL, Yr int NOT NULL) ON psYears (Yr);
GO
CREATE TRIGGER trgQuillDdl ON DATABASE FOR CREATE_PROCEDURE AS SET NOCOUNT ON;
GO
CREATE MESSAGE TYPE [//Quill/Order] VALIDATION = WELL_FORMED_XML;
GO
CREATE MESSAGE TYPE [//Quill/Typed] VALIDATION = VALID_XML WITH SCHEMA COLLECTION dbo.OrderSchema;
GO
CREATE CONTRACT [//Quill/Contract] ([//Quill/Order] SENT BY INITIATOR, [//Quill/Typed] SENT BY TARGET);
GO
CREATE PROCEDURE dbo.Activate AS SET NOCOUNT ON;
GO
CREATE QUEUE dbo.OrderQueue WITH STATUS = ON, ACTIVATION (STATUS = OFF, PROCEDURE_NAME = dbo.Activate, MAX_QUEUE_READERS = 2, EXECUTE AS OWNER);
GO
CREATE SERVICE [//Quill/Service] ON QUEUE dbo.OrderQueue ([//Quill/Contract]);
GO
CREATE ROUTE QuillRoute WITH SERVICE_NAME = '//Quill/Remote', ADDRESS = 'TCP://10.0.0.1:4022';
GO
CREATE EVENT NOTIFICATION QuillTables ON DATABASE FOR CREATE_TABLE TO SERVICE '//Quill/Service', 'D8D63B6C-0000-4000-8000-000000000001';
GO
CREATE USER QuillApp WITHOUT LOGIN;
GO
CREATE REMOTE SERVICE BINDING QuillBinding TO SERVICE '//Quill/Remote' WITH USER = QuillApp, ANONYMOUS = OFF;
GO
CREATE APPLICATION ROLE QuillAppRole WITH PASSWORD = 'Str0ng!Pass', DEFAULT_SCHEMA = dbo;
GO
CREATE SCHEMA rls;
GO
CREATE FUNCTION rls.fn_tenant(@TenantId int) RETURNS TABLE WITH SCHEMABINDING AS RETURN SELECT 1 AS ok WHERE @TenantId = 1;
GO
CREATE SECURITY POLICY rls.TenantPolicy ADD FILTER PREDICATE rls.fn_tenant(TenantId) ON dbo.Orders WITH (STATE = ON);
GO
CREATE TABLE dbo.Versioned (
    Id int NOT NULL CONSTRAINT PK_Versioned PRIMARY KEY,
    Name nvarchar(50) NULL,
    ValidFrom datetime2 GENERATED ALWAYS AS ROW START NOT NULL,
    ValidTo datetime2 GENERATED ALWAYS AS ROW END NOT NULL,
    PERIOD FOR SYSTEM_TIME (ValidFrom, ValidTo)
) WITH (SYSTEM_VERSIONING = ON (HISTORY_TABLE = dbo.VersionedHistory, HISTORY_RETENTION_PERIOD = 6 MONTHS));
GO
CREATE TABLE dbo.Person (Id int NOT NULL CONSTRAINT PK_Person PRIMARY KEY, Name nvarchar(50) NULL) AS NODE;
GO
CREATE TABLE dbo.Knows (Since int NULL) AS EDGE;
GO
ALTER TABLE dbo.Knows ADD CONSTRAINT EC_Knows CONNECTION (dbo.Person TO dbo.Person);
GO
GRANT SELECT (Qty) ON dbo.Orders TO QuillApp;
GO
GRANT EXECUTE ON TYPE::dbo.Amount TO QuillApp;
GO
GRANT SEND ON SERVICE::[//Quill/Service] TO QuillApp;
