CREATE TABLE memory_links (
 principal TEXT NOT NULL, from_id TEXT NOT NULL, from_revision INTEGER NOT NULL,
 to_id TEXT NOT NULL, to_revision INTEGER NOT NULL, relation TEXT NOT NULL,
 PRIMARY KEY(principal,from_id,to_id,relation),
 FOREIGN KEY(principal,from_id,from_revision) REFERENCES sources(principal,id,revision),
 FOREIGN KEY(principal,to_id,to_revision) REFERENCES sources(principal,id,revision)
) STRICT;
CREATE INDEX memory_links_incoming ON memory_links(principal,to_id);
CREATE TABLE note_dependencies (
 principal TEXT NOT NULL, note_id TEXT NOT NULL, source_id TEXT NOT NULL,
 PRIMARY KEY(principal,note_id,source_id)
) STRICT;
CREATE INDEX note_dependents ON note_dependencies(principal,source_id);
CREATE TABLE assistant_profile (
 principal TEXT PRIMARY KEY, revision INTEGER NOT NULL CHECK(revision>0),
 settings TEXT NOT NULL CHECK(json_valid(settings)), next_check_at INTEGER,
 last_check TEXT
) STRICT;
CREATE TABLE assistant_notices (
 principal TEXT NOT NULL, source_id TEXT NOT NULL, created_at INTEGER NOT NULL,
 dismissed INTEGER NOT NULL DEFAULT 0 CHECK(dismissed IN(0,1)),
 PRIMARY KEY(principal,source_id)
) STRICT;
