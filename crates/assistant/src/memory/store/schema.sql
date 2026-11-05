CREATE TABLE meta (
 singleton INTEGER PRIMARY KEY CHECK(singleton=1), schema_version INTEGER NOT NULL,
 migration_hash TEXT NOT NULL, store_id TEXT NOT NULL, owner_id TEXT NOT NULL,
 commit_seq INTEGER NOT NULL DEFAULT 0 CHECK(commit_seq>=0),
 deletion_generation INTEGER NOT NULL DEFAULT 0 CHECK(deletion_generation>=0),
 derived_epoch INTEGER NOT NULL DEFAULT 0 CHECK(derived_epoch>=0),
 budget_now INTEGER NOT NULL DEFAULT 0 CHECK(budget_now>=0)
) STRICT;
CREATE TABLE commands (
 principal TEXT NOT NULL, id TEXT NOT NULL, namespace TEXT NOT NULL, request_key TEXT NOT NULL,
 digest BLOB, receipt TEXT CHECK(receipt IS NULL OR json_valid(receipt)),
 PRIMARY KEY(principal,id), UNIQUE(principal,namespace,request_key)
) STRICT;
CREATE TABLE nodes (
 principal TEXT NOT NULL, id TEXT NOT NULL, kind TEXT NOT NULL, label TEXT NOT NULL,
 PRIMARY KEY(principal,id)
) STRICT;
CREATE TABLE command_sources (
 principal TEXT NOT NULL, command_id TEXT NOT NULL, source_id TEXT NOT NULL,
 PRIMARY KEY(principal,command_id,source_id),
 FOREIGN KEY(principal,command_id) REFERENCES commands(principal,id)
) STRICT;
CREATE INDEX receipt_source ON command_sources(principal,source_id,command_id);
CREATE TABLE node_sources (
 principal TEXT NOT NULL, node_id TEXT NOT NULL, source_id TEXT NOT NULL, source_revision INTEGER NOT NULL,
 PRIMARY KEY(principal,node_id,source_id,source_revision),
 FOREIGN KEY(principal,node_id) REFERENCES nodes(principal,id)
) STRICT;
CREATE INDEX node_source ON node_sources(principal,source_id,node_id);
CREATE TABLE sources (
 principal TEXT NOT NULL, id TEXT NOT NULL, revision INTEGER NOT NULL CHECK(revision>0),
 command_id TEXT NOT NULL, text TEXT NOT NULL CHECK(length(CAST(text AS BLOB))<=131072),
 preserved INTEGER NOT NULL CHECK(preserved IN(0,1)),
 suppressed INTEGER NOT NULL DEFAULT 0 CHECK(suppressed IN(0,1)),
 PRIMARY KEY(principal,id,revision),
 FOREIGN KEY(principal,command_id) REFERENCES commands(principal,id)
) STRICT;
CREATE TABLE claim_versions (
 principal TEXT NOT NULL, id TEXT NOT NULL, revision INTEGER NOT NULL CHECK(revision>0),
 subject TEXT NOT NULL, predicate TEXT NOT NULL, object_node TEXT,
 payload TEXT NOT NULL CHECK(json_valid(payload)),
 recorded_from INTEGER NOT NULL CHECK(recorded_from>0), recorded_to INTEGER,
 PRIMARY KEY(principal,id,revision),
 CHECK(recorded_to IS NULL OR recorded_to>recorded_from),
 FOREIGN KEY(principal,subject) REFERENCES nodes(principal,id),
 FOREIGN KEY(principal,object_node) REFERENCES nodes(principal,id)
) STRICT;
CREATE UNIQUE INDEX current_claim ON claim_versions(principal,id) WHERE recorded_to IS NULL;
CREATE INDEX outgoing_claims ON claim_versions(principal,subject,predicate,recorded_to);
CREATE INDEX incoming_claims ON claim_versions(principal,object_node,predicate,recorded_to);
CREATE TABLE evidence (
 principal TEXT NOT NULL, claim_id TEXT NOT NULL, claim_revision INTEGER NOT NULL,
 source_id TEXT NOT NULL, source_revision INTEGER NOT NULL,
 byte_start INTEGER NOT NULL CHECK(byte_start>=0), byte_end INTEGER NOT NULL CHECK(byte_end>byte_start),
 PRIMARY KEY(principal,claim_id,claim_revision,source_id,source_revision,byte_start,byte_end),
 FOREIGN KEY(principal,claim_id,claim_revision) REFERENCES claim_versions(principal,id,revision) ON DELETE CASCADE,
 FOREIGN KEY(principal,source_id,source_revision) REFERENCES sources(principal,id,revision)
) STRICT;
CREATE INDEX source_evidence ON evidence(principal,source_id,source_revision,claim_id);
CREATE TABLE predicate_scopes (
 principal TEXT NOT NULL, subject TEXT NOT NULL, predicate TEXT NOT NULL, revision INTEGER NOT NULL CHECK(revision>0),
 PRIMARY KEY(principal,subject,predicate)
) STRICT;
CREATE TABLE jobs (
 principal TEXT NOT NULL, id TEXT NOT NULL, job_key TEXT NOT NULL,
 source_id TEXT NOT NULL, source_revision INTEGER NOT NULL,
 state TEXT NOT NULL CHECK(state IN('ready','leased','succeeded','unresolved','obsolete','cancelled')),
 fence INTEGER NOT NULL DEFAULT 0 CHECK(fence>=0), lease_instance TEXT, lease_until INTEGER,
 PRIMARY KEY(principal,id), UNIQUE(principal,job_key),
 CHECK((state='leased' AND lease_instance IS NOT NULL AND lease_until IS NOT NULL)
 OR(state<>'leased' AND lease_instance IS NULL AND lease_until IS NULL)),
 FOREIGN KEY(principal,source_id,source_revision) REFERENCES sources(principal,id,revision)
) STRICT;
CREATE INDEX ready_jobs ON jobs(principal,state,id);
CREATE INDEX source_jobs ON jobs(principal,source_id,source_revision);
CREATE TABLE job_attempts (
 principal TEXT NOT NULL, job_id TEXT NOT NULL, fence INTEGER NOT NULL CHECK(fence>0),
 root_cause TEXT NOT NULL, charged_at INTEGER NOT NULL,
 input_tokens INTEGER NOT NULL CHECK(input_tokens>=0),
 output_tokens INTEGER NOT NULL CHECK(output_tokens>=0),
 native_millis INTEGER NOT NULL CHECK(native_millis>=0),
 completion_digest BLOB, result_json TEXT CHECK(result_json IS NULL OR json_valid(result_json)),
 PRIMARY KEY(principal,job_id,fence)
) STRICT;
CREATE INDEX daily_work ON job_attempts(principal,charged_at);
CREATE INDEX cause_work ON job_attempts(principal,root_cause);
CREATE TABLE change_events (
 principal TEXT NOT NULL, seq INTEGER NOT NULL CHECK(seq>0),
 kind TEXT NOT NULL, root_cause TEXT NOT NULL,
 PRIMARY KEY(principal,seq)
) STRICT;
CREATE TABLE search_documents (
 rowid INTEGER PRIMARY KEY, principal TEXT NOT NULL, source_id TEXT NOT NULL,
 source_revision INTEGER NOT NULL, text TEXT NOT NULL,
 UNIQUE(principal,source_id,source_revision),
 FOREIGN KEY(principal,source_id,source_revision) REFERENCES sources(principal,id,revision)
) STRICT;
CREATE VIRTUAL TABLE search_fts USING fts5(text,tokenize='unicode61');
CREATE TABLE forget_operations (
 principal TEXT NOT NULL, source_id TEXT NOT NULL, source_revision INTEGER NOT NULL,
 deletion_generation INTEGER NOT NULL CHECK(deletion_generation>0),
 journaled INTEGER NOT NULL DEFAULT 0 CHECK(journaled IN(0,1)),
 purged INTEGER NOT NULL DEFAULT 0 CHECK(purged IN(0,1)),
 PRIMARY KEY(principal,source_id,source_revision)
) STRICT;
