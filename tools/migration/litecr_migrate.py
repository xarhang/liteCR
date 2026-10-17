#!/usr/bin/env python3
"""Validate and stage liteCR migration/v1 packages without database side effects.

The importer deliberately stages exact source rows. Identity and relationship
changes are left to authorized Registry use cases in the application.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import sys
import tempfile
from typing import Any, BinaryIO

FORMAT = "litecr-migration/v1"
MAX_MANIFEST = 1024 * 1024
MAX_ROW = 1024 * 1024
# Bound the single-record quarantine buffer while still preserving complete
# physical-line hashes for mechanically repairable ROW_TOO_LARGE records.
MAX_PHYSICAL_ROW = 64 * 1024 * 1024
MAX_PACKAGE = 20 * 1024 * 1024 * 1024
MAX_BATCH_RECORDS = 500
MAX_BATCH_BYTES = 64 * 1024 * 1024
REQUIRED_MANIFEST = {
    "format", "snapshot_id", "source_system_id", "target_registry_id",
    "target_organization_id", "exported_at", "record_count", "stream_bytes",
    "stream_sha256",
}
RELATION_KINDS = {"link", "non_match"}
SUPPORTED_ROW_FIELDS = {
    "source_system_id", "source_record_key", "source_version", "public_id", "cruid",
    "aliases", "assertions", "identifiers", "relationships",
}


class PackageError(Exception):
    """Safe-to-report validation failure; never embeds source values."""


def _sha256_file(path: Path) -> tuple[str, int]:
    digest = hashlib.sha256()
    size = 0
    with path.open("rb") as stream:
        while chunk := stream.read(1024 * 1024):
            size += len(chunk)
            if size > MAX_PACKAGE:
                raise PackageError("PACKAGE_TOO_LARGE")
            digest.update(chunk)
    return digest.hexdigest(), size


def _nonempty_string(value: Any) -> bool:
    return isinstance(value, str) and bool(value.strip())


def _valid_row(row: Any, manifest: dict[str, Any]) -> list[str]:
    errors: list[str] = []
    if not isinstance(row, dict):
        return ["ROW_NOT_OBJECT"]
    if set(row) - SUPPORTED_ROW_FIELDS:
        errors.append("UNSUPPORTED_TOP_LEVEL_FIELD")
    if row.get("source_system_id") != manifest["source_system_id"]:
        errors.append("SOURCE_SYSTEM_MISMATCH")
    limits = {"source_system_id": 256, "source_record_key": 512, "source_version": 512}
    for key, limit in limits.items():
        if not _nonempty_string(row.get(key)) or len(row[key]) > limit:
            errors.append(f"INVALID_{key.upper()}")
    for key in ("public_id", "cruid"):
        if row.get(key) is not None and (not _nonempty_string(row[key]) or len(row[key]) > 512):
            errors.append(f"INVALID_{key.upper()}")
    aliases = row.get("aliases", [])
    if not isinstance(aliases, list):
        errors.append("ALIASES_NOT_ARRAY")
    elif len(aliases) > 256:
        errors.append("TOO_MANY_ALIASES")
    else:
        seen_aliases = {row.get("public_id"), row.get("cruid")} - {None}
        for alias in aliases:
            if not _nonempty_string(alias) or len(alias) > 512:
                errors.append("INVALID_ALIAS")
            elif alias in seen_aliases:
                errors.append("DUPLICATE_ALIAS")
            seen_aliases.add(alias)
    assertions = row.get("assertions", [])
    if not isinstance(assertions, list):
        errors.append("ASSERTIONS_NOT_ARRAY")
    elif not assertions:
        errors.append("NO_REPRESENTABLE_ASSERTION")
    elif len(assertions) > 256:
        errors.append("TOO_MANY_ASSERTIONS")
    else:
        seen_paths: set[str] = set()
        for assertion in assertions:
            if not isinstance(assertion, dict) or not _nonempty_string(assertion.get("field_path")) or len(assertion.get("field_path", "")) > 256 or not isinstance(assertion.get("original_value"), str) or len(assertion.get("original_value", "")) > 16384:
                errors.append("INVALID_ASSERTION")
                continue
            path = assertion["field_path"]
            if path in seen_paths:
                errors.append("DUPLICATE_ASSERTION_PATH")
            seen_paths.add(path)
            for field, limit in (("language_tag", 64), ("script", 32), ("partial_date_precision", 16)):
                value = assertion.get(field)
                if value is not None and (not isinstance(value, str) or len(value) > limit):
                    errors.append(f"INVALID_ASSERTION_{field.upper()}")
            precision = assertion.get("partial_date_precision")
            if precision is not None and precision not in {"unknown", "year", "month", "day"}:
                errors.append("INVALID_PARTIAL_DATE_PRECISION")
    identifiers = row.get("identifiers", [])
    if not isinstance(identifiers, list):
        errors.append("IDENTIFIERS_NOT_ARRAY")
    elif len(identifiers) > 64:
        errors.append("TOO_MANY_IDENTIFIERS")
    else:
        seen_identifiers: set[tuple[str, str, str]] = set()
        for identifier in identifiers:
            if not isinstance(identifier, dict) or not all(_nonempty_string(identifier.get(k)) for k in ("authority", "type", "value")) or not isinstance(identifier.get("authoritative", False), bool) or len(identifier.get("authority", "")) > 2048 or len(identifier.get("type", "")) > 128 or len(identifier.get("value", "")) > 512:
                errors.append("INVALID_IDENTIFIER")
                continue
            signature = (identifier["authority"], identifier["type"], identifier["value"])
            if signature in seen_identifiers:
                errors.append("DUPLICATE_IDENTIFIER")
            seen_identifiers.add(signature)
    relationships = row.get("relationships", [])
    if not isinstance(relationships, list):
        errors.append("RELATIONSHIPS_NOT_ARRAY")
    else:
        seen_relationships: set[tuple[str,str,str]] = set()
        for relationship in relationships:
            if not isinstance(relationship, dict) or relationship.get("kind") not in RELATION_KINDS or not all(_nonempty_string(relationship.get(k)) for k in ("target_source_system_id", "target_source_record_key")) or len(relationship.get("target_source_system_id", "")) > 256 or len(relationship.get("target_source_record_key", "")) > 512:
                errors.append("INVALID_RELATIONSHIP")
                continue
            if (relationship["target_source_system_id"], relationship["target_source_record_key"]) == (row.get("source_system_id"), row.get("source_record_key")):
                errors.append("SELF_RELATIONSHIP")
            signature = (relationship["kind"], relationship["target_source_system_id"], relationship["target_source_record_key"])
            if signature in seen_relationships:
                errors.append("DUPLICATE_RELATIONSHIP")
            seen_relationships.add(signature)
        if relationships:
            errors.append("HISTORICAL_RELATIONSHIPS_UNSUPPORTED")
    return errors


def _manifest(manifest_path: Path, stream_path: Path) -> tuple[dict[str, Any], str, str, int]:
    raw_manifest = manifest_path.read_bytes()
    if len(raw_manifest) > MAX_MANIFEST:
        raise PackageError("MANIFEST_TOO_LARGE")
    manifest_digest = hashlib.sha256(raw_manifest).hexdigest()
    try:
        manifest = json.loads(raw_manifest.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise PackageError("MANIFEST_INVALID_UTF8_OR_JSON") from exc
    if not isinstance(manifest, dict) or set(manifest) != REQUIRED_MANIFEST:
        raise PackageError("MANIFEST_FIELDS_INVALID")
    if manifest["format"] != FORMAT:
        raise PackageError("UNSUPPORTED_FORMAT")
    for key in ("snapshot_id", "source_system_id", "target_registry_id", "target_organization_id", "exported_at"):
        if not _nonempty_string(manifest[key]):
            raise PackageError(f"INVALID_MANIFEST_{key.upper()}")
    for key in ("record_count", "stream_bytes"):
        if not isinstance(manifest[key], int) or isinstance(manifest[key], bool) or manifest[key] < 0:
            raise PackageError(f"INVALID_MANIFEST_{key.upper()}")
    if manifest["stream_bytes"] > MAX_PACKAGE:
        raise PackageError("PACKAGE_TOO_LARGE")
    expected = manifest["stream_sha256"]
    if not isinstance(expected, str) or len(expected) != 64 or any(c not in "0123456789abcdef" for c in expected):
        raise PackageError("INVALID_STREAM_SHA256")
    actual, size = _sha256_file(stream_path)
    if size != manifest["stream_bytes"]:
        raise PackageError("STREAM_BYTE_COUNT_MISMATCH")
    if actual != expected:
        raise PackageError("STREAM_SHA256_MISMATCH")
    return manifest, manifest_digest, actual, size


def _canonical_digest(row: Any) -> str:
    encoded = json.dumps(row, sort_keys=True, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


def _canonical_numbers_supported(value: Any) -> bool:
    if isinstance(value, float):
        return False
    if type(value) is int and abs(value) > (2**53 - 1):
        return False
    if isinstance(value, list):
        return all(_canonical_numbers_supported(item) for item in value)
    if isinstance(value, dict):
        return all(_canonical_numbers_supported(item) for item in value.values())
    return True


def _open_index(path: Path) -> sqlite3.Connection:
    db = sqlite3.connect(path)
    db.execute("PRAGMA journal_mode=WAL")
    db.execute("PRAGMA synchronous=FULL")
    db.execute("CREATE TABLE IF NOT EXISTS rows (line INTEGER PRIMARY KEY, source_system TEXT, source_key TEXT, source_version TEXT, payload_digest TEXT, raw_line BLOB NOT NULL, status TEXT NOT NULL, error_code TEXT)")
    db.execute("CREATE UNIQUE INDEX IF NOT EXISTS source_version_key ON rows(source_system, source_key, source_version) WHERE status='valid'")
    db.execute("CREATE INDEX IF NOT EXISTS source_key_idx ON rows(source_system, source_key)")
    return db


def _validate_stream(stream_path: Path, manifest: dict[str, Any], index_path: Path) -> tuple[int, int, int]:
    db = _open_index(index_path)
    valid = quarantined = duplicates = 0
    try:
        with stream_path.open("rb") as stream:
            line_no = 0
            while raw_line := stream.readline(MAX_PHYSICAL_ROW + 1):
                line_no += 1
                if len(raw_line) > MAX_PHYSICAL_ROW:
                    raise PackageError("PHYSICAL_ROW_EXCEEDS_64_MIB_BOUND")
                if len(raw_line) > MAX_ROW + 1 or (len(raw_line) == MAX_ROW + 1 and not raw_line.endswith(b"\n")):
                    db.execute("INSERT OR REPLACE INTO rows VALUES (?,NULL,NULL,NULL,NULL,?,'quarantined','ROW_TOO_LARGE')", (line_no, raw_line))
                    quarantined += 1
                    continue
                if not raw_line.endswith(b"\n"):
                    # NDJSON requires one UTF-8 line terminator per record.
                    db.execute("INSERT OR REPLACE INTO rows VALUES (?,NULL,NULL,NULL,NULL,?,'quarantined','ROW_MISSING_LINE_FEED')", (line_no, raw_line))
                    quarantined += 1
                    continue
                body = raw_line[:-1]
                if body.endswith(b"\r"):
                    body = body[:-1]
                try:
                    row = json.loads(body.decode("utf-8"))
                except (UnicodeDecodeError, json.JSONDecodeError):
                    db.execute("INSERT OR REPLACE INTO rows VALUES (?,NULL,NULL,NULL,NULL,?,'quarantined','ROW_INVALID_UTF8_OR_JSON')", (line_no, raw_line))
                    quarantined += 1
                    continue
                if not _canonical_numbers_supported(row):
                    db.execute("INSERT INTO rows VALUES (?,NULL,NULL,NULL,NULL,?,'quarantined','NON_CANONICAL_NUMBER')", (line_no, raw_line))
                    quarantined += 1
                    continue
                errors = _valid_row(row, manifest)
                digest = _canonical_digest(row)
                if errors:
                    error = ",".join(sorted(set(errors)))
                    db.execute("INSERT INTO rows VALUES (?,?,?,?,?,?,'quarantined',?)", (line_no, row.get("source_system_id") if isinstance(row, dict) else None, row.get("source_record_key") if isinstance(row, dict) else None, row.get("source_version") if isinstance(row, dict) else None, digest, raw_line, error))
                    quarantined += 1
                    continue
                prior_rows = db.execute("SELECT line,payload_digest,status,raw_line FROM rows WHERE source_system=? AND source_key=? AND source_version=? AND payload_digest IS NOT NULL ORDER BY line", (row["source_system_id"], row["source_record_key"], row["source_version"])).fetchall()
                if prior_rows:
                    has_prior_conflict = any(status == "quarantined" or prior_digest != digest or bytes(prior_raw) != raw_line for _, prior_digest, status, prior_raw in prior_rows)
                    if not has_prior_conflict:
                        db.execute("INSERT INTO rows VALUES (?,?,?,?,?,?,'duplicate',?)", (line_no,row["source_system_id"],row["source_record_key"],row["source_version"],digest,raw_line,"EXACT_REPLAY"))
                        duplicates += 1
                    else:
                        transitioned = db.execute("UPDATE rows SET status='quarantined', error_code='SAME_VERSION_CHANGED_PAYLOAD' WHERE source_system=? AND source_key=? AND source_version=? AND payload_digest IS NOT NULL AND status IN ('valid','duplicate')", (row["source_system_id"],row["source_record_key"],row["source_version"])).rowcount
                        valid -= sum(1 for _, _, status, _ in prior_rows if status == "valid")
                        duplicates -= sum(1 for _, _, status, _ in prior_rows if status == "duplicate")
                        db.execute("INSERT INTO rows VALUES (?,?,?,?,?,?,'quarantined',?)", (line_no,row["source_system_id"],row["source_record_key"],row["source_version"],digest,raw_line,"SAME_VERSION_CHANGED_PAYLOAD"))
                        quarantined += transitioned + 1
                    continue
                db.execute("INSERT INTO rows VALUES (?,?,?,?,?,?,'valid',NULL)", (line_no,row["source_system_id"],row["source_record_key"],row["source_version"],digest,raw_line))
                valid += 1
                if line_no % 500 == 0:
                    db.commit()
        # Explicit relationships only: dangling/contradictory pairs are quarantined.
        # Establish target presence via the complete stream key index. No inferred edges.
        cursor = db.execute("SELECT line,raw_line FROM rows WHERE status='valid'")
        while chunk := cursor.fetchmany(256):
          for line, raw_line in chunk:
            row = json.loads(bytes(raw_line).decode("utf-8"))
            for rel in row.get("relationships", []):
                target = db.execute("SELECT 1 FROM rows WHERE status IN ('valid','duplicate') AND source_system=? AND source_key=? LIMIT 1", (rel["target_source_system_id"], rel["target_source_record_key"])).fetchone()
                if target is None:
                    db.execute("UPDATE rows SET status='quarantined',error_code='DANGLING_RELATIONSHIP' WHERE line=?", (line,))
                    quarantined += 1
                    valid -= 1
                    break
        db.commit()
        actual_count = line_no if 'line_no' in locals() else 0
        if actual_count != manifest["record_count"]:
            raise PackageError("RECORD_COUNT_MISMATCH")
        # Contradictory link/non-match facts for the same unordered pair quarantine both.
        pair_kinds: dict[tuple[tuple[str,str],tuple[str,str]], set[str]] = {}
        cursor = db.execute("SELECT line,raw_line FROM rows WHERE status='valid'")
        while chunk := cursor.fetchmany(256):
            for line, raw_line in chunk:
                row = json.loads(bytes(raw_line).decode("utf-8"))
                source = (row["source_system_id"], row["source_record_key"])
                for rel in row.get("relationships", []):
                    target = (rel["target_source_system_id"], rel["target_source_record_key"])
                    pair = tuple(sorted((source,target)))
                    pair_kinds.setdefault(pair,set()).add(rel["kind"])
        for pair, kinds in pair_kinds.items():
            if len(kinds) > 1:
                for source_system, source_key in pair:
                    changed = db.execute("UPDATE rows SET status='quarantined',error_code='CONTRADICTORY_RELATIONSHIP' WHERE status='valid' AND source_system=? AND source_key=?", (source_system,source_key)).rowcount
                    quarantined += changed
                    valid -= changed
        db.commit()
        return valid, quarantined, duplicates
    finally:
        db.close()


def _write_atomic(path: Path, contents: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temp_name = tempfile.mkstemp(prefix=f".{path.name}.", suffix=".tmp", dir=path.parent)
    try:
        with os.fdopen(fd, "wb") as output:
            output.write(contents)
            output.flush()
            os.fsync(output.fileno())
        os.replace(temp_name, path)
        try:
            directory_fd = os.open(path.parent, os.O_RDONLY)
            try:
                os.fsync(directory_fd)
            finally:
                os.close(directory_fd)
        except OSError:
            pass
    finally:
        if os.path.exists(temp_name):
            os.unlink(temp_name)


def validate(args: argparse.Namespace) -> int:
    manifest, manifest_digest, stream_digest, size = _manifest(args.manifest, args.stream)
    scratch = args.work_dir or Path(tempfile.mkdtemp(prefix="litecr-migration-validate-"))
    scratch.mkdir(parents=True, exist_ok=True)
    valid, quarantined, duplicates = _validate_stream(args.stream, manifest, scratch / "index.sqlite3")
    print(json.dumps({"manifest_sha256":manifest_digest,"stream_sha256":stream_digest,"stream_bytes":size,"records_valid":valid,"records_quarantined":quarantined,"exact_replay_duplicates":duplicates}, separators=(",",":")))
    return 0 if quarantined == 0 else 2


def stage(args: argparse.Namespace) -> int:
    manifest, manifest_digest, stream_digest, size = _manifest(args.manifest, args.stream)
    out = args.output
    out.mkdir(parents=True, exist_ok=True)
    try:
        os.chmod(out, 0o700)
    except OSError:
        pass
    checkpoint_path = out / "checkpoint.json"
    if checkpoint_path.exists():
        checkpoint = json.loads(checkpoint_path.read_text(encoding="utf-8"))
        if checkpoint.get("manifest_sha256") != manifest_digest or checkpoint.get("stream_sha256") != stream_digest:
            raise PackageError("CHECKPOINT_PACKAGE_MISMATCH")
        if checkpoint.get("batch_size") != args.batch_size or checkpoint.get("batch_bytes") != args.batch_bytes:
            raise PackageError("CHECKPOINT_BATCH_SIZE_MISMATCH")
    work = out / ".validation"
    work.mkdir(exist_ok=True)
    try:
        os.chmod(work, 0o700)
    except OSError:
        pass
    index = work / "index.sqlite3"
    if index.exists():
        index.unlink()
    valid_count, quarantine_count, duplicate_count = _validate_stream(args.stream, manifest, index)
    checkpoint = {"format":FORMAT,"state":"staging","manifest_sha256":manifest_digest,"stream_sha256":stream_digest,"stream_bytes":size,"record_count":manifest["record_count"],"valid_records":valid_count,"exact_replay_duplicates":duplicate_count,"quarantined_records":quarantine_count,"batch_size":args.batch_size,"batch_bytes":args.batch_bytes,"committed_batch_sequence":0,"staged_batch_sequence":0,"staged_records":0,"next_line":1}
    _write_atomic(checkpoint_path,(json.dumps(checkpoint,sort_keys=True,indent=2)+"\n").encode())
    db = sqlite3.connect(index)
    batch_seq = 0
    staged = 0
    batch = bytearray()
    batch_records = 0
    first_line = 0
    for line, raw_line in db.execute("SELECT line,raw_line FROM rows WHERE status='valid' ORDER BY line"):
        if batch_records and (batch_records >= args.batch_size or len(batch) + len(raw_line) > args.batch_bytes):
            batch_seq, staged = _persist_batch(out, manifest_digest, stream_digest, batch_seq, staged, first_line, prior_line, batch_records, batch)
            checkpoint.update({"staged_batch_sequence":batch_seq,"staged_records":staged,"next_line":prior_line+1})
            _write_atomic(checkpoint_path,(json.dumps(checkpoint,sort_keys=True,indent=2)+"\n").encode())
            batch = bytearray(); batch_records = 0
        if batch_records == 0:
            first_line = line
        if len(raw_line) > args.batch_bytes:
            raise PackageError("ROW_EXCEEDS_BATCH_BYTE_LIMIT")
        batch.extend(bytes(raw_line))
        batch_records += 1
        prior_line = line
        if batch_records >= args.batch_size or len(batch) >= args.batch_bytes:
            batch_seq, staged = _persist_batch(out, manifest_digest, stream_digest, batch_seq, staged, first_line, line, batch_records, batch)
            checkpoint.update({"staged_batch_sequence":batch_seq,"staged_records":staged,"next_line":line+1})
            _write_atomic(checkpoint_path,(json.dumps(checkpoint,sort_keys=True,indent=2)+"\n").encode())
            batch = bytearray(); batch_records = 0
    if batch_records:
        line = db.execute("SELECT max(line) FROM rows WHERE status='valid'").fetchone()[0]
        batch_seq, staged = _persist_batch(out, manifest_digest, stream_digest, batch_seq, staged, first_line, line, batch_records, batch)
        checkpoint.update({"staged_batch_sequence":batch_seq,"staged_records":staged,"next_line":line+1})
        _write_atomic(checkpoint_path,(json.dumps(checkpoint,sort_keys=True,indent=2)+"\n").encode())
    quarantine = []
    for line, code, raw_line in db.execute("SELECT line,error_code,raw_line FROM rows WHERE status='quarantined' ORDER BY line"):
        quarantine.append({"line":line,"code":code,"payload_sha256":hashlib.sha256(bytes(raw_line)).hexdigest()})
    _write_atomic(out / "quarantine.json", (json.dumps({"format":FORMAT,"manifest_sha256":manifest_digest,"exceptions":quarantine},ensure_ascii=False,sort_keys=True,indent=2)+"\n").encode())
    replays = []
    for line, source_system, source_key, source_version, raw_line in db.execute("SELECT line,source_system,source_key,source_version,raw_line FROM rows WHERE status='duplicate' ORDER BY line"):
        duplicate_of = db.execute("SELECT line FROM rows WHERE line<? AND status='valid' AND source_system=? AND source_key=? AND source_version=? ORDER BY line LIMIT 1", (line, source_system, source_key, source_version)).fetchone()
        if duplicate_of is None:
            raise PackageError("DUPLICATE_REPLAY_PROVENANCE_MISSING")
        replays.append({"line":line,"duplicate_of_line":duplicate_of[0],"code":"EXACT_REPLAY_DUPLICATE","payload_sha256":hashlib.sha256(bytes(raw_line)).hexdigest()})
    _require_exact_batch_artifacts(out, batch_seq)
    _write_atomic(out / "replays.json", (json.dumps({"format":FORMAT,"manifest_sha256":manifest_digest,"replays":replays},ensure_ascii=False,sort_keys=True,indent=2)+"\n").encode())
    checkpoint.update({"state":"staged","staged_batch_sequence":batch_seq,"staged_records":staged,"next_line":manifest["record_count"]+1})
    _write_atomic(checkpoint_path,(json.dumps(checkpoint,sort_keys=True,indent=2)+"\n").encode())
    db.close()
    print(json.dumps({"manifest_sha256":manifest_digest,"batches":batch_seq,"staged_records":staged,"exact_replay_duplicates":duplicate_count,"quarantined_records":quarantine_count,"checkpoint":str(checkpoint_path)},separators=(",",":")))
    return 0 if quarantine_count == 0 else 2


def _require_exact_batch_artifacts(out: Path, batch_seq: int) -> None:
    """Fail closed if an output directory contains an unjournaled batch artifact."""
    batches = out / "batches"
    expected = {
        f"{sequence:08}{suffix}"
        for sequence in range(1, batch_seq + 1)
        for suffix in (".ndjson", ".json")
    }
    if not batches.exists():
        if expected:
            raise PackageError("BATCH_ARTIFACTS_MISSING")
        return
    if not batches.is_dir():
        raise PackageError("BATCH_ARTIFACTS_INVALID")
    try:
        actual = {entry.name for entry in batches.iterdir()}
    except OSError as exc:
        raise PackageError("BATCH_ARTIFACTS_UNREADABLE") from exc
    if actual != expected:
        raise PackageError("UNEXPECTED_BATCH_ARTIFACT")


def _persist_batch(out: Path, manifest_digest: str, stream_digest: str, seq: int, staged: int, first_line: int, last_line: int, count: int, data: bytearray) -> tuple[int,int]:
    seq += 1
    batch_digest = hashlib.sha256(data).hexdigest()
    path = out / "batches" / f"{seq:08}.ndjson"
    meta = {"sequence":seq,"manifest_sha256":manifest_digest,"stream_sha256":stream_digest,"batch_sha256":batch_digest,"record_count":count,"first_input_line":first_line,"last_input_line":last_line,"status":"staged"}
    meta_path = path.with_suffix(".json")
    reusable = False
    if path.exists() and meta_path.exists():
        try:
            reusable = json.loads(meta_path.read_text(encoding="utf-8")) == meta and hashlib.sha256(path.read_bytes()).hexdigest() == batch_digest
        except (OSError, json.JSONDecodeError):
            reusable = False
    if not reusable:
        _write_atomic(path, bytes(data))
        _write_atomic(meta_path,(json.dumps(meta,sort_keys=True,indent=2)+"\n").encode())
    return seq, staged + count


def reverse_check(args: argparse.Namespace) -> int:
    """Fail closed if current source snapshots differ from importer journal entries."""
    current_by_key: dict[tuple[str,str], tuple[str,int]] = {}
    with args.current.open("rb") as stream:
        line_no = 0
        while raw := stream.readline(MAX_ROW + 2):
            line_no += 1
            if len(raw) > MAX_ROW or not raw.endswith(b"\n"):
                raise PackageError("CURRENT_SNAPSHOT_INVALID")
            row = json.loads(raw.decode("utf-8"))
            if not isinstance(row, dict) or not all(_nonempty_string(row.get(k)) for k in ("source_system_id","source_record_key","source_version")) or type(row.get("current_receive_version")) is not int or row["current_receive_version"] < 1:
                raise PackageError("CURRENT_SNAPSHOT_INVALID")
            key=(row["source_system_id"],row["source_record_key"])
            if key in current_by_key:
                raise PackageError("CURRENT_SNAPSHOT_DUPLICATE_SOURCE_KEY")
            payload = dict(row)
            payload.pop("current_receive_version", None)
            current_by_key[key]=( _canonical_digest(payload), row["current_receive_version"] )
    refused = 0
    checked = 0
    with args.journal.open("rb") as journal:
        for raw in journal:
            item=json.loads(raw.decode("utf-8")); checked += 1
            key=(item["source_system_id"],item["source_record_key"])
            current=current_by_key.get(key)
            if current is None or current != (item["expected_current_payload_sha256"],item["expected_receive_version"]):
                refused += 1
    print(json.dumps({"records_checked":checked,"records_refused":refused,"reverse_representable":refused==0},separators=(",",":")))
    return 0 if refused == 0 else 3


def parser() -> argparse.ArgumentParser:
    root=argparse.ArgumentParser(description=__doc__)
    sub=root.add_subparsers(dest="command",required=True)
    for name, handler in (("validate",validate),("stage",stage)):
        command=sub.add_parser(name)
        command.add_argument("--manifest",type=Path,required=True)
        command.add_argument("--stream",type=Path,required=True)
        if name=="validate":
            command.add_argument("--work-dir",type=Path)
        else:
            command.add_argument("--output",type=Path,required=True)
            command.add_argument("--batch-size",type=int,default=500)
            command.add_argument("--batch-bytes",type=int,default=MAX_BATCH_BYTES)
            command.set_defaults(handler=handler)
        if name=="validate": command.set_defaults(handler=handler)
    reverse=sub.add_parser("reverse-check")
    reverse.add_argument("--journal",type=Path,required=True)
    reverse.add_argument("--current",type=Path,required=True)
    reverse.set_defaults(handler=reverse_check)
    return root


def main() -> int:
    os.umask(0o077)
    args=parser().parse_args()
    if getattr(args,"batch_size",1) < 1 or getattr(args,"batch_size",1) > MAX_BATCH_RECORDS or getattr(args,"batch_bytes",1) < 1 or getattr(args,"batch_bytes",1) > MAX_BATCH_BYTES:
        raise PackageError("INVALID_BATCH_SIZE")
    try:
        return args.handler(args)
    except (PackageError, OSError, sqlite3.Error, json.JSONDecodeError) as exc:
        print(f"migration error: {exc if isinstance(exc, PackageError) else 'LOCAL_IO_OR_FORMAT_ERROR'}",file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
