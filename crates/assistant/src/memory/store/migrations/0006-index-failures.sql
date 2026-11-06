CREATE TABLE embedding_failures (
 principal TEXT NOT NULL, source_id TEXT NOT NULL, source_revision INTEGER NOT NULL,
 model TEXT NOT NULL, attempts INTEGER NOT NULL CHECK(attempts BETWEEN 1 AND 3),
 retry_after INTEGER CHECK(retry_after>=0),
 PRIMARY KEY(principal,source_id,source_revision,model),
 FOREIGN KEY(principal,source_id,source_revision) REFERENCES sources(principal,id,revision) ON DELETE CASCADE
) STRICT;
