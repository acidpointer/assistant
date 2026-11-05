-- Derived E5 rows are recoverable; source eligibility remains authoritative.
CREATE TABLE embedding_coverage (
 principal TEXT NOT NULL, source_id TEXT NOT NULL, source_revision INTEGER NOT NULL,
 model TEXT NOT NULL, covered_bytes INTEGER NOT NULL CHECK(covered_bytes>=0),
 complete INTEGER NOT NULL CHECK(complete IN(0,1)),
 PRIMARY KEY(principal,source_id,source_revision,model),
 FOREIGN KEY(principal,source_id,source_revision) REFERENCES sources(principal,id,revision) ON DELETE CASCADE
) STRICT;
CREATE TABLE embedding_chunks (
 principal TEXT NOT NULL, source_id TEXT NOT NULL, source_revision INTEGER NOT NULL,
 model TEXT NOT NULL, byte_start INTEGER NOT NULL CHECK(byte_start>=0),
 byte_end INTEGER NOT NULL CHECK(byte_end>byte_start),
 vector BLOB NOT NULL CHECK(length(vector)=1536),
 PRIMARY KEY(principal,source_id,source_revision,model,byte_start),
 FOREIGN KEY(principal,source_id,source_revision) REFERENCES sources(principal,id,revision) ON DELETE CASCADE
) STRICT;
CREATE INDEX embedding_model ON embedding_chunks(principal,model,source_id);
