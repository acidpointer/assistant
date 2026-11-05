#!/usr/bin/env python3
"""Validate research artifacts, not the unimplemented memory application.

The schema checker implements only the explicitly supported vocabulary below;
unknown schema keywords fail instead of silently receiving partial validation.
It is not a general JSON Schema implementation or a semantic entailment judge.
SQL tests exercise the documented constraint sketch in temporary memory stores.
"""

import json
from pathlib import Path
import re
import sqlite3


ROOT = Path(__file__).resolve().parents[1]
KEYWORDS = {
    "$schema", "title", "description", "type", "const", "enum", "properties",
    "required", "additionalProperties", "items", "minItems", "maxItems",
    "minLength", "maxLength", "pattern",
}


def check_vocabulary(schema):
    """Reject schema constructs this deliberately small checker cannot verify."""
    unknown = set(schema) - KEYWORDS
    if unknown:
        raise ValueError(f"unsupported schema keywords: {sorted(unknown)}")
    for child in schema.get("properties", {}).values():
        check_vocabulary(child)
    if "items" in schema:
        check_vocabulary(schema["items"])


def validate(value, schema, path="$"):
    """Apply the schema's supported structural constraints to one value."""
    types = {
        "object": isinstance(value, dict),
        "array": isinstance(value, list),
        "string": isinstance(value, str),
        "null": value is None,
        "integer": isinstance(value, int) and not isinstance(value, bool),
    }
    expected = schema.get("type")
    if isinstance(expected, str):
        expected = [expected]
    if expected and not any(types[kind] for kind in expected):
        raise ValueError(f"{path}: incorrect type")
    if "const" in schema and (
        value != schema["const"] or isinstance(value, bool) != isinstance(schema["const"], bool)
    ):
        raise ValueError(f"{path}: incorrect constant")
    if "enum" in schema and value not in schema["enum"]:
        raise ValueError(f"{path}: incorrect enum")
    if isinstance(value, dict):
        if not set(schema.get("required", [])).issubset(value):
            raise ValueError(f"{path}: missing required field")
        properties = schema.get("properties", {})
        if schema.get("additionalProperties") is False and set(value) - set(properties):
            raise ValueError(f"{path}: extra fields")
        for key, item in value.items():
            if key in properties:
                validate(item, properties[key], f"{path}.{key}")
    if isinstance(value, (str, list)):
        suffix = "Length" if isinstance(value, str) else "Items"
        if len(value) < schema.get("min" + suffix, 0):
            raise ValueError(f"{path}: too short")
        if len(value) > schema.get("max" + suffix, float("inf")):
            raise ValueError(f"{path}: too long")
    if isinstance(value, str) and "pattern" in schema:
        if not re.search(schema["pattern"], value):
            raise ValueError(f"{path}: pattern mismatch")
    if isinstance(value, list) and "items" in schema:
        for index, item in enumerate(value):
            validate(item, schema["items"], f"{path}[{index}]")


def check_examples():
    schema = json.loads((ROOT / "docs/memory-extraction.schema.json").read_text())
    examples = json.loads((ROOT / "docs/memory-contract-examples.json").read_text())
    check_vocabulary(schema)
    for case in examples["cases"]:
        try:
            validate(case["proposal"], schema)
        except ValueError as error:
            if case["expected"] != "reject_schema":
                raise AssertionError(f"unexpected rejection: {case['id']}") from error
        else:
            if case["expected"] == "reject_schema":
                raise AssertionError(f"invalid proposal accepted: {case['id']}")
    return len(examples["cases"])


def check_sql():
    db = sqlite3.connect(":memory:")
    db.executescript((ROOT / "docs/memory-schema.sql").read_text())
    rejected = 0

    def must_reject(sql, args=()):
        nonlocal rejected
        try:
            db.execute(sql, args)
        except sqlite3.IntegrityError:
            rejected += 1
        else:
            raise AssertionError(f"constraint did not reject: {sql}")

    for principal in ("owner", "other"):
        db.execute("INSERT INTO principals(id) VALUES (?)", (principal,))
        db.execute("INSERT INTO operations VALUES (?,?, 'web','request',NULL,'admitted')",
                   (principal, "op"))
    db.execute("INSERT INTO nodes VALUES ('owner','user','person')")
    db.execute("INSERT INTO nodes VALUES ('owner','claim','claim')")
    db.execute("INSERT INTO nodes VALUES ('other','foreign','person')")
    db.execute("INSERT INTO sources VALUES ('owner','source',1,'op','I prefer tea.',0)")
    db.execute("INSERT INTO sources VALUES ('other','foreign-source',1,'op','Private.',0)")
    insert_claim = "INSERT INTO claim_versions VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?)"
    claim = ("owner", "claim", 1, "user", "prefers", None, "tea", "positive",
             "asserted", "active", None, None, 1, None)
    db.execute(insert_claim, claim)
    must_reject("INSERT INTO operations VALUES ('owner','op2','web','request',NULL,'admitted')")
    must_reject(insert_claim, claim[:2] + (2,) + claim[3:])
    for index, value in ((3, "foreign"), (5, "foreign"), (6, None), (7, "maybe")):
        changed = list(claim)
        changed[2] = 2
        changed[index] = value
        changed[-1] = 3
        must_reject(insert_claim, changed)
    changed = list(claim)
    changed[2], changed[10], changed[11], changed[-1] = 2, 20, 10, 3
    must_reject(insert_claim, changed)
    db.execute("INSERT INTO evidence VALUES ('owner','claim',1,'source',1,0,13)")
    must_reject("INSERT INTO evidence VALUES ('owner','claim',1,'foreign-source',1,0,8)")
    must_reject("INSERT INTO evidence VALUES ('owner','claim',1,'source',1,8,3)")
    must_reject("INSERT INTO jobs VALUES ('owner','bad','bad','leased',0,NULL,NULL,0)")
    db.execute("INSERT INTO jobs VALUES ('owner','job','key','leased',2,'daemon',100,0)")
    result = db.execute("UPDATE jobs SET state='succeeded',lease_instance=NULL,lease_until=NULL "
                        "WHERE principal='owner' AND id='job' AND state='leased' AND fence=1")
    assert result.rowcount == 0, "stale attempt committed"
    result = db.execute("UPDATE jobs SET state='succeeded',lease_instance=NULL,lease_until=NULL "
                        "WHERE principal='owner' AND id='job' AND state='leased' AND fence=2")
    assert result.rowcount == 1, "current attempt failed"
    db.execute("INSERT INTO notices VALUES ('owner','notice','cause',1,'queued','unseen')")
    must_reject("INSERT INTO notices VALUES ('owner','notice2','cause',1,'queued','unseen')")
    must_reject("INSERT INTO active_profiles VALUES ('owner','agent',99)")
    db.commit()
    db.execute("BEGIN")
    db.execute("INSERT INTO profile_versions VALUES ('owner','agent',1,1,'{}')")
    db.execute("INSERT INTO active_profiles VALUES ('owner','agent',1)")
    db.rollback()
    assert db.execute("SELECT count(*) FROM profile_versions").fetchone()[0] == 0
    assert db.execute("SELECT count(*) FROM active_profiles").fetchone()[0] == 0
    db.execute("INSERT INTO search_documents VALUES (1,'owner','source',1,'tea')")
    db.execute("INSERT INTO search_fts(rowid,text) VALUES (1,'tea')")
    assert db.execute("SELECT rowid FROM search_fts WHERE search_fts MATCH 'tea'").fetchall() == [(1,)]
    db.execute("DELETE FROM search_fts WHERE rowid=1")
    db.execute("DELETE FROM search_documents WHERE rowid=1")
    assert not db.execute("SELECT rowid FROM search_fts WHERE search_fts MATCH 'tea'").fetchall()
    assert not db.execute("PRAGMA foreign_key_check").fetchall()
    assert db.execute("PRAGMA integrity_check").fetchone()[0] == "ok"
    db.close()
    return rejected


if __name__ == "__main__":
    count = check_examples()
    rejected = check_sql()
    print(f"PASS: {count} schema examples; {rejected} SQL constraint rejections; "
          "fenced completion, rollback, FTS deletion and integrity checks.")
    print(f"SQLite {sqlite3.sqlite_version}; no Rust/MSRV, semantic-model, power-loss or end-to-end verification.")
