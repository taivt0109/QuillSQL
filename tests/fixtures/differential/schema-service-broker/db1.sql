CREATE MESSAGE TYPE [//diff/Request] VALIDATION = WELL_FORMED_XML
GO
CREATE CONTRACT [//diff/Contract] ([//diff/Request] SENT BY INITIATOR)
GO
CREATE QUEUE dbo.RequestQueue WITH STATUS = ON
GO
CREATE SERVICE [//diff/Service] ON QUEUE dbo.RequestQueue ([//diff/Contract])
