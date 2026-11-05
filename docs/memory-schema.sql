-- Executable constraint sketch, not a production migration.
-- See memory-implementation-spec.md for remaining typed payloads, indexes,
-- migrations, lifecycle transitions, purge rules and writer-owned validation.
-- TEXT ids here are host-generated opaque identifiers; the Rust ID constructor
-- enforces 128-bit random / 32 lowercase hexadecimal representation.
PRAGMA foreign_keys = ON;

CREATE TABLE principals (
    id TEXT PRIMARY KEY,
    commit_seq INTEGER NOT NULL DEFAULT 0 CHECK (commit_seq >= 0),
    deletion_generation INTEGER NOT NULL DEFAULT 0 CHECK (deletion_generation >= 0),
    derived_epoch INTEGER NOT NULL DEFAULT 0 CHECK (derived_epoch >= 0)
) STRICT;

CREATE TABLE operations (
    principal TEXT NOT NULL REFERENCES principals(id),
    id TEXT NOT NULL,
    client_namespace TEXT NOT NULL,
    request_key TEXT NOT NULL,
    digest BLOB,
    state TEXT NOT NULL CHECK (state IN
      ('admitted','running','completed','truncated','cancelled','deadline','failed','interrupted','forgotten')),
    PRIMARY KEY (principal,id),
    UNIQUE (principal,client_namespace,request_key)
) STRICT;

CREATE TABLE sources (
    principal TEXT NOT NULL,
    id TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    operation_id TEXT NOT NULL,
    payload TEXT NOT NULL CHECK (length(CAST(payload AS BLOB)) <= 131072),
    suppressed INTEGER NOT NULL DEFAULT 0 CHECK (suppressed IN (0,1)),
    PRIMARY KEY (principal,id,revision),
    FOREIGN KEY (principal,operation_id) REFERENCES operations(principal,id)
) STRICT;

CREATE TABLE nodes (
    principal TEXT NOT NULL REFERENCES principals(id),
    id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN
      ('person','agent','organization','project','goal','commitment','question','place','occupation','topic','claim')),
    PRIMARY KEY (principal,id)
) STRICT;

CREATE TABLE claim_versions (
    principal TEXT NOT NULL,
    id TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    subject TEXT NOT NULL,
    predicate TEXT NOT NULL,
    object_node TEXT,
    object_literal TEXT,
    polarity TEXT NOT NULL CHECK (polarity IN ('positive','negative')),
    modality TEXT NOT NULL CHECK (modality IN ('asserted','reported','hypothetical','question','fiction')),
    state TEXT NOT NULL CHECK (state IN ('active','superseded','retracted','disputed','stale')),
    valid_from INTEGER,
    valid_to INTEGER,
    recorded_from INTEGER NOT NULL CHECK (recorded_from > 0),
    recorded_to INTEGER,
    PRIMARY KEY (principal,id,revision),
    CHECK ((object_node IS NULL) <> (object_literal IS NULL)),
    CHECK (valid_from IS NULL OR valid_to IS NULL OR valid_from < valid_to),
    CHECK (recorded_to IS NULL OR recorded_from < recorded_to),
    FOREIGN KEY (principal,id) REFERENCES nodes(principal,id),
    FOREIGN KEY (principal,subject) REFERENCES nodes(principal,id),
    FOREIGN KEY (principal,object_node) REFERENCES nodes(principal,id)
) STRICT;
CREATE UNIQUE INDEX one_current_record_version
    ON claim_versions(principal,id) WHERE recorded_to IS NULL;
CREATE INDEX claim_subject_predicate
    ON claim_versions(principal,subject,predicate,state);
CREATE INDEX claim_object_predicate
    ON claim_versions(principal,object_node,predicate,state);

CREATE TABLE evidence (
    principal TEXT NOT NULL,
    claim_id TEXT NOT NULL,
    claim_revision INTEGER NOT NULL,
    source_id TEXT NOT NULL,
    source_revision INTEGER NOT NULL,
    byte_start INTEGER NOT NULL CHECK (byte_start >= 0),
    byte_end INTEGER NOT NULL CHECK (byte_end > byte_start),
    PRIMARY KEY (principal,claim_id,claim_revision,source_id,source_revision,byte_start,byte_end),
    FOREIGN KEY (principal,claim_id,claim_revision)
        REFERENCES claim_versions(principal,id,revision),
    FOREIGN KEY (principal,source_id,source_revision)
        REFERENCES sources(principal,id,revision)
) STRICT;
-- Span bounds, UTF-8 boundaries and entailment are checked by the writer;
-- a cross-row SQL CHECK cannot establish them.

CREATE TABLE dependencies (
    principal TEXT NOT NULL,
    derived_id TEXT NOT NULL,
    derived_revision INTEGER NOT NULL,
    premise_id TEXT NOT NULL,
    premise_revision INTEGER NOT NULL,
    PRIMARY KEY (principal,derived_id,derived_revision,premise_id,premise_revision),
    FOREIGN KEY (principal,derived_id,derived_revision)
        REFERENCES claim_versions(principal,id,revision),
    FOREIGN KEY (principal,premise_id,premise_revision)
        REFERENCES claim_versions(principal,id,revision)
) STRICT;
CREATE INDEX reverse_dependencies
    ON dependencies(principal,premise_id,premise_revision);

CREATE TABLE jobs (
    principal TEXT NOT NULL REFERENCES principals(id),
    id TEXT NOT NULL,
    job_key TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN
      ('ready','leased','deferred','retry_wait','succeeded','unresolved','obsolete','cancelled')),
    fence INTEGER NOT NULL DEFAULT 0 CHECK (fence >= 0),
    lease_instance TEXT,
    lease_until INTEGER,
    next_due INTEGER NOT NULL,
    PRIMARY KEY (principal,id),
    UNIQUE (principal,job_key),
    CHECK ((state = 'leased' AND lease_instance IS NOT NULL AND lease_until IS NOT NULL)
       OR (state <> 'leased' AND lease_instance IS NULL AND lease_until IS NULL))
) STRICT;
CREATE INDEX eligible_jobs ON jobs(principal,state,next_due,id);

CREATE TABLE change_events (
    principal TEXT NOT NULL REFERENCES principals(id),
    seq INTEGER NOT NULL CHECK (seq > 0),
    root_cause TEXT NOT NULL,
    kind TEXT NOT NULL,
    PRIMARY KEY (principal,seq)
) STRICT;
CREATE TABLE consumer_cursors (
    principal TEXT NOT NULL REFERENCES principals(id),
    consumer TEXT NOT NULL,
    seq INTEGER NOT NULL CHECK (seq >= 0),
    PRIMARY KEY (principal,consumer)
) STRICT;

CREATE TABLE notices (
    principal TEXT NOT NULL REFERENCES principals(id),
    id TEXT NOT NULL,
    cause_key TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    delivery TEXT NOT NULL CHECK (delivery IN ('queued','published','expired')),
    user_state TEXT NOT NULL CHECK (user_state IN ('unseen','seen','dismissed','snoozed')),
    PRIMARY KEY (principal,id),
    UNIQUE (principal,cause_key)
) STRICT;

CREATE TABLE profile_versions (
    principal TEXT NOT NULL REFERENCES principals(id),
    agent_id TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    generation INTEGER NOT NULL CHECK (generation > 0),
    profile_json TEXT NOT NULL CHECK (json_valid(profile_json)),
    PRIMARY KEY (principal,agent_id,revision)
) STRICT;
CREATE TABLE active_profiles (
    principal TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    revision INTEGER NOT NULL,
    PRIMARY KEY (principal,agent_id),
    FOREIGN KEY (principal,agent_id,revision)
        REFERENCES profile_versions(principal,agent_id,revision)
) STRICT;

CREATE TABLE search_documents (
    rowid INTEGER PRIMARY KEY,
    principal TEXT NOT NULL,
    source_id TEXT NOT NULL,
    source_revision INTEGER NOT NULL,
    text TEXT NOT NULL,
    FOREIGN KEY (principal,source_id,source_revision)
        REFERENCES sources(principal,id,revision),
    UNIQUE (principal,source_id,source_revision)
) STRICT;
CREATE VIRTUAL TABLE search_fts USING fts5(text, tokenize='unicode61');
-- Writer updates search_documents and search_fts with matching rowid in one
-- transaction. All reads join back to current principal/eligibility records.

CREATE TABLE forget_operations (
    principal TEXT NOT NULL REFERENCES principals(id),
    id TEXT NOT NULL,
    deletion_generation INTEGER NOT NULL CHECK (deletion_generation > 0),
    phase TEXT NOT NULL CHECK (phase IN
      ('suppressed','journaled','barrier_complete','purging','purged')),
    PRIMARY KEY (principal,id),
    UNIQUE (principal,deletion_generation)
) STRICT;
