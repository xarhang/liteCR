#!/usr/bin/env python3
"""Prepare immutable, auditable migration quarantine dispositions offline.

This tool never accesses a database. It writes a new derived package and a
hash-only request for the protected migration API; it never edits source input.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import sys
import tempfile
from typing import Any

from litecr_migrate import FORMAT, MAX_PACKAGE, MAX_ROW, PackageError, _valid_row, _write_atomic

MAX_DISPOSITION_SOURCE_ROW = 64 * 1024 * 1024

MECHANICAL_CODES = {
    "ROW_INVALID_UTF8_OR_JSON",
    "ROW_MISSING_LINE_FEED",
    "ROW_TOO_LARGE",
    "NON_CANONICAL_NUMBER",
}


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def read_json(path: Path, maximum: int = 16 * 1024 * 1024) -> Any:
    data = path.read_bytes()
    if len(data) > maximum:
        raise PackageError("DISPOSITION_INPUT_TOO_LARGE")
    return json.loads(data.decode("utf-8"))


def iter_stream(stream_path: Path):
    with stream_path.open("rb") as stream:
        while True:
            raw = stream.readline(MAX_DISPOSITION_SOURCE_ROW + 1)
            if not raw:
                break
            # A bounded read that reaches EOF without LF is one physical final
            # row. The caller/stager records ROW_MISSING_LINE_FEED for it; a
            # derivative writer must still emit canonical terminated NDJSON.
            if len(raw) > MAX_DISPOSITION_SOURCE_ROW:
                raise PackageError("SOURCE_LINE_EXCEEDS_DISPOSITION_BOUND")
            yield raw


def load_package(manifest_path: Path, stream_path: Path) -> tuple[dict[str, Any], str]:
    manifest_bytes = manifest_path.read_bytes()
    stream_size = stream_path.stat().st_size
    if len(manifest_bytes) > 1024 * 1024 or stream_size > MAX_PACKAGE:
        raise PackageError("PACKAGE_TOO_LARGE")
    manifest = json.loads(manifest_bytes.decode("utf-8"))
    fields = {
        "format", "snapshot_id", "source_system_id", "target_registry_id",
        "target_organization_id", "exported_at", "record_count", "stream_bytes",
        "stream_sha256",
    }
    if not isinstance(manifest, dict) or set(manifest) != fields or manifest.get("format") != FORMAT:
        raise PackageError("MANIFEST_INVALID")
    stream_hasher = hashlib.sha256()
    line_count = 0
    for raw in iter_stream(stream_path):
        line_count += 1
        stream_hasher.update(raw)
    if manifest["stream_bytes"] != stream_size or manifest["stream_sha256"] != stream_hasher.hexdigest():
        raise PackageError("STREAM_DIGEST_MISMATCH")
    if line_count != manifest["record_count"]:
        raise PackageError("RECORD_COUNT_MISMATCH")
    return manifest, digest(manifest_bytes)


def line_evidence(stream_path: Path, wanted: set[int]) -> dict[int, tuple[str, int, int]]:
    result: dict[int, tuple[str, int, int]] = {}
    offset = 0
    for number, raw in enumerate(iter_stream(stream_path), 1):
        if number in wanted:
            result[number] = (digest(raw), len(raw), offset)
        offset += len(raw)
    if result.keys() != wanted:
        raise PackageError("DISPOSITION_LINE_NOT_IN_SOURCE")
    return result


def stage_evidence(stage_dir: Path, manifest_digest: str) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    checkpoint = read_json(stage_dir / "checkpoint.json", 1024 * 1024)
    if checkpoint.get("manifest_sha256") != manifest_digest or checkpoint.get("state") != "staged":
        raise PackageError("STAGE_CHECKPOINT_MISMATCH")
    quarantine = read_json(stage_dir / "quarantine.json")
    if quarantine.get("manifest_sha256") != manifest_digest or quarantine.get("format") != FORMAT:
        raise PackageError("QUARANTINE_REPORT_MISMATCH")
    replays_path = stage_dir / "replays.json"
    replays = read_json(replays_path) if replays_path.exists() else {
        "format": FORMAT, "manifest_sha256": manifest_digest, "replays": []
    }
    if replays.get("manifest_sha256") != manifest_digest or replays.get("format") != FORMAT:
        raise PackageError("REPLAY_REPORT_MISMATCH")
    return quarantine.get("exceptions", []), replays.get("replays", [])


def prepare(args: argparse.Namespace) -> int:
    manifest, parent_digest = load_package(args.manifest, args.stream)
    if args.output.resolve() in {args.manifest.resolve().parent, args.stream.resolve().parent}:
        raise PackageError("OUTPUT_MUST_BE_SEPARATE_FROM_SOURCE")
    if args.output.is_symlink() or (args.output.exists() and not args.output.is_dir()):
        raise PackageError("OUTPUT_DIRECTORY_UNSAFE")
    if args.output.exists() and any(args.output.iterdir()):
        raise PackageError("OUTPUT_DIRECTORY_NOT_EMPTY")
    args.output.mkdir(mode=0o700, parents=True, exist_ok=True)
    exceptions, replays = stage_evidence(args.stage_dir, parent_digest)
    action = args.action
    item_evidence: list[dict[str, Any]] = []
    replacement_digest: str | None = None

    if action == "deferred":
        if not exceptions:
            raise PackageError("NO_QUARANTINE_TO_DEFER")
        if args.corrections:
            raise PackageError("DEFERRED_ACTION_DOES_NOT_ACCEPT_CORRECTIONS")
        for item in exceptions:
            line = item.get("line")
            if not isinstance(line, int) or line < 1:
                raise PackageError("QUARANTINE_LINE_INVALID")
        evidence = line_evidence(args.stream, {item["line"] for item in exceptions})
        for item in exceptions:
            line = item["line"]
            actual_hash, _, _ = evidence[line]
            if item.get("payload_sha256") != actual_hash:
                raise PackageError("QUARANTINE_ROW_DIGEST_MISMATCH")
            item_evidence.append({
                "input_line": line,
                "duplicate_of_line": None,
                "quarantine_code": item["code"],
                "original_payload_sha256": actual_hash,
                "corrected_payload_sha256": None,
            })

    elif action == "corrected_resubmission":
        if not exceptions:
            raise PackageError("CORRECTION_REQUIRES_QUARANTINE")
        corrections = read_json(args.corrections)
        if not isinstance(corrections, list) or len(corrections) != len(exceptions):
            raise PackageError("CORRECTION_SET_MUST_COVER_ALL_QUARANTINES")
        exception_by_line = {item["line"]: item for item in exceptions}
        corrected_lines: dict[int, bytes] = {}
        original_evidence = line_evidence(args.stream, set(exception_by_line))
        for item in corrections:
            if not isinstance(item, dict) or set(item) != {"line", "row"}:
                raise PackageError("CORRECTION_ENTRY_INVALID")
            line, row = item["line"], item["row"]
            exception = exception_by_line.get(line)
            if exception is None or exception.get("code") not in MECHANICAL_CODES:
                raise PackageError("QUARANTINE_CODE_REQUIRES_DEFERRED_REVIEW")
            original_hash, original_size, original_offset = original_evidence[line]
            raw_original_bytes = read_line_at(args.stream, original_offset, original_size)
            if exception.get("payload_sha256") != original_hash:
                raise PackageError("QUARANTINE_ROW_DIGEST_MISMATCH")
            if not isinstance(row, dict) or _valid_row(row, manifest):
                raise PackageError("CORRECTED_ROW_INVALID")
            if row.get("relationships"):
                raise PackageError("HISTORICAL_RELATIONSHIPS_UNSUPPORTED")
            if len(exception_by_line) != len(corrections):
                raise PackageError("CORRECTION_SET_MUST_COVER_ALL_QUARANTINES")
            corrected = (json.dumps(row, ensure_ascii=False, sort_keys=True, separators=(",", ":")) + "\n").encode("utf-8")
            if len(corrected) > MAX_ROW or corrected == raw_original_bytes:
                raise PackageError("CORRECTED_ROW_INVALID")
            corrected_lines[line] = corrected
            item_evidence.append({
                "input_line": line,
                "duplicate_of_line": None,
                "quarantine_code": exception["code"],
                "original_payload_sha256": original_hash,
                "corrected_payload_sha256": digest(corrected),
            })
        remove_lines = set()
        if replays:
            replay_evidence = line_evidence(
                args.stream,
                {item["line"] for item in replays}
                | {item["duplicate_of_line"] for item in replays},
            )
            for replay in replays:
                line = replay.get("line")
                first_line = replay.get("duplicate_of_line")
                if not isinstance(line, int) or not isinstance(first_line, int) or not (1 <= first_line < line):
                    raise PackageError("REPLAY_PROVENANCE_INVALID")
                duplicate_hash, duplicate_size, _ = replay_evidence[line]
                first_hash, first_size, _ = replay_evidence[first_line]
                if replay.get("code") != "EXACT_REPLAY_DUPLICATE" or replay.get("payload_sha256") != duplicate_hash or duplicate_hash != first_hash or duplicate_size != first_size:
                    raise PackageError("REPLAY_BYTES_NOT_IDENTICAL")
                if first_line in remove_lines:
                    raise PackageError("REPLAY_TARGET_IS_REMOVED")
                remove_lines.add(line)
                item_evidence.append({
                    "input_line": line,
                    "duplicate_of_line": first_line,
                    "quarantine_code": "EXACT_REPLAY_DUPLICATE",
                    "original_payload_sha256": duplicate_hash,
                    "corrected_payload_sha256": duplicate_hash,
                })
        derived = (
            corrected_lines.get(number, raw)
            for number, raw in enumerate(iter_stream(args.stream), 1)
            if number not in remove_lines
        )
        replacement_digest = write_derived(args.output, manifest, derived, manifest["record_count"] - len(remove_lines))

    elif action == "duplicate_acknowledged":
        if exceptions or not replays or args.corrections:
            raise PackageError("DUPLICATE_ACK_REQUIRES_REPLAYS_ONLY")
        remove_lines = set()
        replay_lines = {item["line"] for item in replays}
        wanted = replay_lines | {item["duplicate_of_line"] for item in replays}
        evidence = line_evidence(args.stream, wanted)
        for replay in replays:
            line = replay.get("line")
            first_line = replay.get("duplicate_of_line")
            if not isinstance(line, int) or not isinstance(first_line, int) or not (1 <= first_line < line):
                raise PackageError("REPLAY_PROVENANCE_INVALID")
            duplicate_hash, duplicate_size, _ = evidence[line]
            first_hash, first_size, _ = evidence[first_line]
            if replay.get("code") != "EXACT_REPLAY_DUPLICATE" or replay.get("payload_sha256") != duplicate_hash or duplicate_hash != first_hash or duplicate_size != first_size:
                raise PackageError("REPLAY_BYTES_NOT_IDENTICAL")
            if first_line in remove_lines:
                raise PackageError("REPLAY_TARGET_IS_REMOVED")
            remove_lines.add(line)
            item_evidence.append({
                "input_line": line,
                "duplicate_of_line": first_line,
                "quarantine_code": "EXACT_REPLAY_DUPLICATE",
                "original_payload_sha256": duplicate_hash,
                "corrected_payload_sha256": duplicate_hash,
            })
        def retained_lines():
            for number, raw in enumerate(iter_stream(args.stream), 1):
                if number not in remove_lines:
                    yield raw
        replacement_digest = write_derived(args.output, manifest, retained_lines(), manifest["record_count"] - len(remove_lines))
    else:
        raise PackageError("DISPOSITION_ACTION_INVALID")

    item_evidence.sort(key=lambda item: item["input_line"])
    request = {
        "action": action,
        "parent_manifest_sha256": parent_digest,
        "replacement_manifest_sha256": replacement_digest,
        "registry_id": manifest["target_registry_id"],
        "organization_id": manifest["target_organization_id"],
        "reason": args.reason,
        "idempotency_key": args.idempotency_key,
        "corrections": item_evidence,
    }
    request_path = args.output / "disposition-request.json"
    _write_atomic(request_path, (json.dumps(request, ensure_ascii=False, sort_keys=True, indent=2) + "\n").encode())
    if os.name != "nt":
        request_path.chmod(0o600)
    print(json.dumps({"action": action, "parent_manifest_sha256": parent_digest, "replacement_manifest_sha256": replacement_digest, "correction_count": len(item_evidence), "request_file": str(request_path)}, separators=(",", ":")))
    return 0


def read_line_at(stream_path: Path, offset: int, length: int) -> bytes:
    with stream_path.open("rb") as stream:
        stream.seek(offset)
        raw = stream.read(length)
    if len(raw) != length:
        raise PackageError("DISPOSITION_LINE_NOT_IN_SOURCE")
    return raw


def write_derived(output: Path, parent: dict[str, Any], lines, record_count: int | None = None) -> str:
    if output.exists() and (output.is_symlink() or not output.is_dir()):
        raise PackageError("OUTPUT_DIRECTORY_UNSAFE")
    output.mkdir(mode=0o700, parents=True, exist_ok=True)
    if os.name != "nt":
        output.chmod(0o700)
    stream_path = output / "records.ndjson"
    stream_hasher = hashlib.sha256()
    stream_size = 0
    actual_count = 0
    descriptor, temporary_path = tempfile.mkstemp(prefix=".records.", suffix=".tmp", dir=output)
    try:
        with os.fdopen(descriptor, "wb") as stream:
            for raw in lines:
                stream.write(raw)
                stream_hasher.update(raw)
                stream_size += len(raw)
                actual_count += 1
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary_path, stream_path)
        if os.name != "nt":
            directory_fd = os.open(output, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
            try:
                os.fsync(directory_fd)
            finally:
                os.close(directory_fd)
    except Exception:
        try:
            os.unlink(temporary_path)
        except OSError:
            pass
        raise
    manifest = dict(parent)
    manifest["record_count"] = record_count if record_count is not None else actual_count
    if actual_count != manifest["record_count"]:
        raise PackageError("DERIVED_RECORD_COUNT_MISMATCH")
    manifest["stream_bytes"] = stream_size
    manifest["stream_sha256"] = stream_hasher.hexdigest()
    manifest_bytes = (json.dumps(manifest, ensure_ascii=False, sort_keys=True, indent=2) + "\n").encode()
    _write_atomic(output / "manifest.json", manifest_bytes)
    if os.name != "nt":
        (output / "records.ndjson").chmod(0o600)
        (output / "manifest.json").chmod(0o600)
    return digest(manifest_bytes)


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    command = root.add_subparsers(dest="command", required=True).add_parser("prepare")
    command.add_argument("--action", choices=("corrected_resubmission", "deferred", "duplicate_acknowledged"), required=True)
    command.add_argument("--manifest", type=Path, required=True)
    command.add_argument("--stream", type=Path, required=True)
    command.add_argument("--stage-dir", type=Path, required=True)
    command.add_argument("--output", type=Path, required=True)
    command.add_argument("--reason", required=True)
    command.add_argument("--idempotency-key", required=True)
    command.add_argument("--corrections", type=Path)
    command.set_defaults(handler=prepare)
    return root


def main() -> int:
    os.umask(0o077)
    args = parser().parse_args()
    if not args.reason.strip() or len(args.reason) > 2000 or not args.idempotency_key.strip() or len(args.idempotency_key) > 256:
        print("migration error: DISPOSITION_METADATA_INVALID", file=sys.stderr)
        return 1
    try:
        return args.handler(args)
    except (PackageError, OSError, ValueError, TypeError, KeyError, json.JSONDecodeError):
        print("migration error: DISPOSITION_INPUT_INVALID", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
