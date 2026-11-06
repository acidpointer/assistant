CREATE TABLE operation_history (
 ordinal INTEGER PRIMARY KEY AUTOINCREMENT,
 principal TEXT NOT NULL, operation_id TEXT NOT NULL,
 client_key TEXT CHECK(client_key IS NULL OR json_valid(client_key)),
 UNIQUE(principal,operation_id),
 FOREIGN KEY(principal,operation_id) REFERENCES product_operations(principal,id)
) STRICT;
INSERT INTO operation_history(principal,operation_id)
 SELECT p.principal,p.id FROM product_operations p JOIN commands c
 ON c.principal=p.principal AND c.id=p.id ORDER BY c.rowid;
CREATE INDEX owner_operation_history ON operation_history(principal,ordinal);
