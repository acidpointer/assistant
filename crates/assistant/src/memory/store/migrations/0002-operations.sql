CREATE TABLE migration_history (
 version INTEGER PRIMARY KEY CHECK(version>0), checksum TEXT NOT NULL
) STRICT;
CREATE TABLE product_operations (
 principal TEXT NOT NULL, id TEXT NOT NULL, input_source TEXT NOT NULL,
 revision INTEGER NOT NULL DEFAULT 1 CHECK(revision>0),
 state TEXT NOT NULL CHECK(state IN('accepted','running','completed','cancelled','failed','interrupted')),
 observed_at INTEGER NOT NULL CHECK(observed_at>=0), limits_json TEXT NOT NULL CHECK(json_valid(limits_json)),
 reserved_bytes INTEGER NOT NULL CHECK(reserved_bytes>=0),
 assistant_source TEXT, unknown_tool_outcome INTEGER NOT NULL DEFAULT 0 CHECK(unknown_tool_outcome IN(0,1)),
 redacted INTEGER NOT NULL DEFAULT 0 CHECK(redacted IN(0,1)),
 CHECK((state IN('accepted','running') AND reserved_bytes>0) OR (state NOT IN('accepted','running') AND reserved_bytes=0)),
 PRIMARY KEY(principal,id),
 FOREIGN KEY(principal,id) REFERENCES commands(principal,id)
) STRICT;
CREATE UNIQUE INDEX one_active_operation ON product_operations(principal) WHERE state IN('accepted','running');
CREATE INDEX outstanding_reservations ON product_operations(reserved_bytes) WHERE reserved_bytes>0;
CREATE TABLE source_metadata (
 principal TEXT NOT NULL, source_id TEXT NOT NULL, source_revision INTEGER NOT NULL,
 role TEXT NOT NULL CHECK(role IN('user','assistant','tool')), operation_id TEXT, ordinal INTEGER,
 observed_at INTEGER CHECK(observed_at>=0),
 formation TEXT NOT NULL CHECK(formation IN('queued','deferred','complete','unresolved','not_applicable')),
 PRIMARY KEY(principal,source_id,source_revision), UNIQUE(principal,operation_id,ordinal),
 FOREIGN KEY(principal,source_id,source_revision) REFERENCES sources(principal,id,revision) ON DELETE CASCADE,
 FOREIGN KEY(principal,operation_id) REFERENCES product_operations(principal,id)
) STRICT;
INSERT INTO source_metadata(principal,source_id,source_revision,role,formation)
 SELECT s.principal,s.id,s.revision,'user',
 CASE WHEN EXISTS(SELECT 1 FROM jobs j WHERE j.principal=s.principal AND j.source_id=s.id AND j.state='succeeded') THEN 'complete'
 WHEN EXISTS(SELECT 1 FROM jobs j WHERE j.principal=s.principal AND j.source_id=s.id AND j.state='unresolved') THEN 'unresolved'
 WHEN EXISTS(SELECT 1 FROM jobs j WHERE j.principal=s.principal AND j.source_id=s.id AND j.state IN('ready','leased')) THEN 'queued'
 ELSE 'not_applicable' END FROM sources s;
CREATE INDEX operation_sources ON source_metadata(principal,operation_id,source_id);
CREATE INDEX deferred_formation ON source_metadata(principal,formation,source_id);
CREATE TABLE tool_invocations (
 principal TEXT NOT NULL, operation_id TEXT NOT NULL, ordinal INTEGER NOT NULL CHECK(ordinal>0),
 handler TEXT NOT NULL, effect TEXT NOT NULL CHECK(effect IN('read_only','external')),
 capture TEXT NOT NULL CHECK(capture IN('ephemeral','retained')), arguments TEXT,
 state TEXT NOT NULL CHECK(state IN('intent','succeeded','failed','unknown')),
 result_source TEXT,
 PRIMARY KEY(principal,operation_id,ordinal),
 CHECK(capture='retained' OR (arguments IS NULL AND result_source IS NULL)),
 FOREIGN KEY(principal,operation_id) REFERENCES product_operations(principal,id)
) STRICT;
CREATE TABLE event_consumers (
 principal TEXT NOT NULL, name TEXT NOT NULL,
 requirement TEXT NOT NULL CHECK(requirement IN('required','optional')),
 cursor INTEGER NOT NULL DEFAULT 0 CHECK(cursor>=0),
 PRIMARY KEY(principal,name)
) STRICT;
