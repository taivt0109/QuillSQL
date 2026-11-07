SELECT Id, A FROM dbo.T ORDER BY Id
SELECT COUNT(*) AS nulls FROM dbo.T_History WHERE A IS NULL
SELECT is_nullable FROM sys.columns WHERE object_id = OBJECT_ID('dbo.T_History') AND name = 'A'
