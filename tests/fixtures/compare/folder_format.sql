-- Everything the scripts-folder writer has a layout rule for that the other fixtures leave out,
-- for comparing our /makescripts folder byte for byte with SQL Compare's.
DECLARE @db sysname = DB_NAME(), @sql nvarchar(max);
SET @sql = N'ALTER DATABASE ' + QUOTENAME(@db) + N' SET CHANGE_TRACKING = ON (CHANGE_RETENTION = 2 DAYS, AUTO_CLEANUP = ON);
ALTER DATABASE ' + QUOTENAME(@db) + N' ADD FILEGROUP FG2;
ALTER DATABASE ' + QUOTENAME(@db) + N' ADD FILE (NAME = N''' + @db + N'_fg2'', FILENAME = N''/var/opt/mssql/data/' + @db + N'_fg2.ndf'') TO FILEGROUP FG2;';
EXEC (@sql);
GO
CREATE USER Clerk WITHOUT LOGIN WITH DEFAULT_SCHEMA = Shop;
GO
CREATE USER Viewer WITHOUT LOGIN;
GO
CREATE ROLE Staff AUTHORIZATION Clerk;
GO
CREATE ROLE Seniors;
GO
ALTER ROLE Staff ADD MEMBER Seniors;
GO
ALTER ROLE Staff ADD MEMBER Viewer;
GO
ALTER ROLE db_datawriter ADD MEMBER Clerk;
GO
GRANT CREATE TABLE TO Staff;
GO
DENY BACKUP DATABASE TO Viewer;
GO
CREATE SCHEMA Shop AUTHORIZATION Clerk;
GO
GRANT SELECT ON SCHEMA::Shop TO Viewer;
GO
EXEC sp_addextendedproperty N'MS_Description', N'Shop things', 'SCHEMA', N'Shop';
GO
EXEC sp_addextendedproperty N'Version', N'3', NULL, NULL, NULL, NULL, NULL, NULL;
GO
CREATE TYPE Shop.Code FROM nvarchar(12) NOT NULL;
GO
CREATE TYPE Shop.Money2 FROM decimal(9, 3) NULL;
GO
CREATE TYPE Shop.Stamp FROM datetime2(3) NULL;
GO
CREATE TYPE Shop.Basket AS TABLE (
    Line int NOT NULL PRIMARY KEY NONCLUSTERED,
    Sku varchar(20) COLLATE Latin1_General_BIN NOT NULL DEFAULT 'x',
    Qty int NOT NULL CHECK (Qty > 0),
    Price money NULL,
    INDEX IX_Sku (Sku DESC),
    UNIQUE (Sku, Qty)
);
GO
GRANT EXECUTE ON TYPE::Shop.Basket TO Staff;
GO
CREATE PARTITION FUNCTION pfQuarter (date) AS RANGE LEFT FOR VALUES ('2024-03-31', '2024-06-30');
GO
CREATE PARTITION SCHEME psQuarter AS PARTITION pfQuarter TO (FG2, [PRIMARY], FG2);
GO
CREATE TABLE Shop.Product (
    Id int IDENTITY(10, 5) NOT FOR REPLICATION NOT NULL,
    Code Shop.Code,
    Name nvarchar(100) COLLATE Latin1_General_CI_AS NOT NULL CONSTRAINT DF_Product_Name DEFAULT (N'?'),
    Price decimal(10, 2) NOT NULL DEFAULT 0,
    Weight float(24) NULL,
    Ratio float NULL,
    Made datetime2(0) NULL,
    Seen datetime2 NULL,
    Lasts time(3) NULL,
    Zoned datetimeoffset NULL,
    Small smalldatetime NULL,
    Big varbinary(max) NULL,
    Fixed char(3) NULL,
    Uid uniqueidentifier ROWGUIDCOL NOT NULL CONSTRAINT DF_Product_Uid DEFAULT NEWSEQUENTIALID(),
    Total AS (Price * 2) PERSISTED NOT NULL,
    Half AS (Price / 2),
    Secret nvarchar(40) MASKED WITH (FUNCTION = 'partial(1,"xx",0)') NULL,
    Notes nvarchar(max) SPARSE NULL,
    Extra xml COLUMN_SET FOR ALL_SPARSE_COLUMNS,
    Num numeric(5, 0) NULL,
    Stamp Shop.Stamp NULL,
    Ver rowversion,
    CONSTRAINT PK_Product PRIMARY KEY NONCLUSTERED (Id) WITH (FILLFACTOR = 80, PAD_INDEX = ON) ON FG2,
    CONSTRAINT UQ_Product_Code UNIQUE CLUSTERED (Code DESC, Name) WITH (IGNORE_DUP_KEY = OFF, ALLOW_PAGE_LOCKS = OFF),
    CONSTRAINT CK_Product_Price CHECK NOT FOR REPLICATION (Price >= 0),
    CONSTRAINT CK_Product_Name CHECK (LEN(Name) > 0)
) TEXTIMAGE_ON FG2;
GO
ALTER TABLE Shop.Product SET (LOCK_ESCALATION = AUTO);
GO
ALTER TABLE Shop.Product ENABLE CHANGE_TRACKING WITH (TRACK_COLUMNS_UPDATED = ON);
GO
CREATE NONCLUSTERED INDEX ix_Product_Price ON Shop.Product (Price) INCLUDE (Name, Made) WITH (STATISTICS_NORECOMPUTE = ON);
GO
CREATE NONCLUSTERED INDEX IX_Product_Name ON Shop.Product (Name) WHERE Name > N'A' WITH (ALLOW_ROW_LOCKS = OFF) ON FG2;
GO
CREATE INDEX IX_Product_Made ON Shop.Product (Made, Id DESC);
GO
CREATE INDEX IX_Product_Made2 ON Shop.Product (Made) INCLUDE (Price);
GO
CREATE UNIQUE INDEX UX_Product_Uid ON Shop.Product (Uid) WITH (OPTIMIZE_FOR_SEQUENTIAL_KEY = ON);
GO
ALTER INDEX IX_Product_Made2 ON Shop.Product DISABLE;
GO
CREATE STATISTICS ST_Product_Weight ON Shop.Product (Weight, Ratio) WHERE Weight > 1;
GO
CREATE STATISTICS st_Product_Code ON Shop.Product (Code);
GO
EXEC sp_addextendedproperty N'MS_Description', N'Products', 'SCHEMA', N'Shop', 'TABLE', N'Product';
GO
EXEC sp_addextendedproperty N'MS_Description', N'Its name', 'SCHEMA', N'Shop', 'TABLE', N'Product', 'COLUMN', N'Name';
GO
EXEC sp_addextendedproperty N'MS_Description', N'Price index', 'SCHEMA', N'Shop', 'TABLE', N'Product', 'INDEX', N'ix_Product_Price';
GO
EXEC sp_addextendedproperty N'MS_Description', N'Positive', 'SCHEMA', N'Shop', 'TABLE', N'Product', 'CONSTRAINT', N'CK_Product_Price';
GO
GRANT SELECT ON Shop.Product TO Viewer;
GO
GRANT UPDATE (Price, Name) ON Shop.Product TO Staff WITH GRANT OPTION;
GO
DENY DELETE ON Shop.Product TO Viewer;
GO
GRANT INSERT ON Shop.Product TO Staff;
GO
CREATE TABLE Shop.Sale (
    Id bigint NOT NULL,
    Day date NOT NULL,
    ProductId int NOT NULL,
    Qty smallint NOT NULL CONSTRAINT DF_Sale_Qty DEFAULT ((1)),
    Note varchar(200) NULL,
    CONSTRAINT PK_Sale PRIMARY KEY CLUSTERED (Id, Day) WITH (DATA_COMPRESSION = ROW) ON psQuarter (Day)
) ON psQuarter (Day);
GO
ALTER TABLE Shop.Sale SET (LOCK_ESCALATION = DISABLE);
GO
ALTER TABLE Shop.Sale WITH NOCHECK ADD CONSTRAINT FK_Sale_Product FOREIGN KEY (ProductId) REFERENCES Shop.Product (Id) ON DELETE CASCADE ON UPDATE NO ACTION NOT FOR REPLICATION;
GO
ALTER TABLE Shop.Sale ADD CONSTRAINT CK_Sale_Qty CHECK (Qty <> 0);
GO
ALTER TABLE Shop.Sale NOCHECK CONSTRAINT CK_Sale_Qty;
GO
ALTER TABLE Shop.Sale WITH NOCHECK ADD CONSTRAINT CK_Sale_Day CHECK (Day > '2000-01-01');
GO
CREATE INDEX IX_Sale_Product ON Shop.Sale (ProductId) WITH (DATA_COMPRESSION = PAGE ON PARTITIONS (1, 3)) ON psQuarter (Day);
GO
CREATE INDEX IX_Sale_Note ON Shop.Sale (Note) WITH (FILLFACTOR = 60) ON [PRIMARY];
GO
CREATE TABLE Shop.Line (
    SaleId bigint NOT NULL,
    Day date NOT NULL,
    No int NOT NULL,
    Parent int NULL,
    CONSTRAINT PK_Line PRIMARY KEY (SaleId, Day, No),
    CONSTRAINT FK_Line_Sale FOREIGN KEY (SaleId, Day) REFERENCES Shop.Sale (Id, Day) ON DELETE CASCADE ON UPDATE CASCADE,
    CONSTRAINT FK_Line_Line FOREIGN KEY (SaleId, Day, Parent) REFERENCES Shop.Line (SaleId, Day, No)
);
GO
ALTER TABLE Shop.Line NOCHECK CONSTRAINT FK_Line_Line;
GO
CREATE TABLE dbo.Heap (A int NULL, B text NULL, C image NULL) ON FG2 TEXTIMAGE_ON [PRIMARY] WITH (DATA_COMPRESSION = PAGE);
GO
CREATE TABLE dbo.Plain (Id int NOT NULL CONSTRAINT PK_Plain PRIMARY KEY, V nvarchar(20) NULL);
GO
CREATE TRIGGER dbo.Plain_Ins ON dbo.Plain AFTER INSERT AS SET NOCOUNT ON;
GO
CREATE TRIGGER dbo.Plain_Upd ON dbo.Plain FOR UPDATE, DELETE NOT FOR REPLICATION AS BEGIN SET NOCOUNT ON; END;
GO
EXEC sp_settriggerorder N'dbo.Plain_Upd', N'First', 'UPDATE';
GO
DISABLE TRIGGER dbo.Plain_Ins ON dbo.Plain;
GO
EXEC sp_addextendedproperty N'MS_Description', N'Trigger', 'SCHEMA', N'dbo', 'TABLE', N'Plain', 'TRIGGER', N'Plain_Ins';
GO
CREATE VIEW dbo.PlainView WITH SCHEMABINDING AS SELECT Id, V FROM dbo.Plain;
GO
CREATE UNIQUE CLUSTERED INDEX CX_PlainView ON dbo.PlainView (Id);
GO
CREATE INDEX IX_PlainView_V ON dbo.PlainView (V);
GO
CREATE TRIGGER dbo.PlainView_Ins ON dbo.PlainView INSTEAD OF INSERT AS SELECT 1;
GO
GRANT SELECT ON dbo.PlainView TO Viewer;
GO
EXEC sp_addextendedproperty N'MS_Description', N'A view', 'SCHEMA', N'dbo', 'VIEW', N'PlainView';
GO
EXEC sp_addextendedproperty N'MS_Description', N'Its id', 'SCHEMA', N'dbo', 'VIEW', N'PlainView', 'COLUMN', N'Id';
GO
CREATE FUNCTION Shop.Inline (@id int) RETURNS TABLE AS RETURN SELECT Id FROM Shop.Product WHERE Id = @id;
GO
CREATE FUNCTION Shop.Multi (@id int) RETURNS @t TABLE (Id int) WITH EXECUTE AS OWNER AS BEGIN INSERT @t VALUES (@id); RETURN; END;
GO
GRANT SELECT ON Shop.Inline TO Viewer;
GO
EXEC sp_addextendedproperty N'MS_Description', N'Param', 'SCHEMA', N'Shop', 'FUNCTION', N'Inline', 'PARAMETER', N'@id';
GO
CREATE PROCEDURE Shop.Restock @id int, @qty int = 1 OUTPUT WITH RECOMPILE AS
BEGIN
    -- comment with 'quotes'
    UPDATE Shop.Sale SET Qty = @qty WHERE ProductId = @id;
END
GO
GRANT EXECUTE ON Shop.Restock TO Staff;
GO
DENY EXECUTE ON Shop.Restock TO Viewer;
GO
CREATE SEQUENCE Shop.Ticket AS smallint START WITH 5 INCREMENT BY -1 MINVALUE -100 MAXVALUE 100 CYCLE CACHE 7;
GO
CREATE SEQUENCE Shop.Ticket2 AS decimal(10, 0) NO CACHE;
GO
GRANT UPDATE ON Shop.Ticket TO Staff;
GO
CREATE SYNONYM Shop.Prod FOR Shop.Product;
GO
CREATE SYNONYM dbo.Remote FOR OtherDb.dbo.Thing;
GO
CREATE MESSAGE TYPE [//Shop/Msg] VALIDATION = WELL_FORMED_XML;
GO
CREATE MESSAGE TYPE [//Shop/Empty] VALIDATION = EMPTY;
GO
CREATE CONTRACT [//Shop/Contract] ([//Shop/Msg] SENT BY INITIATOR, [//Shop/Empty] SENT BY ANY);
GO
CREATE QUEUE Shop.InQueue WITH STATUS = ON, RETENTION = OFF, POISON_MESSAGE_HANDLING (STATUS = ON);
GO
CREATE SERVICE [//Shop/Service] ON QUEUE Shop.InQueue ([//Shop/Contract]);
GO
CREATE ROUTE ShopRoute WITH SERVICE_NAME = '//Shop/Service', ADDRESS = 'LOCAL';
GO
CREATE APPLICATION ROLE Kiosk WITH PASSWORD = 'Str0ng!Pass1';
GO
