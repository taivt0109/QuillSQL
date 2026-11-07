SELECT dbo.Limit() AS limit, (SELECT COUNT(*) FROM sys.check_constraints WHERE name = 'CK_T_A') AS ck, (SELECT COUNT(*) FROM sys.views WHERE name = 'V') AS v
SELECT Id, A, L FROM dbo.T
