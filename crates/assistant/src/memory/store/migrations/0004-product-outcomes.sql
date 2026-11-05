-- Bounded host-only terminal metadata. Literal assistant text stays in sources.
CREATE TABLE product_outcomes (
 principal TEXT NOT NULL, operation_id TEXT NOT NULL,
 details TEXT NOT NULL CHECK(json_valid(details) AND length(CAST(details AS BLOB))<=2048),
 PRIMARY KEY(principal,operation_id),
 FOREIGN KEY(principal,operation_id) REFERENCES product_operations(principal,id)
) STRICT;
