//! Versioned synthetic label-blind candidate export contract.
//!
//! This module accepts only opaque IDs and operational matcher metadata. The
//! opaque-ID mapping must remain in the protected evaluation workspace.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_EXPORT_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_QUERY_MANIFEST_BYTES: usize = 1024 * 1024;
const MAX_QUERIES: usize = 10_000;
const MAX_CANDIDATES_PER_QUERY: usize = 2_000;
const MAX_CANDIDATE_COUNT_PER_QUERY: usize = 1_000_000;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ExportBody {
    schema_version: String,
    run_id: String,
    system_version: String,
    config_digest: String,
    queries: Vec<QueryOutput>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Export {
    body: ExportBody,
    export_digest: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct QueryOutput {
    query_id: String,
    status: String,
    candidate_count: usize,
    truncated: bool,
    elapsed_microseconds: Option<u64>,
    candidates: Vec<Candidate>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Candidate {
    candidate_id: String,
    rank: usize,
}

#[derive(Debug, Serialize)]
struct SafeReport {
    report_version: &'static str,
    export_digest: String,
    query_manifest_digest: String,
    system_version: String,
    config_digest: String,
    queries: usize,
    complete_queries: usize,
    failed_queries: usize,
    unavailable_queries: usize,
    truncated_queries: usize,
    returned_candidates: usize,
    total_candidates: usize,
    latency_observations: usize,
    elapsed_microseconds_total: Option<u64>,
    claim_boundary: &'static str,
}

/// Validates exact query coverage against an independently supplied label-blind
/// manifest and returns aggregate-only output. Expected IDs must also be opaque.
pub fn validate_json(bytes: &[u8], expected_query_ids: &[String]) -> Result<String, String> {
    if bytes.len() > MAX_EXPORT_BYTES {
        return Err("export exceeds 16 MiB size limit".into());
    }
    if expected_query_ids.len() > MAX_QUERIES {
        return Err("query manifest exceeds 10000-query limit".into());
    }
    let export: Export = serde_json::from_slice(bytes)
        .map_err(|_| "invalid export JSON or unsupported fields".to_owned())?;
    validate(&export, expected_query_ids)?;
    let mut counts = BTreeMap::<&str, usize>::new();
    let mut returned = 0usize;
    let mut total = 0usize;
    let mut truncated = 0usize;
    let mut latency_count = 0usize;
    let mut latency_sum = 0u64;
    for query in &export.body.queries {
        let status_count = counts.entry(query.status.as_str()).or_default();
        checked_add(status_count, 1)?;
        checked_add(&mut returned, query.candidates.len())?;
        checked_add(&mut total, query.candidate_count)?;
        checked_add(&mut truncated, usize::from(query.truncated))?;
        if let Some(elapsed) = query.elapsed_microseconds {
            checked_add(&mut latency_count, 1)?;
            latency_sum = latency_sum
                .checked_add(elapsed)
                .ok_or_else(|| "aggregate latency overflow".to_owned())?;
        }
    }
    let report = SafeReport {
        report_version: "litecr-label-blind-export-report-v1",
        export_digest: export.export_digest,
        query_manifest_digest: query_manifest_digest(expected_query_ids),
        system_version: export.body.system_version,
        config_digest: export.body.config_digest,
        queries: export.body.queries.len(),
        complete_queries: counts.get("complete").copied().unwrap_or(0),
        failed_queries: counts.get("failed").copied().unwrap_or(0),
        unavailable_queries: counts.get("unavailable").copied().unwrap_or(0),
        truncated_queries: truncated,
        returned_candidates: returned,
        total_candidates: total,
        latency_observations: latency_count,
        elapsed_microseconds_total: (latency_count > 0).then_some(latency_sum),
        claim_boundary: "label-blind synthetic export contract validation only; no matching accuracy or superiority claim",
    };
    serde_json::to_string_pretty(&report)
        .map_err(|_| "could not serialize redacted export report".to_owned())
}

/// Binds the aggregate report to the semantically unordered opaque query set
/// that was checked, without disclosing any query ID in the report.
fn query_manifest_digest(expected_query_ids: &[String]) -> String {
    let mut ids: Vec<_> = expected_query_ids.iter().map(String::as_str).collect();
    ids.sort_unstable();
    let mut hash = Sha256::new();
    hash.update(b"litecr-label-blind-query-manifest-v1\0");
    for id in ids {
        hash.update((id.len() as u64).to_be_bytes());
        hash.update(id.as_bytes());
    }
    format_digest(hash.finalize().as_slice())
}

/// Builds an export with a digest bound to its serialized body.
pub fn build_json(body_json: &[u8], expected_query_ids: &[String]) -> Result<String, String> {
    if body_json.len() > MAX_EXPORT_BYTES {
        return Err("export body exceeds 16 MiB size limit".into());
    }
    if expected_query_ids.len() > MAX_QUERIES {
        return Err("query manifest exceeds 10000-query limit".into());
    }
    let body: ExportBody = serde_json::from_slice(body_json)
        .map_err(|_| "invalid export body JSON or unsupported fields".to_owned())?;
    let body_bytes =
        serde_json::to_vec(&body).map_err(|_| "could not encode export body".to_owned())?;
    let export = Export {
        body,
        export_digest: digest(&body_bytes),
    };
    validate(&export, expected_query_ids)?;
    let encoded = serde_json::to_string_pretty(&export)
        .map_err(|_| "could not serialize export".to_owned())?;
    if encoded.len() > MAX_EXPORT_BYTES {
        return Err("encoded export exceeds 16 MiB size limit".into());
    }
    Ok(encoded)
}

fn validate(export: &Export, expected_query_ids: &[String]) -> Result<(), String> {
    let body = &export.body;
    if body.schema_version != "1.0" {
        return Err("unsupported export schema_version".into());
    }
    if !opaque_id(&body.run_id, "run_") {
        return Err("run_id must be an opaque token".into());
    }
    if !version_token(&body.system_version) {
        return Err("system_version must be a safe version token".into());
    }
    if !sha256_digest(&body.config_digest) {
        return Err("config_digest must be a sha256 digest".into());
    }
    if !sha256_digest(&export.export_digest) {
        return Err("export_digest must be a sha256 digest".into());
    }
    let body_bytes =
        serde_json::to_vec(body).map_err(|_| "could not encode export body".to_owned())?;
    if digest(&body_bytes) != export.export_digest {
        return Err("export_digest does not match export body".into());
    }
    let expected: BTreeSet<_> = expected_query_ids.iter().map(String::as_str).collect();
    if expected.len() != expected_query_ids.len() || expected.iter().any(|id| !opaque_id(id, "q_"))
    {
        return Err("expected query manifest must contain unique opaque IDs".into());
    }
    if body.queries.len() > MAX_QUERIES {
        return Err("export exceeds 10000-query limit".into());
    }
    let mut seen_queries = BTreeSet::new();
    for query in &body.queries {
        if !opaque_id(&query.query_id, "q_") || !seen_queries.insert(query.query_id.as_str()) {
            return Err("query IDs must be unique opaque tokens".into());
        }
        if !expected.contains(query.query_id.as_str()) {
            return Err("export contains query outside expected manifest".into());
        }
        match query.status.as_str() {
            "complete" | "failed" | "unavailable" => {}
            _ => return Err("query status is unsupported".into()),
        }
        if query.status != "complete"
            && (!query.candidates.is_empty() || query.candidate_count != 0 || query.truncated)
        {
            return Err("non-complete query cannot claim candidate output".into());
        }
        if query.candidate_count < query.candidates.len()
            || query.truncated != (query.candidate_count > query.candidates.len())
        {
            return Err("candidate count and truncation do not match returned rows".into());
        }
        if query.candidates.len() > MAX_CANDIDATES_PER_QUERY {
            return Err("query exceeds 2000 returned-candidate limit".into());
        }
        if query.candidate_count > MAX_CANDIDATE_COUNT_PER_QUERY {
            return Err("query exceeds 1000000 total-candidate limit".into());
        }
        let mut candidate_ids = BTreeSet::new();
        let mut ranks = BTreeSet::new();
        for (index, candidate) in query.candidates.iter().enumerate() {
            if !opaque_id(&candidate.candidate_id, "c_")
                || !candidate_ids.insert(candidate.candidate_id.as_str())
            {
                return Err("candidate IDs must be unique opaque tokens per query".into());
            }
            let rank = index + 1;
            if candidate.rank != rank || !ranks.insert(candidate.rank) {
                return Err("candidate ranks must be unique and contiguous from one".into());
            }
        }
    }
    if seen_queries != expected {
        return Err("export query coverage does not match expected manifest".into());
    }
    Ok(())
}

fn opaque_id(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|suffix| {
        (16..=64).contains(&suffix.len())
            && suffix
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    })
}

fn version_token(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && bytes[0].is_ascii_alphanumeric()
        && bytes[bytes.len() - 1].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._+-".contains(byte))
}

fn checked_add(target: &mut usize, value: usize) -> Result<(), String> {
    *target = target
        .checked_add(value)
        .ok_or_else(|| "aggregate count overflow".to_owned())?;
    Ok(())
}

fn sha256_digest(value: &str) -> bool {
    value
        .strip_prefix("sha256:")
        .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn digest(bytes: &[u8]) -> String {
    format_digest(&Sha256::digest(bytes))
}

fn format_digest(bytes: &[u8]) -> String {
    let mut out = String::from("sha256:");
    for byte in bytes {
        use std::fmt::Write;
        write!(&mut out, "{byte:02x}").expect("writing to string");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    const Q1: &str = "q_0123456789abcdef";
    const Q2: &str = "q_fedcba9876543210";
    const C1: &str = "c_0123456789abcdef";
    const C2: &str = "c_fedcba9876543210";
    const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn body() -> Value {
        json!({"schema_version":"1.0", "run_id":"run_0123456789abcdef", "system_version":"litecr-matcher-v1", "config_digest":DIGEST,
        "queries":[
          {"query_id":Q1,"status":"complete","candidate_count":2,"truncated":false,"elapsed_microseconds":17,
           "candidates":[{"candidate_id":C1,"rank":1},{"candidate_id":C2,"rank":2}]},
          {"query_id":Q2,"status":"unavailable","candidate_count":0,"truncated":false,"elapsed_microseconds":null,"candidates":[]}
        ]})
    }
    fn expected() -> Vec<String> {
        vec![Q1.into(), Q2.into()]
    }
    fn export(v: Value) -> String {
        build_json(v.to_string().as_bytes(), &expected()).unwrap()
    }
    fn validate(v: Value) -> Result<String, String> {
        validate_json(seal(v).as_bytes(), &expected())
    }
    fn seal(v: Value) -> String {
        let body: ExportBody = serde_json::from_value(v).unwrap();
        let body_bytes = serde_json::to_vec(&body).unwrap();
        serde_json::to_string(&Export {
            export_digest: digest(&body_bytes),
            body,
        })
        .unwrap()
    }

    #[test]
    fn exact_query_coverage_and_redacted_report() {
        let output = validate(body()).unwrap();
        let report: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(report["queries"], 2);
        assert_eq!(report["returned_candidates"], 2);
        assert_eq!(report["unavailable_queries"], 1);
        assert_eq!(report["elapsed_microseconds_total"], 17);
        assert!(
            report["query_manifest_digest"]
                .as_str()
                .unwrap()
                .starts_with("sha256:")
        );
        for id in [Q1, Q2, C1, C2] {
            assert!(!output.contains(id));
        }
        assert!(!output.contains("labels"));
    }

    #[test]
    fn query_manifest_digest_is_order_independent_and_changes_with_membership() {
        let expected = expected();
        let reversed = vec![Q2.to_owned(), Q1.to_owned()];
        let changed = vec![Q1.to_owned(), "q_aaaaaaaaaaaaaaaa".to_owned()];
        assert_eq!(
            query_manifest_digest(&expected),
            query_manifest_digest(&reversed)
        );
        assert_ne!(
            query_manifest_digest(&expected),
            query_manifest_digest(&changed)
        );
        let output = validate_json(seal(body()).as_bytes(), &expected).unwrap();
        for id in [Q1, Q2] {
            assert!(!output.contains(id));
        }
    }

    #[test]
    fn rejects_missing_or_extra_query_coverage() {
        let mut v = body();
        v["queries"].as_array_mut().unwrap().pop();
        assert!(validate(v).unwrap_err().contains("coverage"));
        let mut v = body();
        v["queries"][1]["query_id"] = json!("q_aaaaaaaaaaaaaaaa");
        assert!(validate(v).unwrap_err().contains("outside expected"));
    }

    #[test]
    fn rejects_duplicate_candidates_and_ranks() {
        let mut v = body();
        v["queries"][0]["candidates"][1]["candidate_id"] = json!(C1);
        assert!(validate(v).unwrap_err().contains("candidate IDs"));
        let mut v = body();
        v["queries"][0]["candidates"][1]["rank"] = json!(1);
        assert!(validate(v).unwrap_err().contains("ranks"));
    }

    #[test]
    fn rejects_inconsistent_count_truncation_and_failed_output() {
        let mut v = body();
        v["queries"][0]["candidate_count"] = json!(3);
        assert!(validate(v).unwrap_err().contains("count and truncation"));
        let mut v = body();
        v["queries"][0]["truncated"] = json!(true);
        assert!(validate(v).unwrap_err().contains("count and truncation"));
        let mut v = body();
        v["queries"][1]["status"] = json!("failed");
        v["queries"][1]["candidates"] = json!([{"candidate_id":C1,"rank":1}]);
        v["queries"][1]["candidate_count"] = json!(1);
        assert!(validate(v).unwrap_err().contains("non-complete"));
    }

    #[test]
    fn enforces_query_candidate_and_byte_limits() {
        let mut too_many_returned = body();
        let candidates: Vec<_> = (0..=MAX_CANDIDATES_PER_QUERY)
            .map(|index| {
                json!({
                    "candidate_id": format!("c_{index:016x}"),
                    "rank": index + 1
                })
            })
            .collect();
        too_many_returned["queries"][0]["candidates"] = json!(candidates);
        too_many_returned["queries"][0]["candidate_count"] = json!(MAX_CANDIDATES_PER_QUERY + 1);
        assert!(
            validate(too_many_returned)
                .unwrap_err()
                .contains("2000 returned-candidate")
        );

        let mut v = body();
        v["queries"][0]["candidate_count"] = json!(MAX_CANDIDATE_COUNT_PER_QUERY + 1);
        v["queries"][0]["truncated"] = json!(true);
        assert!(validate(v).unwrap_err().contains("1000000 total-candidate"));

        let too_many_queries = vec![Q1.to_owned(); MAX_QUERIES + 1];
        assert!(
            validate_json(b"{}", &too_many_queries)
                .unwrap_err()
                .contains("10000-query")
        );
        assert!(
            validate_json(&vec![b' '; MAX_EXPORT_BYTES + 1], &expected())
                .unwrap_err()
                .contains("16 MiB")
        );
    }

    #[test]
    fn rejects_free_text_version_and_reports_aggregate_overflow() {
        let mut v = body();
        v["system_version"] = json!("matcher v1 patient-123");
        assert!(validate(v).unwrap_err().contains("version token"));

        let mut v = body();
        v["queries"][0]["elapsed_microseconds"] = json!(u64::MAX);
        v["queries"][1]["elapsed_microseconds"] = json!(1);
        assert!(validate(v).unwrap_err().contains("latency overflow"));

        let mut aggregate = usize::MAX;
        assert!(
            checked_add(&mut aggregate, 1)
                .unwrap_err()
                .contains("count overflow")
        );
    }

    #[test]
    fn rejects_digest_tampering_and_unknown_fields() {
        let mut value: Value = serde_json::from_str(&export(body())).unwrap();
        value["body_extra"] = json!("unexpected");
        assert!(validate_json(value.to_string().as_bytes(), &expected()).is_err());
        let mut value: Value = serde_json::from_str(&export(body())).unwrap();
        value["body"]["system_version"] = json!("tampered");
        assert!(
            validate_json(value.to_string().as_bytes(), &expected())
                .unwrap_err()
                .contains("digest")
        );
        let mut value = body();
        value["unexpected"] = json!("field");
        assert!(build_json(value.to_string().as_bytes(), &expected()).is_err());
    }

    #[test]
    fn rejects_non_opaque_ids_and_labels_in_export_input() {
        let mut value = body();
        value["queries"][0]["query_id"] = json!("patient-123");
        assert!(validate(value).unwrap_err().contains("opaque"));
        let mut value = body();
        value["labels"] = json!([]);
        assert!(build_json(value.to_string().as_bytes(), &expected()).is_err());
    }
}
