#!/usr/bin/env python3
"""Synthetic authenticated API workload for the exact schema-24 package harness."""
from __future__ import annotations

import concurrent.futures
import json
import sys
import time
import urllib.error
import urllib.request


BASE, CREDENTIAL_FILE, RECORDS_TEXT, CONCURRENCY_TEXT = sys.argv[1:]
with open(CREDENTIAL_FILE, "r", encoding="ascii") as credential_file:
    TOKEN = credential_file.read()
if not TOKEN or len(TOKEN) > 256:
    raise SystemExit("synthetic credential file invalid")
RECORDS = int(RECORDS_TEXT)
CONCURRENCY = int(CONCURRENCY_TEXT)
REGISTRY = "synthetic-registry"
ORGANIZATION = "synthetic-org"
SOURCES = ("synthetic-clinic-a", "synthetic-clinic-b", "synthetic-clinic-c")
NAMES = (
    ("Synthetic Lao Sample", "en", "Latn"),
    ("ຄຳທົດສອບ ສີວິໄລ", "lo", "Lao"),
    ("ตัวอย่าง สมชาย", "th", "Thai"),
    ("Nguyễn Minh An", "vi", "Latn"),
    ("आरव उदाहरण", "hi-IN", "Deva"),
)


def call(method: str, path: str, body: dict | None = None, key: str | None = None):
    payload = None if body is None else json.dumps(body, ensure_ascii=False, separators=(",", ":")).encode()
    headers = {"Authorization": f"Bearer {TOKEN}", "Accept": "application/json"}
    if payload is not None:
        headers["Content-Type"] = "application/json"
    if key is not None:
        headers["Idempotency-Key"] = key
    request = urllib.request.Request(BASE + path, data=payload, headers=headers, method=method)
    started = time.perf_counter()
    try:
        with urllib.request.urlopen(request, timeout=45) as response:
            raw = response.read()
            status = response.status
        parsed = json.loads(raw) if raw else {}
        return status, time.perf_counter() - started, parsed
    except urllib.error.HTTPError as exc:
        # Do not print response bodies; they can include synthetic source payloads.
        return exc.code, time.perf_counter() - started, {}
    except Exception:
        return 0, time.perf_counter() - started, {}


def report(label: str, results: list[tuple[int, float, dict]], ok_statuses: set[int], wall_seconds: float, workers: int):
    latencies = [elapsed * 1000 for status, elapsed, _ in results]
    good = [value for (status, _, _), value in zip(results, latencies) if status in ok_statuses]
    errors = len(results) - len(good)
    duration = wall_seconds
    ordered = sorted(good)

    def percentile(p: float) -> float:
        if not ordered:
            return 0.0
        # Hyndman-Fan type 7 / NumPy default: linearly interpolate between
        # adjacent order statistics at h=(n-1)*p. This keeps small samples,
        # especially the 30-request match phase, from using a lower-rank step.
        position = (len(ordered) - 1) * p
        lower = int(position)
        upper = min(lower + 1, len(ordered) - 1)
        fraction = position - lower
        return ordered[lower] + fraction * (ordered[upper] - ordered[lower])

    throughput = len(good) / duration if duration > 0 else 0.0
    print(
        f"LOAD_PHASE name={label} requests={len(results)} successful={len(good)} errors={errors} "
        f"duration_s={duration:.3f} throughput_rps={throughput:.2f} "
        f"latency_ms_p50={percentile(.50):.2f} latency_ms_p95={percentile(.95):.2f} "
        f"latency_ms_p99={percentile(.99):.2f} latency_ms_max={max(good, default=0):.2f} "
        f"concurrency={workers} retries=0 statuses={status_counts(results)}",
        flush=True,
    )
    return errors


def status_counts(results: list[tuple[int, float, dict]]) -> str:
    counts: dict[int, int] = {}
    for status, _, _ in results:
        counts[status] = counts.get(status, 0) + 1
    return ",".join(f"{code}:{counts[code]}" for code in sorted(counts))


def parallel(label: str, jobs, expected: set[int], workers: int = CONCURRENCY):
    started = time.perf_counter()
    with concurrent.futures.ThreadPoolExecutor(max_workers=workers) as executor:
        results = list(executor.map(lambda job: job(), jobs))
    # The phase wall clock measures aggregate throughput. Request latencies are
    # measured independently in `call`.
    wall = time.perf_counter() - started
    errors = report(label, results, expected, wall, workers)
    return results, errors, wall


def main() -> int:
    records = []
    jobs = []
    for index in range(RECORDS):
        group = index // 2
        name, language, script = NAMES[group % len(NAMES)]
        source_system = SOURCES[index % len(SOURCES)]
        key = f"synthetic-load-record-{index:06d}"
        payload = {
            "registry_id": REGISTRY,
            "organization_id": ORGANIZATION,
            "source_system_id": source_system,
            "source_record_key": key,
            "source_version": "v1",
            "assertions": [
                {"field_path": "name.original", "original_value": name, "language_tag": language, "script": script}
            ],
            "identifiers": [
                {"authority": "urn:litecr:synthetic-load", "identifier_type": "LOCAL", "value": f"SYN-{index:06d}", "authoritative": False}
            ],
        }
        record = {"key": key, "payload": payload, "idempotency_key": f"load-idem-{index:06d}"}
        records.append(record)
        jobs.append(lambda record=record: call("POST", "/api/v1/source-records", record["payload"], record["idempotency_key"]))

    first, errors, _ = parallel("ingestion_first_write", jobs, {202})
    if errors:
        print("LOAD_ABORT reason=first_write_errors; later phases omitted", flush=True)
        return 2
    for record, (_, _, response) in zip(records, first):
        record["source_record_id"] = response.get("data", {}).get("source_record_id")
        record["person_public_id"] = response.get("data", {}).get("person_public_id")
    if any(not record.get("source_record_id") or not record.get("person_public_id") for record in records):
        print("LOAD_ABORT reason=ingestion_response_shape", flush=True)
        return 2

    replay_jobs = [lambda record=record: call("POST", "/api/v1/source-records", record["payload"], record["idempotency_key"]) for record in records]
    _, errors, _ = parallel("ingestion_exact_idempotent_replay", replay_jobs, {200})
    if errors:
        print("LOAD_ABORT reason=replay_errors; later phases omitted", flush=True)
        return 2

    search_jobs = []
    for record in records:
        body = {"registry_id": REGISTRY, "organization_id": ORGANIZATION, "public_id_or_alias": record["person_public_id"], "limit": 1}
        search_jobs.append(lambda body=body: call("POST", "/api/v1/person-search", body))
    _, errors, _ = parallel("person_search_exact_public_id", search_jobs, {200})
    if errors:
        print("LOAD_ABORT reason=search_errors; later phases omitted", flush=True)
        return 2

    detail_jobs = []
    for record in records[: min(RECORDS, 100)]:
        path = f"/api/v1/registries/{REGISTRY}/organizations/{ORGANIZATION}/persons/{record['person_public_id']}"
        detail_jobs.append(lambda path=path: call("GET", path))
    _, errors, _ = parallel("person_detail", detail_jobs, {200})
    if errors:
        print("LOAD_ABORT reason=detail_errors; later phases omitted", flush=True)
        return 2

    match_records = records[: min(RECORDS, 30)]
    match_jobs = []
    for record in match_records:
        body = {
            "registry_id": REGISTRY,
            "organization_id": ORGANIZATION,
            "query_source_record_id": record["source_record_id"],
            "candidate_limit": 20,
        }
        match_jobs.append(lambda body=body: call("POST", "/api/v1/match", body))
    matches, errors, _ = parallel("match_evidence_generation", match_jobs, {200}, workers=min(4, CONCURRENCY))
    candidate_counts = []
    for _, _, response in matches:
        retrieval = response.get("data", {}).get("retrieval", {})
        if isinstance(retrieval.get("returned_candidates"), int):
            candidate_counts.append(retrieval["returned_candidates"])
    print(
        f"LOAD_DATASET source_records={RECORDS} distinct_source_keys={RECORDS} distinct_local_identifiers={RECORDS} "
        f"synthetic_name_groups={(RECORDS + 1) // 2} duplicate_name_records={RECORDS // 2} "
        f"name_locales=lo,en,th,vi,hi-IN match_requests={len(matches)} "
        f"match_returned_candidates_total={sum(candidate_counts)}",
        flush=True,
    )
    return 0 if errors == 0 else 2


if __name__ == "__main__":
    raise SystemExit(main())
