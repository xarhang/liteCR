//! Thin, authenticated operator client for staged `litecr-migration/v1` packages.
//!
//! This module verifies package integrity and staging metadata, then asks the
//! authenticated server to validate all staged batches before the first write.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::Duration,
};

use clap::Subcommand;
use litecr_application::migration::{
    MAX_RECORD_BYTES as MAX_ROW_BYTES, PACKAGE_FORMAT,
    package::{MAX_MANIFEST_BYTES, MAX_PACKAGE_BYTES, parse_manifest, verify_package},
};
use reqwest::{Client, StatusCode, Url, redirect::Policy};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const MAX_DISPOSITION_SOURCE_ROW_BYTES: usize = 64 * 1024 * 1024;
const MAX_BATCH_RECORDS: usize = 500;
const MAX_BATCH_BYTES: usize = 8 * 1024 * 1024;
const MAX_CHECKPOINT_BYTES: usize = 1024 * 1024;
const MAX_QUARANTINE_BYTES: usize = 16 * 1024 * 1024;
const MAX_API_RESPONSE_BYTES: usize = 64 * 1024;
const CREDENTIAL_MIN_BYTES: usize = 32;
const CREDENTIAL_MAX_BYTES: usize = 256;
const CLIENT_TIMEOUT: Duration = Duration::from_secs(120);

/// Commands are intended to be nested beneath the root `litecr` CLI.
#[derive(Debug, Subcommand)]
pub enum MigrationCommand {
    /// Purge at most `limit` expired server-staged v2 uploads, preserving applying runs.
    CleanupV2 {
        #[arg(
            long,
            default_value_t = 100,
            value_parser = clap::value_parser!(u32).range(1..=1000)
        )]
        limit: u32,
    },
    /// Verify and apply a fully staged package, resuming from the server checkpoint.
    Apply {
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        stream: PathBuf,
        #[arg(long)]
        stage_dir: PathBuf,
        #[arg(long)]
        credential_file: PathBuf,
        #[arg(long)]
        api_base_url: String,
        /// Must exactly match the SHA-256 of the exact manifest bytes.
        #[arg(long)]
        confirm_manifest_sha256: String,
    },
    /// Read the authenticated server checkpoint for an exact package manifest.
    Status {
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        credential_file: PathBuf,
        #[arg(long)]
        api_base_url: String,
    },
    /// Request safe source withdrawal after `--confirm-manifest-sha256` and reason review.
    Reverse {
        /// The exact manifest used for the import; no local-only reverse is allowed.
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        reason: String,
        #[arg(long)]
        credential_file: PathBuf,
        #[arg(long)]
        api_base_url: String,
        /// Must exactly match the SHA-256 of the exact manifest bytes.
        #[arg(long)]
        confirm_manifest_sha256: String,
    },
    /// Inspect whether a source-only manifest reverse appears eligible now.
    /// This is advisory; the reverse endpoint rechecks atomically.
    ReverseCheck {
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        credential_file: PathBuf,
        #[arg(long)]
        api_base_url: String,
    },
    /// Submit locally verified, hash-only quarantine disposition evidence.
    Disposition {
        #[arg(long)]
        request_file: PathBuf,
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        stream: PathBuf,
        #[arg(long)]
        stage_dir: PathBuf,
        #[arg(long)]
        replacement_manifest: Option<PathBuf>,
        #[arg(long)]
        replacement_stream: Option<PathBuf>,
        #[arg(long)]
        replacement_stage_dir: Option<PathBuf>,
        #[arg(long)]
        credential_file: PathBuf,
        #[arg(long)]
        api_base_url: String,
    },
}

/// Execute a CLI migration command without logging credentials or source data.
pub async fn run(command: MigrationCommand) -> Result<(), String> {
    match command {
        MigrationCommand::CleanupV2 { .. } => Err(
            "migration cleanup-v2 must be run through the native service database operator path"
                .into(),
        ),
        MigrationCommand::Apply {
            manifest,
            stream,
            stage_dir,
            credential_file,
            api_base_url,
            confirm_manifest_sha256,
        } => {
            let package = validate_package(&manifest, &stream, &stage_dir)?;
            if package.manifest_sha256 != confirm_manifest_sha256 {
                return Err("manifest confirmation does not match the package".into());
            }
            let credential = read_credential(&credential_file)?;
            let client = http_client()?;
            apply_package(&client, &api_base_url, &credential, &package).await
        }
        MigrationCommand::Status {
            manifest,
            credential_file,
            api_base_url,
        } => {
            let run = read_manifest(&manifest)?.run;
            let credential = read_credential(&credential_file)?;
            let client = http_client()?;
            match fetch_checkpoint(&client, &api_base_url, &credential, &run).await? {
                Some(checkpoint) => {
                    println!(
                        "migration status: {}; committed batches: {}; committed records: {}",
                        checkpoint.state,
                        checkpoint.committed_batch_sequence,
                        checkpoint.committed_record_count
                    );
                    Ok(())
                }
                None => {
                    println!(
                        "migration status: not found; committed batches: 0; committed records: 0"
                    );
                    Ok(())
                }
            }
        }
        MigrationCommand::ReverseCheck {
            manifest,
            credential_file,
            api_base_url,
        } => {
            let run = read_manifest(&manifest)?.run;
            let credential = read_credential(&credential_file)?;
            let client = http_client()?;
            let preflight =
                fetch_reverse_preflight(&client, &api_base_url, &credential, &run).await?;
            print_reverse_preflight(&preflight);
            Ok(())
        }
        MigrationCommand::Disposition {
            request_file,
            manifest,
            stream,
            stage_dir,
            replacement_manifest,
            replacement_stream,
            replacement_stage_dir,
            credential_file,
            api_base_url,
        } => {
            let request = read_json_file(&request_file, MAX_QUARANTINE_BYTES)?;
            let body = validate_disposition_request(&request)?;
            verify_disposition_local(
                &body,
                &manifest,
                &stream,
                &stage_dir,
                replacement_manifest.as_deref(),
                replacement_stream.as_deref(),
                replacement_stage_dir.as_deref(),
            )?;
            let credential = read_credential(&credential_file)?;
            let client = http_client()?;
            let response = send_json(
                &client,
                &api_base_url,
                "/api/v1/migrations/quarantine-dispositions",
                &credential,
                &body,
            )
            .await?;
            if response.status() != StatusCode::OK {
                return Err(api_error(response.status()));
            }
            let receipt = response_json(response, "quarantine disposition receipt").await?;
            validate_disposition_receipt(&receipt, &body)?;
            let data = &receipt["data"];
            println!(
                "quarantine disposition recorded: action {}; corrections {}; receipt {}{}",
                string_field(data, "action")?,
                unsigned_field(data, "correction_count")?,
                string_field(data, "disposition_id")?,
                if data["replayed"] == true {
                    " (idempotent retry)"
                } else {
                    ""
                },
            );
            Ok(())
        }
        MigrationCommand::Reverse {
            manifest,
            reason,
            credential_file,
            api_base_url,
            confirm_manifest_sha256,
        } => {
            validate_hex_digest(&confirm_manifest_sha256)?;
            if reason.trim().is_empty()
                || reason.len() > 2000
                || reason.chars().any(char::is_control)
            {
                return Err("reverse reason is invalid".into());
            }
            let run = read_manifest(&manifest)?.run;
            if run.manifest_sha256 != confirm_manifest_sha256 {
                return Err("manifest confirmation does not match the exact manifest bytes".into());
            }
            let credential = read_credential(&credential_file)?;
            let client = http_client()?;
            let checkpoint = fetch_checkpoint(&client, &api_base_url, &credential, &run)
                .await?
                .ok_or_else(|| "authoritative migration checkpoint was not found".to_owned())?;
            let is_complete = checkpoint.state == "completed"
                && checkpoint.committed_record_count == run.expected_record_count
                && checkpoint.committed_batch_sequence > 0;
            let is_partial = checkpoint.state == "importing"
                && checkpoint.committed_record_count > 0
                && checkpoint.committed_record_count < run.expected_record_count
                && checkpoint.committed_batch_sequence > 0;
            let is_already_reversed = checkpoint.state == "reversed";
            if !(is_complete || is_partial || is_already_reversed) {
                return Err("authoritative checkpoint is not in a reversible state".into());
            }
            if !is_already_reversed {
                let preflight =
                    fetch_reverse_preflight(&client, &api_base_url, &credential, &run).await?;
                if !preflight.eligible {
                    print_reverse_preflight(&preflight);
                    return Err(
                        "advisory reverse preflight found blockers; no reverse request was sent"
                            .into(),
                    );
                }
            }
            let expected_manifest_sha256 = run.manifest_sha256.clone();
            let body = json!({
                "manifest_sha256": run.manifest_sha256,
                "confirm_manifest_sha256": confirm_manifest_sha256,
                "registry_id": run.registry_id,
                "organization_id": run.organization_id,
                "reason": reason,
            });
            let response = send_json(
                &client,
                &api_base_url,
                "/api/v1/migrations/reverse",
                &credential,
                &body,
            )
            .await?;
            if response.status() != StatusCode::OK {
                return Err(api_error(response.status()));
            }
            let receipt = parse_reverse_receipt(response, &expected_manifest_sha256).await?;
            println!(
                "migration reversed: {}; withdrawn records: {}; detached identifiers: {}; retained aliases: {}",
                receipt.state,
                receipt.withdrawn_record_count,
                receipt.detached_identifier_count,
                receipt.retained_alias_count
            );
            Ok(())
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ManifestData {
    run: RunData,
    stream_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RunData {
    manifest_sha256: String,
    stream_sha256: String,
    snapshot_id: String,
    source_system_id: String,
    registry_id: String,
    organization_id: String,
    expected_record_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReversePreflight {
    eligible: bool,
    blockers: Vec<(String, u64)>,
}

impl RunData {
    fn api_json(&self) -> Value {
        json!({
            "manifest_sha256": self.manifest_sha256,
            "stream_sha256": self.stream_sha256,
            "snapshot_id": self.snapshot_id,
            "source_system_id": self.source_system_id,
            "registry_id": self.registry_id,
            "organization_id": self.organization_id,
            "expected_record_count": self.expected_record_count,
        })
    }
}

#[derive(Debug, Clone)]
struct StagedBatch {
    sequence: u64,
    first_input_line: u64,
    last_input_line: u64,
    batch_sha256: String,
    record_count: u64,
    path: PathBuf,
}

#[derive(Debug, Clone)]
struct StagedPackage {
    manifest: ManifestData,
    manifest_sha256: String,
    batches: Vec<StagedBatch>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Checkpoint {
    manifest_sha256: String,
    committed_batch_sequence: u64,
    committed_record_count: u64,
    state: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReverseReceipt {
    state: String,
    withdrawn_record_count: u64,
    detached_identifier_count: u64,
    retained_alias_count: u64,
}

fn validate_package(
    manifest_path: &Path,
    stream_path: &Path,
    stage_dir: &Path,
) -> Result<StagedPackage, String> {
    let manifest_bytes = read_regular_file(manifest_path, MAX_MANIFEST_BYTES)?;
    let manifest = manifest_from_bytes(&manifest_bytes)?;
    let stream =
        File::open(stream_path).map_err(|_| "migration stream is unavailable".to_owned())?;
    verify_package(&manifest_bytes, stream).map_err(|error| error.to_string())?;
    if manifest.run.expected_record_count == 0 {
        return Err("empty migration packages cannot be applied".into());
    }

    ensure_real_directory(stage_dir)?;
    validate_private_staging_tree(stage_dir)?;
    let checkpoint_path = stage_dir.join("checkpoint.json");
    let checkpoint = read_json_file(&checkpoint_path, MAX_CHECKPOINT_BYTES)?;
    let expected_checkpoint_fields = [
        "format",
        "state",
        "manifest_sha256",
        "stream_sha256",
        "stream_bytes",
        "record_count",
        "valid_records",
        "exact_replay_duplicates",
        "quarantined_records",
        "batch_size",
        "batch_bytes",
        "committed_batch_sequence",
        "staged_batch_sequence",
        "staged_records",
        "next_line",
    ];
    validate_stage_checkpoint(&checkpoint, &manifest, &expected_checkpoint_fields)?;
    let staged_batch_sequence = unsigned_field(&checkpoint, "staged_batch_sequence")?;
    if staged_batch_sequence == 0 {
        return Err("staging checkpoint contains no batches".into());
    }
    let expected_next_line = manifest
        .run
        .expected_record_count
        .checked_add(1)
        .ok_or_else(|| "manifest record count is invalid".to_owned())?;
    if unsigned_field(&checkpoint, "next_line")? != expected_next_line {
        return Err("staging checkpoint line count is incomplete".into());
    }

    let quarantine_path = stage_dir.join("quarantine.json");
    let quarantine = read_json_file(&quarantine_path, MAX_QUARANTINE_BYTES)?;
    require_exact_fields(&quarantine, &["format", "manifest_sha256", "exceptions"])?;
    if string_field(&quarantine, "format")? != PACKAGE_FORMAT
        || string_field(&quarantine, "manifest_sha256")? != manifest.run.manifest_sha256
        || !array_field(&quarantine, "exceptions")?.is_empty()
    {
        return Err("staging quarantine report is not empty or does not match".into());
    }

    let batches_dir = stage_dir.join("batches");
    ensure_real_directory(&batches_dir)?;
    let expected_file_count = staged_batch_sequence
        .checked_mul(2)
        .ok_or_else(|| "staged batch count is invalid".to_owned())?;
    let actual_files = fs::read_dir(&batches_dir)
        .map_err(|_| "could not read staged batch directory".to_owned())?
        .count() as u64;
    if actual_files != expected_file_count {
        return Err("staged batch files do not match the checkpoint".into());
    }

    let mut batches = Vec::with_capacity(staged_batch_sequence as usize);
    let mut total_records = 0_u64;
    let mut staged_stream_bytes = 0_u64;
    let mut staged_stream_hasher = Sha256::new();
    let mut expected_first_line = 1_u64;
    for sequence in 1..=staged_batch_sequence {
        let ndjson_path = batches_dir.join(format!("{sequence:08}.ndjson"));
        let sidecar_path = batches_dir.join(format!("{sequence:08}.json"));
        let sidecar = read_json_file(&sidecar_path, 16 * 1024)?;
        require_exact_fields(
            &sidecar,
            &[
                "sequence",
                "manifest_sha256",
                "stream_sha256",
                "batch_sha256",
                "record_count",
                "first_input_line",
                "last_input_line",
                "status",
            ],
        )?;
        let first_input_line = unsigned_field(&sidecar, "first_input_line")?;
        let last_input_line = unsigned_field(&sidecar, "last_input_line")?;
        let record_count = unsigned_field(&sidecar, "record_count")?;
        if unsigned_field(&sidecar, "sequence")? != sequence
            || string_field(&sidecar, "manifest_sha256")? != manifest.run.manifest_sha256
            || string_field(&sidecar, "stream_sha256")? != manifest.run.stream_sha256
            || string_field(&sidecar, "status")? != "staged"
            || first_input_line != expected_first_line
            || last_input_line < first_input_line
            || last_input_line - first_input_line + 1 != record_count
            || record_count == 0
            || record_count > unsigned_field(&checkpoint, "batch_size")?
            || record_count > MAX_BATCH_RECORDS as u64
        {
            return Err("staged batch metadata is inconsistent".into());
        }
        let raw_bytes = read_regular_file(&ndjson_path, MAX_BATCH_BYTES)?;
        let actual_batch_digest = sha256_hex(&raw_bytes);
        let batch_sha256 = string_field(&sidecar, "batch_sha256")?.to_owned();
        validate_hex_digest(&batch_sha256)?;
        if actual_batch_digest != batch_sha256 {
            return Err("staged batch checksum does not match its sidecar".into());
        }
        staged_stream_bytes = staged_stream_bytes
            .checked_add(raw_bytes.len() as u64)
            .ok_or_else(|| "staged stream byte count is invalid".to_owned())?;
        staged_stream_hasher.update(&raw_bytes);
        let raw_records = parse_batch_rows(&raw_bytes, &manifest.run)?;
        if raw_records.len() as u64 != record_count {
            return Err("staged batch row count does not match its sidecar".into());
        }
        total_records = total_records
            .checked_add(record_count)
            .ok_or_else(|| "staged record count is invalid".to_owned())?;
        expected_first_line = last_input_line
            .checked_add(1)
            .ok_or_else(|| "staged line number is invalid".to_owned())?;
        batches.push(StagedBatch {
            sequence,
            first_input_line,
            last_input_line,
            batch_sha256,
            record_count,
            path: ndjson_path,
        });
    }
    if total_records != manifest.run.expected_record_count
        || total_records != unsigned_field(&checkpoint, "staged_records")?
        || staged_stream_bytes != manifest.stream_bytes
        || hex(&staged_stream_hasher.finalize()) != manifest.run.stream_sha256
    {
        return Err("staged batch totals or bytes do not match the manifest".into());
    }
    let manifest_sha256 = manifest.run.manifest_sha256.clone();
    Ok(StagedPackage {
        manifest,
        manifest_sha256,
        batches,
    })
}

fn read_manifest(path: &Path) -> Result<ManifestData, String> {
    let bytes = read_regular_file(path, MAX_MANIFEST_BYTES)?;
    manifest_from_bytes(&bytes)
}

fn manifest_from_bytes(bytes: &[u8]) -> Result<ManifestData, String> {
    let parsed = parse_manifest(bytes).map_err(|error| error.to_string())?;
    Ok(ManifestData {
        run: RunData {
            manifest_sha256: hex(&parsed.run.manifest_sha256),
            stream_sha256: hex(&parsed.run.stream_sha256),
            snapshot_id: parsed.run.snapshot_id,
            source_system_id: parsed.run.source_system_id,
            registry_id: parsed.run.registry_id,
            organization_id: parsed.run.organization_id,
            expected_record_count: parsed.run.expected_record_count,
        },
        stream_bytes: parsed.stream_bytes,
    })
}

fn validate_stage_checkpoint(
    checkpoint: &Value,
    manifest: &ManifestData,
    expected_fields: &[&str],
) -> Result<(), String> {
    require_exact_fields(checkpoint, expected_fields)?;
    if string_field(checkpoint, "format")? != PACKAGE_FORMAT
        || string_field(checkpoint, "state")? != "staged"
        || string_field(checkpoint, "manifest_sha256")? != manifest.run.manifest_sha256
        || string_field(checkpoint, "stream_sha256")? != manifest.run.stream_sha256
        || unsigned_field(checkpoint, "stream_bytes")? != manifest.stream_bytes
        || unsigned_field(checkpoint, "record_count")? != manifest.run.expected_record_count
        || unsigned_field(checkpoint, "valid_records")? != manifest.run.expected_record_count
        || unsigned_field(checkpoint, "staged_records")? != manifest.run.expected_record_count
        || unsigned_field(checkpoint, "quarantined_records")? != 0
        || unsigned_field(checkpoint, "exact_replay_duplicates")? != 0
        || unsigned_field(checkpoint, "committed_batch_sequence")? != 0
        || unsigned_field(checkpoint, "batch_size")? == 0
        || unsigned_field(checkpoint, "batch_size")? > MAX_BATCH_RECORDS as u64
        || unsigned_field(checkpoint, "batch_bytes")? == 0
        || unsigned_field(checkpoint, "batch_bytes")? > MAX_BATCH_BYTES as u64
    {
        return Err("staging checkpoint does not match a complete clean package".into());
    }
    Ok(())
}

fn parse_batch_rows(bytes: &[u8], run: &RunData) -> Result<Vec<Value>, String> {
    if bytes.is_empty() || !bytes.ends_with(b"\n") || bytes.len() > MAX_BATCH_BYTES {
        return Err("staged batch bytes are invalid".into());
    }
    let mut rows = Vec::new();
    for raw_line in bytes.split_inclusive(|byte| *byte == b'\n') {
        if raw_line.len() > MAX_ROW_BYTES || raw_line.last() != Some(&b'\n') {
            return Err("staged row exceeds its byte limit or lacks LF".into());
        }
        let row_bytes = &raw_line[..raw_line.len() - 1];
        let row: Value = serde_json::from_slice(row_bytes)
            .map_err(|_| "staged row is not valid UTF-8 JSON".to_owned())?;
        let object = row
            .as_object()
            .ok_or_else(|| "staged row is not a JSON object".to_owned())?;
        let source_system_id = object
            .get("source_system_id")
            .and_then(Value::as_str)
            .ok_or_else(|| "staged row identity is invalid".to_owned())?;
        let source_record_key = object
            .get("source_record_key")
            .and_then(Value::as_str)
            .ok_or_else(|| "staged row identity is invalid".to_owned())?;
        let source_version = object
            .get("source_version")
            .and_then(Value::as_str)
            .ok_or_else(|| "staged row identity is invalid".to_owned())?;
        if source_system_id != run.source_system_id
            || source_record_key.trim().is_empty()
            || source_record_key.len() > 512
            || source_version.trim().is_empty()
            || source_version.len() > 512
            || !canonical_numbers_supported(&row)
        {
            return Err("staged row identity or canonical values are invalid".into());
        }
        let canonical = serde_json::to_vec(&row)
            .map_err(|_| "staged row could not be canonicalized".to_owned())?;
        rows.push(json!({
            "source_system_id": source_system_id,
            "source_record_key": source_record_key,
            "source_version": source_version,
            "payload_sha256": sha256_hex(&canonical),
            "raw_ndjson": std::str::from_utf8(raw_line)
                .map_err(|_| "staged row is not UTF-8".to_owned())?,
        }));
        if rows.len() > MAX_BATCH_RECORDS {
            return Err("staged batch contains too many rows".into());
        }
    }
    Ok(rows)
}

fn canonical_numbers_supported(value: &Value) -> bool {
    match value {
        Value::Number(number) => number
            .as_i64()
            .map(|n| n.unsigned_abs() < (1_u64 << 53))
            .or_else(|| number.as_u64().map(|n| n < (1_u64 << 53)))
            .unwrap_or(false),
        Value::Array(values) => values.iter().all(canonical_numbers_supported),
        Value::Object(values) => values.values().all(canonical_numbers_supported),
        _ => true,
    }
}

fn require_exact_fields(value: &Value, expected: &[&str]) -> Result<(), String> {
    let object = value
        .as_object()
        .ok_or_else(|| "migration metadata is invalid".to_owned())?;
    let actual = object.keys().map(String::as_str).collect::<BTreeSet<_>>();
    let expected = expected.iter().copied().collect::<BTreeSet<_>>();
    if actual != expected {
        return Err("migration metadata fields are invalid".into());
    }
    Ok(())
}

fn string_field<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| "migration metadata is invalid".to_owned())
}

fn unsigned_field(value: &Value, key: &str) -> Result<u64, String> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| "migration metadata is invalid".to_owned())
}

fn array_field<'a>(value: &'a Value, key: &str) -> Result<&'a Vec<Value>, String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| "migration metadata is invalid".to_owned())
}

fn bounded_string(value: &Value, key: &str, max_bytes: usize) -> Result<String, String> {
    let field = string_field(value, key)?;
    if field.trim().is_empty() || field.len() > max_bytes || field.chars().any(char::is_control) {
        return Err("manifest metadata is invalid".into());
    }
    Ok(field.to_owned())
}

fn ensure_real_directory(path: &Path) -> Result<(), String> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| "staging directory is unavailable".to_owned())?;
    if !metadata.file_type().is_dir() {
        return Err("staging directory must be a real directory".into());
    }
    ensure_private_permissions(&metadata)?;
    Ok(())
}

fn validate_private_staging_tree(stage_dir: &Path) -> Result<(), String> {
    let required_root = BTreeSet::from([
        "checkpoint.json".to_owned(),
        "quarantine.json".to_owned(),
        ".validation".to_owned(),
        "batches".to_owned(),
    ]);
    // The Python stager always emits replay reconciliation evidence. It is
    // optional for older/synthetic stages, but when present it is parsed and
    // bound to the parent manifest by the disposition workflow.
    let allowed_root = required_root
        .iter()
        .cloned()
        .chain(std::iter::once("replays.json".to_owned()))
        .collect::<BTreeSet<_>>();
    let mut root_entries = BTreeSet::new();
    for entry in
        fs::read_dir(stage_dir).map_err(|_| "staging directory is unavailable".to_owned())?
    {
        let entry = entry.map_err(|_| "staging directory is invalid".to_owned())?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "staging directory contains an invalid name".to_owned())?;
        root_entries.insert(name.clone());
        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|_| "staging entry is unavailable".to_owned())?;
        if metadata.file_type().is_symlink() {
            return Err("staging tree must not contain symbolic links".into());
        }
        ensure_private_permissions(&metadata)?;
        if name == ".validation" {
            if !metadata.file_type().is_dir() {
                return Err("staging validation index is unavailable".into());
            }
            validate_private_directory_contents(
                &entry.path(),
                &BTreeSet::from(["index.sqlite3", "index.sqlite3-shm", "index.sqlite3-wal"]),
            )?;
        }
    }
    if !required_root.is_subset(&root_entries) || !root_entries.is_subset(&allowed_root) {
        return Err("staging directory contains missing or unexpected entries".into());
    }
    Ok(())
}

fn validate_private_directory_contents(
    path: &Path,
    allowed_files: &BTreeSet<&str>,
) -> Result<(), String> {
    let mut found = BTreeSet::new();
    for entry in
        fs::read_dir(path).map_err(|_| "staging validation index is unavailable".to_owned())?
    {
        let entry = entry.map_err(|_| "staging validation index is invalid".to_owned())?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "staging validation index has an invalid name".to_owned())?;
        if !allowed_files.contains(name.as_str()) {
            return Err("staging validation index contains an unexpected file".into());
        }
        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|_| "staging validation index is unavailable".to_owned())?;
        if !metadata.file_type().is_file() {
            return Err("staging validation index contains a non-file entry".into());
        }
        ensure_private_permissions(&metadata)?;
        found.insert(name);
    }
    if !found.contains("index.sqlite3") {
        return Err("staging validation index is missing".into());
    }
    Ok(())
}

fn ensure_private_permissions(_metadata: &fs::Metadata) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if _metadata.permissions().mode() & 0o077 != 0 {
            return Err("migration package permissions must exclude group and other users".into());
        }
    }
    Ok(())
}

fn read_json_file(path: &Path, max_bytes: usize) -> Result<Value, String> {
    let bytes = read_regular_file(path, max_bytes)?;
    serde_json::from_slice(&bytes).map_err(|_| "staged metadata is invalid JSON".into())
}

fn read_regular_file(path: &Path, max_bytes: usize) -> Result<Vec<u8>, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| "required migration file is unavailable".to_owned())?;
    if !metadata.file_type().is_file() || metadata.len() > max_bytes as u64 {
        return Err("migration file is not a bounded regular file".into());
    }
    ensure_private_permissions(&metadata)?;
    let file = File::open(path).map_err(|_| "migration file could not be opened".to_owned())?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(max_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "migration file could not be read".to_owned())?;
    if bytes.len() > max_bytes {
        return Err("migration file exceeds its byte limit".into());
    }
    Ok(bytes)
}

fn hash_file(path: &Path, max_bytes: u64) -> Result<(String, u64), String> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| "migration stream is unavailable".to_owned())?;
    if !metadata.file_type().is_file() || metadata.len() > max_bytes {
        return Err("migration stream is not a bounded regular file".into());
    }
    ensure_private_permissions(&metadata)?;
    let mut file =
        File::open(path).map_err(|_| "migration stream could not be opened".to_owned())?;
    let mut hasher = Sha256::new();
    let mut count = 0_u64;
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| "migration stream could not be read".to_owned())?;
        if read == 0 {
            break;
        }
        count = count
            .checked_add(read as u64)
            .ok_or_else(|| "migration stream length is invalid".to_owned())?;
        if count > max_bytes {
            return Err("migration stream exceeds its byte limit".into());
        }
        hasher.update(&buffer[..read]);
    }
    Ok((hex(&hasher.finalize()), count))
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        result.push(HEX[(byte >> 4) as usize] as char);
        result.push(HEX[(byte & 0x0f) as usize] as char);
    }
    result
}

fn validate_hex_digest(value: &str) -> Result<(), String> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("digest must be 64 lowercase hexadecimal characters".into());
    }
    Ok(())
}

fn validate_api_base_url(value: &str) -> Result<Url, String> {
    let url = Url::parse(value).map_err(|_| "API base URL is invalid".to_owned())?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(
            "API base URL must be an origin without credentials, path, query, or fragment".into(),
        );
    }
    match url.scheme() {
        "https" => {}
        "http" if is_loopback_host(&url) => {}
        _ => return Err("API URL must use HTTPS except for loopback HTTP".into()),
    }
    Ok(url)
}

fn is_loopback_host(url: &Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    host.parse::<std::net::IpAddr>()
        .is_ok_and(|address| address.is_loopback())
}

fn http_client() -> Result<Client, String> {
    Client::builder()
        .redirect(Policy::none())
        .timeout(CLIENT_TIMEOUT)
        .build()
        .map_err(|_| "could not initialize the migration HTTP client".into())
}

fn read_credential(path: &Path) -> Result<String, String> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| "credential file is unavailable".to_owned())?;
    if !metadata.file_type().is_file() || metadata.len() > CREDENTIAL_MAX_BYTES as u64 + 2 {
        return Err("credential file must be a bounded regular file".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err("credential file permissions must exclude group and other users".into());
        }
    }
    let mut bytes = read_regular_file(path, CREDENTIAL_MAX_BYTES + 2)?;
    while bytes
        .last()
        .is_some_and(|byte| matches!(byte, b'\n' | b'\r'))
    {
        bytes.pop();
    }
    if !(CREDENTIAL_MIN_BYTES..=CREDENTIAL_MAX_BYTES).contains(&bytes.len())
        || bytes.iter().any(|byte| !byte.is_ascii_graphic())
    {
        return Err("credential file content is invalid".into());
    }
    String::from_utf8(bytes).map_err(|_| "credential file content is invalid".into())
}

fn api_error(status: StatusCode) -> String {
    match status {
        StatusCode::UNAUTHORIZED => "migration API authentication failed".into(),
        StatusCode::FORBIDDEN => "credential lacks source-write access to this scope".into(),
        StatusCode::PAYLOAD_TOO_LARGE => "migration API batch exceeds its body limit".into(),
        StatusCode::CONFLICT => "migration checkpoint conflicts or reversal is blocked".into(),
        StatusCode::UNPROCESSABLE_ENTITY => {
            "migration API rejected the request; earlier committed batches remain recorded".into()
        }
        status if status.is_server_error() => "migration API is temporarily unavailable".into(),
        _ => "migration API returned an unexpected response".into(),
    }
}

fn endpoint(base: &str, path: &str) -> Result<Url, String> {
    let base = validate_api_base_url(base)?;
    base.join(path.trim_start_matches('/'))
        .map_err(|_| "API endpoint URL is invalid".into())
}

async fn send_json(
    client: &Client,
    base: &str,
    path: &str,
    credential: &str,
    body: &Value,
) -> Result<reqwest::Response, String> {
    let url = endpoint(base, path)?;
    client
        .post(url)
        .bearer_auth(credential)
        .json(body)
        .send()
        .await
        .map_err(|_| {
            "migration API request failed; rerun to resume from the server checkpoint".into()
        })
}

async fn fetch_checkpoint(
    client: &Client,
    base: &str,
    credential: &str,
    run: &RunData,
) -> Result<Option<Checkpoint>, String> {
    let response = send_json(
        client,
        base,
        "/api/v1/migrations/checkpoint",
        credential,
        &json!({ "run": run.api_json() }),
    )
    .await?;
    if response.status() == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if response.status() != StatusCode::OK {
        return Err(api_error(response.status()));
    }
    let checkpoint = parse_checkpoint(response).await?;
    if checkpoint.manifest_sha256 != run.manifest_sha256 {
        return Err("migration API returned a checkpoint for another manifest".into());
    }
    Ok(Some(checkpoint))
}

async fn fetch_reverse_preflight(
    client: &Client,
    base: &str,
    credential: &str,
    run: &RunData,
) -> Result<ReversePreflight, String> {
    let response = send_json(
        client,
        base,
        "/api/v1/migrations/reverse/preflight",
        credential,
        &json!({
            "manifest_sha256": run.manifest_sha256,
            "registry_id": run.registry_id,
            "organization_id": run.organization_id,
        }),
    )
    .await?;
    if response.status() != StatusCode::OK {
        return Err(api_error(response.status()));
    }
    parse_reverse_preflight(response).await
}

async fn parse_reverse_preflight(response: reqwest::Response) -> Result<ReversePreflight, String> {
    let value = response_json(response, "reverse preflight").await?;
    parse_reverse_preflight_value(&value)
}

fn parse_reverse_preflight_value(value: &Value) -> Result<ReversePreflight, String> {
    require_exact_fields(value, &["data", "meta"])?;
    validate_response_meta(&value["meta"])?;
    require_exact_fields(&value["data"], &["eligible", "blockers"])?;
    let eligible = value["data"]["eligible"]
        .as_bool()
        .ok_or_else(|| "migration API returned an invalid reverse preflight".to_owned())?;
    let array = value["data"]["blockers"]
        .as_array()
        .ok_or_else(|| "migration API returned invalid reverse blockers".to_owned())?;
    if array.len() > 32 {
        return Err("migration API returned too many reverse blocker codes".into());
    }
    let mut blockers = Vec::with_capacity(array.len());
    let mut prior_code: Option<String> = None;
    for blocker in array {
        require_exact_fields(blocker, &["code", "count"])?;
        let code = string_field(blocker, "code")?;
        if !matches!(
            code,
            "already_reversed"
                | "invalid_checkpoint"
                | "journal_mismatch"
                | "missing_current_source"
                | "source_already_withdrawn"
                | "source_changed"
                | "later_source_version"
                | "identity_dependency"
                | "missing_person"
        ) {
            return Err("migration API returned an unknown reverse blocker".into());
        }
        if prior_code
            .as_deref()
            .is_some_and(|previous| previous >= code)
        {
            return Err("migration API returned unordered reverse blockers".into());
        }
        prior_code = Some(code.into());
        let count = unsigned_field(blocker, "count")?;
        if count == 0 {
            return Err("migration API returned an empty reverse blocker count".into());
        }
        blockers.push((code.into(), count));
    }
    if eligible != blockers.is_empty() {
        return Err("migration API returned inconsistent reverse eligibility".into());
    }
    Ok(ReversePreflight { eligible, blockers })
}

fn print_reverse_preflight(preflight: &ReversePreflight) {
    if preflight.eligible {
        println!(
            "advisory reverse preflight: eligible at query time; reverse will recheck atomically"
        );
    } else {
        println!("advisory reverse preflight: blocked at query time");
        for (code, count) in &preflight.blockers {
            println!("  {code}: {count}");
        }
    }
}

async fn parse_checkpoint(response: reqwest::Response) -> Result<Checkpoint, String> {
    let value = response_json(response, "checkpoint").await?;
    require_exact_fields(&value, &["data", "meta"])?;
    validate_response_meta(&value["meta"])?;
    require_exact_fields(
        &value["data"],
        &[
            "manifest_sha256",
            "committed_batch_sequence",
            "committed_record_count",
            "state",
        ],
    )?;
    validate_hex_digest(string_field(&value["data"], "manifest_sha256")?)?;
    let state = string_field(&value["data"], "state")?;
    if !matches!(
        state,
        "validated" | "importing" | "completed" | "reverse_blocked" | "reversed"
    ) {
        return Err("migration API returned an unknown checkpoint state".into());
    }
    Ok(Checkpoint {
        manifest_sha256: string_field(&value["data"], "manifest_sha256")?.into(),
        committed_batch_sequence: unsigned_field(&value["data"], "committed_batch_sequence")?,
        committed_record_count: unsigned_field(&value["data"], "committed_record_count")?,
        state: state.into(),
    })
}

async fn parse_reverse_receipt(
    response: reqwest::Response,
    expected_manifest_sha256: &str,
) -> Result<ReverseReceipt, String> {
    let value = response_json(response, "reverse receipt").await?;
    require_exact_fields(&value, &["data", "meta"])?;
    validate_response_meta(&value["meta"])?;
    require_exact_fields(
        &value["data"],
        &[
            "manifest_sha256",
            "withdrawn_record_count",
            "detached_identifier_count",
            "retained_alias_count",
            "state",
        ],
    )?;
    if string_field(&value["data"], "state")? != "reversed"
        || string_field(&value["data"], "manifest_sha256")? != expected_manifest_sha256
    {
        return Err("migration API did not confirm completed reversal".into());
    }
    Ok(ReverseReceipt {
        state: "reversed".into(),
        withdrawn_record_count: unsigned_field(&value["data"], "withdrawn_record_count")?,
        detached_identifier_count: unsigned_field(&value["data"], "detached_identifier_count")?,
        retained_alias_count: unsigned_field(&value["data"], "retained_alias_count")?,
    })
}

async fn response_json(response: reqwest::Response, kind: &str) -> Result<Value, String> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_API_RESPONSE_BYTES as u64)
    {
        return Err(format!(
            "migration API {kind} response exceeds its size bound"
        ));
    }
    let mut response = response;
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| format!("migration API {kind} response could not be read"))?
    {
        if bytes.len().saturating_add(chunk.len()) > MAX_API_RESPONSE_BYTES {
            return Err(format!(
                "migration API {kind} response exceeds its size bound"
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| format!("migration API returned invalid {kind} metadata"))
}

fn validate_response_meta(value: &Value) -> Result<(), String> {
    require_exact_fields(value, &["api_version"])?;
    if string_field(value, "api_version")? != "v1" {
        return Err("migration API returned an unsupported response version".into());
    }
    Ok(())
}

fn validate_disposition_request(value: &Value) -> Result<Value, String> {
    require_exact_fields(
        value,
        &[
            "action",
            "parent_manifest_sha256",
            "replacement_manifest_sha256",
            "registry_id",
            "organization_id",
            "reason",
            "idempotency_key",
            "corrections",
        ],
    )?;
    let action = string_field(value, "action")?;
    if !matches!(
        action,
        "corrected_resubmission" | "deferred" | "duplicate_acknowledged"
    ) {
        return Err("quarantine disposition action is invalid".into());
    }
    let parent = string_field(value, "parent_manifest_sha256")?;
    validate_hex_digest(parent)?;
    let replacement = value
        .get("replacement_manifest_sha256")
        .unwrap_or(&Value::Null);
    if replacement.is_null() != (action == "deferred") {
        return Err("quarantine disposition replacement manifest is inconsistent".into());
    }
    if let Some(replacement) = replacement.as_str() {
        validate_hex_digest(replacement)?;
        if replacement == parent {
            return Err("replacement package must have a distinct manifest digest".into());
        }
    }
    for field in ["registry_id", "organization_id"] {
        bounded_string(value, field, 128)?;
    }
    let reason = string_field(value, "reason")?;
    let idem = string_field(value, "idempotency_key")?;
    if reason.trim().is_empty()
        || reason.len() > 2000
        || reason.chars().any(char::is_control)
        || idem.trim().is_empty()
        || idem.len() > 256
        || idem.chars().any(char::is_control)
    {
        return Err("quarantine disposition metadata is invalid".into());
    }
    let corrections = array_field(value, "corrections")?;
    if corrections.is_empty() || corrections.len() > 1000 {
        return Err("quarantine disposition evidence count is invalid".into());
    }
    let mut previous = 0_u64;
    for item in corrections {
        require_exact_fields(
            item,
            &[
                "input_line",
                "duplicate_of_line",
                "quarantine_code",
                "original_payload_sha256",
                "corrected_payload_sha256",
            ],
        )?;
        let line = unsigned_field(item, "input_line")?;
        let code = string_field(item, "quarantine_code")?;
        if line == 0
            || line <= previous
            || code.is_empty()
            || code.len() > 64
            || !code
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
        {
            return Err("quarantine disposition evidence is invalid".into());
        }
        previous = line;
        let original = string_field(item, "original_payload_sha256")?;
        validate_hex_digest(original)?;
        let corrected = item.get("corrected_payload_sha256").unwrap_or(&Value::Null);
        let duplicate_of = item.get("duplicate_of_line").unwrap_or(&Value::Null);
        match action {
            "corrected_resubmission" => {
                if code == "EXACT_REPLAY_DUPLICATE" {
                    let duplicate_of = duplicate_of
                        .as_u64()
                        .ok_or_else(|| "duplicate replay provenance line is invalid".to_owned())?;
                    if duplicate_of == 0 || duplicate_of >= line {
                        return Err("duplicate replay provenance line is invalid".into());
                    }
                    let corrected = corrected
                        .as_str()
                        .ok_or_else(|| "duplicate row digest is missing".to_owned())?;
                    validate_hex_digest(corrected)?;
                    if corrected != original {
                        return Err(
                            "duplicate acknowledgement must prove the same exact row digest".into(),
                        );
                    }
                } else {
                    if !matches!(
                        code,
                        "ROW_INVALID_UTF8_OR_JSON"
                            | "ROW_MISSING_LINE_FEED"
                            | "ROW_TOO_LARGE"
                            | "NON_CANONICAL_NUMBER"
                    ) {
                        return Err(
                            "this quarantine class is deferred; it cannot be corrected into an import"
                                .into(),
                        );
                    }
                    if !duplicate_of.is_null() {
                        return Err("only an exact replay may reference a source line".into());
                    }
                    let corrected = corrected
                        .as_str()
                        .ok_or_else(|| "corrected row digest is missing".to_owned())?;
                    validate_hex_digest(corrected)?;
                    if corrected == original {
                        return Err(
                            "corrected row digest must differ from the quarantined row".into()
                        );
                    }
                }
            }
            "deferred" if !corrected.is_null() => {
                return Err("deferred evidence cannot include corrected row bytes".into());
            }
            "duplicate_acknowledged" => {
                let duplicate_of = duplicate_of
                    .as_u64()
                    .ok_or_else(|| "duplicate replay provenance line is invalid".to_owned())?;
                if duplicate_of >= line || duplicate_of == 0 {
                    return Err("duplicate replay provenance line is invalid".into());
                }
                let corrected = corrected
                    .as_str()
                    .ok_or_else(|| "duplicate row digest is missing".to_owned())?;
                validate_hex_digest(corrected)?;
                if code != "EXACT_REPLAY_DUPLICATE" || corrected != original {
                    return Err(
                        "duplicate acknowledgement must prove the same exact row digest".into(),
                    );
                }
            }
            _ => {}
        }
        if action == "duplicate_acknowledged" && duplicate_of.is_null() {
            return Err("duplicate acknowledgement requires its replay source line".into());
        }
        if action == "deferred" && !duplicate_of.is_null() {
            return Err("deferred evidence cannot claim a duplicate source line".into());
        }
    }
    Ok(value.clone())
}

fn verify_disposition_local(
    request: &Value,
    manifest_path: &Path,
    stream_path: &Path,
    stage_dir: &Path,
    replacement_manifest_path: Option<&Path>,
    replacement_stream_path: Option<&Path>,
    replacement_stage_dir: Option<&Path>,
) -> Result<(), String> {
    let action = string_field(request, "action")?;
    let parent = read_manifest(manifest_path)?;
    let (stream_digest, stream_bytes) = hash_file(stream_path, MAX_PACKAGE_BYTES)?;
    if parent.run.manifest_sha256 != string_field(request, "parent_manifest_sha256")?
        || stream_digest != parent.run.stream_sha256
        || stream_bytes != parent.stream_bytes
        || parent.run.registry_id != string_field(request, "registry_id")?
        || parent.run.organization_id != string_field(request, "organization_id")?
    {
        return Err("local source package does not match the disposition request".into());
    }
    ensure_real_directory(stage_dir)?;
    validate_private_staging_tree(stage_dir)?;
    let checkpoint = read_json_file(&stage_dir.join("checkpoint.json"), MAX_CHECKPOINT_BYTES)?;
    require_exact_fields(
        &checkpoint,
        &[
            "format",
            "state",
            "manifest_sha256",
            "stream_sha256",
            "stream_bytes",
            "record_count",
            "valid_records",
            "exact_replay_duplicates",
            "quarantined_records",
            "batch_size",
            "batch_bytes",
            "committed_batch_sequence",
            "staged_batch_sequence",
            "staged_records",
            "next_line",
        ],
    )?;
    if string_field(&checkpoint, "manifest_sha256")? != parent.run.manifest_sha256
        || string_field(&checkpoint, "stream_sha256")? != parent.run.stream_sha256
        || unsigned_field(&checkpoint, "stream_bytes")? != parent.stream_bytes
        || unsigned_field(&checkpoint, "record_count")? != parent.run.expected_record_count
        || unsigned_field(&checkpoint, "committed_batch_sequence")? != 0
    {
        return Err("source stage checkpoint does not match the original package".into());
    }
    let quarantine = read_json_file(&stage_dir.join("quarantine.json"), MAX_QUARANTINE_BYTES)?;
    require_exact_fields(&quarantine, &["format", "manifest_sha256", "exceptions"])?;
    if string_field(&quarantine, "format")? != PACKAGE_FORMAT
        || string_field(&quarantine, "manifest_sha256")? != parent.run.manifest_sha256
    {
        return Err("source quarantine report does not match the original package".into());
    }
    let replays_path = stage_dir.join("replays.json");
    let replays = if replays_path.exists() {
        read_json_file(&replays_path, MAX_QUARANTINE_BYTES)?
    } else {
        json!({"format": PACKAGE_FORMAT, "manifest_sha256": parent.run.manifest_sha256, "replays": []})
    };
    require_exact_fields(&replays, &["format", "manifest_sha256", "replays"])?;
    if string_field(&replays, "format")? != PACKAGE_FORMAT
        || string_field(&replays, "manifest_sha256")? != parent.run.manifest_sha256
    {
        return Err("source replay report does not match the original package".into());
    }

    let items = array_field(request, "corrections")?;
    let quarantine_items = array_field(&quarantine, "exceptions")?;
    let replay_items = array_field(&replays, "replays")?;
    let mut selected = BTreeMap::new();
    for item in items {
        let line = unsigned_field(item, "input_line")?;
        if selected.insert(line, item).is_some() {
            return Err("disposition request repeats an input line".into());
        }
    }
    let mut expected_items = match action {
        "duplicate_acknowledged" => replay_items.iter().collect::<Vec<_>>(),
        "corrected_resubmission" => quarantine_items
            .iter()
            .chain(replay_items.iter())
            .collect::<Vec<_>>(),
        _ => quarantine_items.iter().collect::<Vec<_>>(),
    };
    expected_items.sort_by_key(|item| unsigned_field(item, "line").unwrap_or(0));
    if expected_items.len() != items.len() {
        return Err(
            "disposition request does not cover exactly the locally staged evidence".into(),
        );
    }
    let mut requested_lines = BTreeSet::new();
    for evidence in expected_items {
        let line = unsigned_field(evidence, "line")?;
        let supplied = selected
            .get(&line)
            .ok_or_else(|| "disposition omits a staged exception".to_owned())?;
        let expected_code = if action == "duplicate_acknowledged" {
            "EXACT_REPLAY_DUPLICATE"
        } else {
            string_field(evidence, "code")?
        };
        if string_field(supplied, "quarantine_code")? != expected_code
            || string_field(supplied, "original_payload_sha256")?
                != string_field(evidence, "payload_sha256")?
        {
            return Err("disposition row code or digest differs from the staged evidence".into());
        }
        requested_lines.insert(line);
        if expected_code == "EXACT_REPLAY_DUPLICATE" {
            let target_line = unsigned_field(evidence, "duplicate_of_line")?;
            if unsigned_field(supplied, "duplicate_of_line")? != target_line {
                return Err("duplicate acknowledgement target differs from replay evidence".into());
            }
        }
    }
    if action == "duplicate_acknowledged" && !quarantine_items.is_empty() {
        return Err("duplicate acknowledgement cannot bypass quarantined rows".into());
    }

    let mut lines_to_verify = requested_lines.clone();
    if action == "corrected_resubmission" || action == "duplicate_acknowledged" {
        for item in replay_items {
            lines_to_verify.insert(unsigned_field(item, "duplicate_of_line")?);
        }
    }
    let parent_line_info = collect_line_info(stream_path, &lines_to_verify)?;
    for (line, supplied) in &selected {
        let info = parent_line_info
            .get(line)
            .ok_or_else(|| "disposition line is outside the source stream".to_owned())?;
        if hex(&info.digest) != string_field(supplied, "original_payload_sha256")? {
            return Err("original row bytes do not match the disposition digest".into());
        }
    }
    if action == "corrected_resubmission" || action == "duplicate_acknowledged" {
        for item in replay_items {
            let line = unsigned_field(item, "line")?;
            let target = unsigned_field(item, "duplicate_of_line")?;
            let left = &parent_line_info[&line];
            let right = &parent_line_info[&target];
            if left.digest != right.digest || left.length != right.length {
                return Err(
                    "duplicate row is not byte-identical to its original physical line".into(),
                );
            }
            let duplicate = read_at(stream_path, left.offset, left.length)?;
            let original = read_at(stream_path, right.offset, right.length)?;
            if duplicate != original {
                return Err("duplicate row bytes differ from their original physical line".into());
            }
        }
    }

    if action == "deferred" {
        if replacement_manifest_path.is_some()
            || replacement_stream_path.is_some()
            || replacement_stage_dir.is_some()
        {
            return Err(
                "deferred disposition cannot carry an importable replacement package".into(),
            );
        }
        return Ok(());
    }
    let replacement_manifest_path = replacement_manifest_path
        .ok_or_else(|| "replacement package manifest is required".to_owned())?;
    let replacement_stream_path = replacement_stream_path
        .ok_or_else(|| "replacement package stream is required".to_owned())?;
    let replacement_stage_dir =
        replacement_stage_dir.ok_or_else(|| "replacement staged package is required".to_owned())?;
    let replacement = validate_package(
        replacement_manifest_path,
        replacement_stream_path,
        replacement_stage_dir,
    )?;
    if replacement.manifest.run.manifest_sha256
        != string_field(request, "replacement_manifest_sha256")?
        || replacement.manifest.run.source_system_id != parent.run.source_system_id
        || replacement.manifest.run.snapshot_id != parent.run.snapshot_id
        || replacement.manifest.run.registry_id != parent.run.registry_id
        || replacement.manifest.run.organization_id != parent.run.organization_id
    {
        return Err("replacement package does not match the locally prepared disposition".into());
    }
    if action == "corrected_resubmission" {
        let expected_records = parent
            .run
            .expected_record_count
            .checked_sub(replay_items.len() as u64)
            .ok_or_else(|| "replay row count exceeds source row count".to_owned())?;
        if replacement.manifest.run.expected_record_count != expected_records {
            return Err(
                "corrected package changed the source row count beyond exact replay removal".into(),
            );
        }
        compare_streams(
            stream_path,
            replacement_stream_path,
            &requested_lines,
            true,
            &selected,
        )?;
    } else {
        compare_streams(
            stream_path,
            replacement_stream_path,
            &requested_lines,
            false,
            &selected,
        )?;
    }
    Ok(())
}

#[derive(Debug)]
struct LineInfo {
    offset: u64,
    length: u64,
    digest: [u8; 32],
}

fn collect_line_info(
    path: &Path,
    selected: &BTreeSet<u64>,
) -> Result<BTreeMap<u64, LineInfo>, String> {
    let file = File::open(path).map_err(|_| "source stream could not be opened".to_owned())?;
    let mut reader = BufReader::new(file);
    let mut result = BTreeMap::new();
    let mut line_no = 0_u64;
    let mut offset = 0_u64;
    loop {
        let mut bytes = Vec::new();
        let count = reader
            .read_until(b'\n', &mut bytes)
            .map_err(|_| "source stream could not be read".to_owned())?;
        if count == 0 {
            break;
        }
        line_no += 1;
        if selected.contains(&line_no) {
            if bytes.len() > MAX_DISPOSITION_SOURCE_ROW_BYTES {
                return Err("selected source row exceeds local verification bound".into());
            }
            result.insert(
                line_no,
                LineInfo {
                    offset,
                    length: count as u64,
                    digest: Sha256::digest(&bytes).into(),
                },
            );
        }
        offset = offset.saturating_add(count as u64);
    }
    if line_no == 0 && !selected.is_empty() {
        return Err("source stream is empty".into());
    }
    if result.len() != selected.len() {
        return Err("a disposition line is missing from the source stream".into());
    }
    Ok(result)
}

fn read_at(path: &Path, offset: u64, length: u64) -> Result<Vec<u8>, String> {
    if length > MAX_DISPOSITION_SOURCE_ROW_BYTES as u64 {
        return Err("selected row exceeds local verification bound".into());
    }
    let mut file = File::open(path).map_err(|_| "source stream could not be opened".to_owned())?;
    file.seek(SeekFrom::Start(offset))
        .map_err(|_| "source stream could not be positioned".to_owned())?;
    let mut bytes = vec![0; length as usize];
    file.read_exact(&mut bytes)
        .map_err(|_| "source row could not be read".to_owned())?;
    Ok(bytes)
}

fn compare_streams(
    parent: &Path,
    replacement: &Path,
    selected: &BTreeSet<u64>,
    replace_selected: bool,
    items: &BTreeMap<u64, &Value>,
) -> Result<(), String> {
    let mut original = BufReader::new(
        File::open(parent).map_err(|_| "source stream could not be opened".to_owned())?,
    );
    let mut derived = BufReader::new(
        File::open(replacement).map_err(|_| "replacement stream could not be opened".to_owned())?,
    );
    let mut line_no = 0_u64;
    loop {
        let mut original_line = Vec::new();
        let count = original
            .read_until(b'\n', &mut original_line)
            .map_err(|_| "source stream could not be read".to_owned())?;
        if count == 0 {
            break;
        }
        line_no += 1;
        let remove_exact_replay = items
            .get(&line_no)
            .is_some_and(|item| !item["duplicate_of_line"].is_null());
        if selected.contains(&line_no) && (!replace_selected || remove_exact_replay) {
            continue;
        }
        let mut replacement_line = Vec::new();
        if derived
            .read_until(b'\n', &mut replacement_line)
            .map_err(|_| "replacement stream could not be read".to_owned())?
            == 0
        {
            return Err("replacement stream ended before its source evidence".into());
        }
        if replace_selected && selected.contains(&line_no) {
            let expected = string_field(items[&line_no], "corrected_payload_sha256")?;
            let corrected_digest: [u8; 32] = Sha256::digest(&replacement_line).into();
            if hex(&corrected_digest) != expected {
                return Err(
                    "corrected replacement row bytes do not match the audited digest".into(),
                );
            }
        } else if original_line != replacement_line {
            return Err("replacement stream changed bytes outside explicitly selected rows".into());
        }
    }
    let mut extra = Vec::new();
    if derived
        .read_until(b'\n', &mut extra)
        .map_err(|_| "replacement stream could not be read".to_owned())?
        != 0
    {
        return Err("replacement stream contains rows beyond the derived source package".into());
    }
    Ok(())
}

fn validate_disposition_receipt(value: &Value, request: &Value) -> Result<(), String> {
    require_exact_fields(value, &["data", "meta"])?;
    validate_response_meta(&value["meta"])?;
    require_exact_fields(
        &value["data"],
        &[
            "disposition_id",
            "action",
            "replacement_manifest_sha256",
            "correction_count",
            "replayed",
        ],
    )?;
    let data = &value["data"];
    if string_field(data, "action")? != string_field(request, "action")?
        || unsigned_field(data, "correction_count")?
            != array_field(request, "corrections")?.len() as u64
        || data["replayed"].as_bool().is_none()
        || data["replacement_manifest_sha256"] != request["replacement_manifest_sha256"]
        || bounded_string(data, "disposition_id", 64)?.is_empty()
    {
        return Err("migration API returned an inconsistent disposition receipt".into());
    }
    Ok(())
}

async fn apply_package(
    client: &Client,
    base: &str,
    credential: &str,
    package: &StagedPackage,
) -> Result<(), String> {
    let mut committed_sequence = 0_u64;
    let mut committed_records = 0_u64;
    if let Some(checkpoint) =
        fetch_checkpoint(client, base, credential, &package.manifest.run).await?
    {
        if checkpoint.state == "reversed" || checkpoint.state == "reverse_blocked" {
            return Err("migration checkpoint is not resumable".into());
        }
        if checkpoint.committed_batch_sequence > package.batches.len() as u64 {
            return Err("server checkpoint exceeds the staged package".into());
        }
        let expected_prefix_records = package
            .batches
            .iter()
            .take(checkpoint.committed_batch_sequence as usize)
            .map(|batch| batch.record_count)
            .sum::<u64>();
        if expected_prefix_records != checkpoint.committed_record_count {
            return Err("server checkpoint does not match the staged batch prefix".into());
        }
        let valid_checkpoint_state = match checkpoint.state.as_str() {
            "validated" => {
                checkpoint.committed_batch_sequence == 0 && checkpoint.committed_record_count == 0
            }
            "importing" => {
                checkpoint.committed_batch_sequence > 0
                    && checkpoint.committed_record_count > 0
                    && checkpoint.committed_record_count
                        < package.manifest.run.expected_record_count
            }
            "completed" => {
                checkpoint.committed_batch_sequence == package.batches.len() as u64
                    && checkpoint.committed_record_count
                        == package.manifest.run.expected_record_count
            }
            _ => false,
        };
        if !valid_checkpoint_state {
            return Err("server checkpoint state or counts are inconsistent".into());
        }
        committed_sequence = checkpoint.committed_batch_sequence;
        committed_records = checkpoint.committed_record_count;
    }
    if committed_sequence == package.batches.len() as u64 {
        if committed_records != package.manifest.run.expected_record_count {
            return Err("completed batch count does not match the package".into());
        }
        println!(
            "migration already complete: committed batches: {committed_sequence}; committed records: {committed_records}"
        );
        return Ok(());
    }

    // Validate every staged batch on the authenticated server before the first
    // write. Each payload is loaded and dropped one at a time to bound memory.
    for batch in &package.batches {
        let body = batch_request(package, batch)?;
        let response = send_json(
            client,
            base,
            "/api/v1/migrations/batches/validate",
            credential,
            &body,
        )
        .await?;
        if response.status() != StatusCode::NO_CONTENT {
            if response.status() == StatusCode::UNPROCESSABLE_ENTITY {
                return Err("server-side migration preflight rejected a batch; no batch was committed by this invocation".into());
            }
            return Err(api_error(response.status()));
        }
    }
    eprintln!(
        "All staged batches passed authenticated server-side mapping validation. Database-dependent conflicts are still checked transactionally during apply; OpenCR/client compatibility remains unverified."
    );
    for batch in package.batches.iter().skip(committed_sequence as usize) {
        let body = batch_request(package, batch)?;
        let response = send_json(
            client,
            base,
            "/api/v1/migrations/batches",
            credential,
            &body,
        )
        .await?;
        if response.status() != StatusCode::ACCEPTED && response.status() != StatusCode::OK {
            return Err(api_error(response.status()));
        }
        let receipt = parse_checkpoint(response).await?;
        committed_records = committed_records
            .checked_add(batch.record_count)
            .ok_or_else(|| "committed record count is invalid".to_owned())?;
        if receipt.manifest_sha256 != package.manifest_sha256
            || receipt.committed_batch_sequence != batch.sequence
            || receipt.committed_record_count != committed_records
            || (committed_records == package.manifest.run.expected_record_count
                && receipt.state != "completed")
            || (committed_records < package.manifest.run.expected_record_count
                && receipt.state != "importing")
        {
            return Err("migration API receipt does not match the submitted batch".into());
        }
        committed_sequence = batch.sequence;
        println!(
            "migration progress: committed batch {committed_sequence}/{}; committed records {committed_records}/{}",
            package.batches.len(),
            package.manifest.run.expected_record_count
        );
    }
    Ok(())
}

fn batch_request(package: &StagedPackage, batch: &StagedBatch) -> Result<Value, String> {
    let raw_bytes = read_regular_file(&batch.path, MAX_BATCH_BYTES)?;
    if sha256_hex(&raw_bytes) != batch.batch_sha256 {
        return Err("staged batch changed after package preflight".into());
    }
    let raw_records = parse_batch_rows(&raw_bytes, &package.manifest.run)?;
    if raw_records.len() as u64 != batch.record_count {
        return Err("staged batch changed after package preflight".into());
    }
    Ok(json!({
        "run": package.manifest.run.api_json(),
        "sequence": batch.sequence,
        "first_input_line": batch.first_input_line,
        "last_input_line": batch.last_input_line,
        "batch_sha256": batch.batch_sha256,
        "records": raw_records,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::io::Write;

    #[test]
    fn accepts_https_and_loopback_http_only_and_rejects_redirectable_origins() {
        assert!(validate_api_base_url("https://litecr.example").is_ok());
        assert!(validate_api_base_url("http://127.0.0.1:8080").is_ok());
        assert!(validate_api_base_url("http://[::1]:8080").is_ok());
        assert!(validate_api_base_url("http://localhost:8080").is_ok());
        for value in [
            "http://example.com",
            "ftp://localhost",
            "https://user:secret@example.com",
            "https://example.com/path",
            "https://example.com?token=x",
        ] {
            assert!(validate_api_base_url(value).is_err(), "accepted {value}");
        }
    }

    #[test]
    fn digest_parser_requires_lowercase_sha256() {
        assert!(validate_hex_digest(&"ab".repeat(32)).is_ok());
        assert!(validate_hex_digest(&"AB".repeat(32)).is_err());
        assert!(validate_hex_digest("abc").is_err());
    }

    #[test]
    fn reverse_preflight_parser_accepts_only_aggregate_codes_and_consistent_eligibility() {
        let eligible = json!({"data":{"eligible":true,"blockers":[]},"meta":{"api_version":"v1"}});
        assert!(parse_reverse_preflight_value(&eligible).unwrap().eligible);
        let blocked = json!({"data":{"eligible":false,"blockers":[{"code":"source_changed","count":3}]},"meta":{"api_version":"v1"}});
        assert_eq!(
            parse_reverse_preflight_value(&blocked).unwrap().blockers,
            vec![("source_changed".into(), 3)]
        );
        let leaked = json!({"data":{"eligible":false,"blockers":[{"code":"source_changed","count":3,"source_record_id":"secret"}]},"meta":{"api_version":"v1"}});
        assert!(parse_reverse_preflight_value(&leaked).is_err());
        let inconsistent = json!({"data":{"eligible":true,"blockers":[{"code":"source_changed","count":1}]},"meta":{"api_version":"v1"}});
        assert!(parse_reverse_preflight_value(&inconsistent).is_err());
    }

    #[test]
    fn disposition_cli_rejects_semantic_relations_and_unproven_replay_hashes() {
        let corrected = json!({
            "action":"corrected_resubmission",
            "parent_manifest_sha256":"11".repeat(32),
            "replacement_manifest_sha256":"22".repeat(32),
            "registry_id":"registry-a",
            "organization_id":"org-a",
            "reason":"reviewed mechanical correction",
            "idempotency_key":"synthetic-key-1",
            "corrections":[{
                "input_line":3,
                "duplicate_of_line":null,
                "quarantine_code":"ROW_INVALID_UTF8_OR_JSON",
                "original_payload_sha256":"33".repeat(32),
                "corrected_payload_sha256":"44".repeat(32)
            }]
        });
        assert!(validate_disposition_request(&corrected).is_ok());

        let mut relationship = corrected.clone();
        relationship["corrections"][0]["quarantine_code"] =
            json!("HISTORICAL_RELATIONSHIPS_UNSUPPORTED");
        assert!(validate_disposition_request(&relationship).is_err());

        let mut mixed = corrected.clone();
        mixed["corrections"].as_array_mut().unwrap().push(json!({
            "input_line":5,
            "duplicate_of_line":2,
            "quarantine_code":"EXACT_REPLAY_DUPLICATE",
            "original_payload_sha256":"55".repeat(32),
            "corrected_payload_sha256":"55".repeat(32)
        }));
        assert!(validate_disposition_request(&mixed).is_ok());

        let duplicate = json!({
            "action":"duplicate_acknowledged",
            "parent_manifest_sha256":"11".repeat(32),
            "replacement_manifest_sha256":"22".repeat(32),
            "registry_id":"registry-a",
            "organization_id":"org-a",
            "reason":"acknowledged exact replay",
            "idempotency_key":"synthetic-key-2",
            "corrections":[{
                "input_line":5,
                "duplicate_of_line":2,
                "quarantine_code":"EXACT_REPLAY_DUPLICATE",
                "original_payload_sha256":"33".repeat(32),
                "corrected_payload_sha256":"55".repeat(32)
            }]
        });
        assert!(validate_disposition_request(&duplicate).is_err());
    }

    #[test]
    fn local_duplicate_resubmission_requires_every_retained_line_to_match_exact_bytes() {
        let root =
            std::env::temp_dir().join(format!("litecr-disposition-stream-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let parent = root.join("parent.ndjson");
        let replacement = root.join("replacement.ndjson");
        let first = b"{\"row\":1}\n";
        let other = b"{\"row\":2}\n";
        fs::write(
            &parent,
            [first.as_slice(), other.as_slice(), first.as_slice()].concat(),
        )
        .unwrap();
        fs::write(&replacement, [first.as_slice(), other.as_slice()].concat()).unwrap();
        let selected = BTreeSet::from([3_u64]);
        let item = json!({
            "input_line":3,
            "duplicate_of_line":1,
            "quarantine_code":"EXACT_REPLAY_DUPLICATE",
            "original_payload_sha256":sha256_hex(first),
            "corrected_payload_sha256":sha256_hex(first)
        });
        let items = BTreeMap::from([(3_u64, &item)]);
        assert!(compare_streams(&parent, &replacement, &selected, false, &items).is_ok());

        fs::write(
            &replacement,
            [first.as_slice(), b"{\"row\":tampered}\n".as_slice()].concat(),
        )
        .unwrap();
        assert!(compare_streams(&parent, &replacement, &selected, false, &items).is_err());

        let repaired = b"{\"row\":\"repaired\"}\n";
        fs::write(
            &replacement,
            [repaired.as_slice(), first.as_slice()].concat(),
        )
        .unwrap();
        let correction = json!({
            "input_line":1,
            "duplicate_of_line":null,
            "quarantine_code":"ROW_INVALID_UTF8_OR_JSON",
            "original_payload_sha256":sha256_hex(b"invalid\n"),
            "corrected_payload_sha256":sha256_hex(repaired)
        });
        let replay = json!({
            "input_line":3,
            "duplicate_of_line":2,
            "quarantine_code":"EXACT_REPLAY_DUPLICATE",
            "original_payload_sha256":sha256_hex(first),
            "corrected_payload_sha256":sha256_hex(first)
        });
        let selected = BTreeSet::from([1_u64, 3_u64]);
        let items = BTreeMap::from([(1_u64, &correction), (3_u64, &replay)]);
        fs::write(
            &parent,
            [b"invalid\n".as_slice(), first.as_slice(), first.as_slice()].concat(),
        )
        .unwrap();
        assert!(compare_streams(&parent, &replacement, &selected, true, &items).is_ok());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_stager_checkpoint_with_quarantine_or_duplicate_rows() {
        let checkpoint = json!({
            "format": PACKAGE_FORMAT,
            "state":"staged",
            "manifest_sha256":"11".repeat(32),
            "stream_sha256":"22".repeat(32),
            "stream_bytes":10,
            "record_count":1,
            "valid_records":1,
            "exact_replay_duplicates":1,
            "quarantined_records":0,
            "batch_size":500,
            "batch_bytes":MAX_BATCH_BYTES,
            "committed_batch_sequence":0,
            "staged_batch_sequence":1,
            "staged_records":1,
            "next_line":2
        });
        let manifest = ManifestData {
            run: RunData {
                manifest_sha256: "11".repeat(32),
                stream_sha256: "22".repeat(32),
                snapshot_id: "snapshot".into(),
                source_system_id: "source".into(),
                registry_id: "registry".into(),
                organization_id: "organization".into(),
                expected_record_count: 1,
            },
            stream_bytes: 10,
        };
        let fields = [
            "format",
            "state",
            "manifest_sha256",
            "stream_sha256",
            "stream_bytes",
            "record_count",
            "valid_records",
            "exact_replay_duplicates",
            "quarantined_records",
            "batch_size",
            "batch_bytes",
            "committed_batch_sequence",
            "staged_batch_sequence",
            "staged_records",
            "next_line",
        ];
        assert!(validate_stage_checkpoint(&checkpoint, &manifest, &fields).is_err());
        let mut clean = checkpoint;
        clean["exact_replay_duplicates"] = json!(0);
        assert!(validate_stage_checkpoint(&clean, &manifest, &fields).is_ok());
    }

    #[test]
    fn credential_file_reader_requires_private_unix_mode_and_valid_bytes() {
        let path = env::temp_dir().join(format!("litecr-cli-credential-{}", std::process::id()));
        let mut file = File::create(&path).unwrap();
        file.write_all(b"abcdefghijklmnopqrstuvwxyz0123456789\n")
            .unwrap();
        file.sync_all().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert_eq!(
            read_credential(&path).unwrap(),
            "abcdefghijklmnopqrstuvwxyz0123456789"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            assert!(read_credential(&path).is_err());
        }
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn package_manifest_parser_is_strict_and_hashes_exact_bytes() {
        let directory = env::temp_dir().join(format!("litecr-cli-manifest-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("manifest.json");
        let manifest = json!({
            "format":PACKAGE_FORMAT,
            "snapshot_id":"snapshot",
            "source_system_id":"source",
            "target_registry_id":"registry",
            "target_organization_id":"organization",
            "exported_at":"2026-10-08T00:00:00Z",
            "record_count":1,
            "stream_bytes":2,
            "stream_sha256":"00".repeat(32)
        });
        let raw = serde_json::to_vec(&manifest).unwrap();
        fs::write(&path, &raw).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let parsed = read_manifest(&path).unwrap();
        assert_eq!(parsed.run.manifest_sha256, sha256_hex(&raw));
        let extra = json!({"unexpected":"field"});
        fs::write(&path, serde_json::to_vec(&extra).unwrap()).unwrap();
        assert!(read_manifest(&path).is_err());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn private_staging_tree_accepts_optional_python_replay_report_and_rejects_extras() {
        let directory = env::temp_dir().join(format!(
            "litecr-cli-staging-replays-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(directory.join(".validation")).unwrap();
        fs::create_dir(directory.join("batches")).unwrap();
        for name in ["checkpoint.json", "quarantine.json"] {
            fs::write(directory.join(name), b"{}").unwrap();
        }
        fs::write(directory.join(".validation/index.sqlite3"), b"").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for path in [
                &directory,
                &directory.join(".validation"),
                &directory.join("batches"),
            ] {
                fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
            }
            for path in [
                &directory.join("checkpoint.json"),
                &directory.join("quarantine.json"),
                &directory.join(".validation/index.sqlite3"),
            ] {
                fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
            }
        }

        // Older and hand-built stages may omit this report. Python-generated
        // stages include it, and the CLI must accept that canonical output.
        assert!(validate_private_staging_tree(&directory).is_ok());
        let replay_report = directory.join("replays.json");
        fs::write(
            &replay_report,
            br#"{"format":"litecr-migration/v1","manifest_sha256":"0000000000000000000000000000000000000000000000000000000000000000","replays":[]}"#,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&replay_report, fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert!(validate_private_staging_tree(&directory).is_ok());

        fs::write(directory.join("unexpected.txt"), b"extra").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                directory.join("unexpected.txt"),
                fs::Permissions::from_mode(0o600),
            )
            .unwrap();
        }
        assert!(validate_private_staging_tree(&directory).is_err());
        fs::remove_dir_all(directory).unwrap();
    }
}
