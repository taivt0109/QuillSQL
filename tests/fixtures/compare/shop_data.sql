-- Rows for the data-preservation tests.
SET IDENTITY_INSERT dbo.Customer ON;
INSERT INTO dbo.Customer (CustomerId, Name, Email, Balance) VALUES (1, N'An', 'an@example.com', 10), (2, N'Binh', NULL, 0), (7, N'Chi', 'chi@example.com', 5.5);
SET IDENTITY_INSERT dbo.Customer OFF;
INSERT INTO Sales.[Order] (OrderId, CustomerId, Total, Status) VALUES (1, 1, 12.5, 'N'), (2, 1, 3, 'P'), (3, 7, 99.99, 'S');
INSERT INTO Sales.OrderLine (OrderId, LineNumber, Qty) VALUES (1, 1, 2), (1, 2, 1), (3, 1, 5);
