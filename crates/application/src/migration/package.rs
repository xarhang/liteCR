//! Pure verification of the exact bytes in a `litecr-migration/v1` package.
//!
//! This module performs no persistence, authorization, or registry mutation.
//! It can prove that bytes presented together match the package commitment;
//! callers still need to retain those exact bytes until verification completes.

use std::io::Read;

use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::{MAX_RECORD_BYTES, MigrationRun, PACKAGE_FORMAT};

pub const MAX_MANIFEST_BYTES: usize = 1024 * 1024;
pub const MAX_PACKAGE_BYTES: u64 = 20 * 1024 * 1024 * 1024;

/// Verified package metadata. The stream digest is computed by this module,
/// rather than copied from the manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedPackage {
    pub run: MigrationRun,
    pub stream_bytes: u64,
    pub record_count: u64,
    pub exported_at: String,
}

/// Strictly parsed manifest metadata. This alone does not verify any stream
/// bytes and must not be treated as proof that an upload is complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedManifest {
    pub run: MigrationRun,
    pub stream_bytes: u64,
    pub exported_at: String,
}

/// Incremental exact-byte verifier for storage adapters that read ordered
/// package batches asynchronously. Feed each stored batch exactly once and in
/// sequence, then call [`finish`](Self::finish). This does not persist data or
/// assert that the supplied chunks were obtained from a trusted source.
pub struct PackageStreamVerifier {
    manifest: ParsedManifest,
    stream_hasher: Sha256,
    byte_count: u64,
    record_count: u64,
    line_bytes: usize,
}

#[derive(Debug, Error)]
pub enum PackageVerificationError {
    #[error("migration manifest exceeds the supported byte limit")]
    ManifestTooLarge,
    #[error("migration manifest is invalid")]
    InvalidManifest,
    #[error("migration manifest format is unsupported")]
    UnsupportedFormat,
    #[error("migration manifest metadata is invalid")]
    InvalidMetadata,
    #[error("migration stream exceeds the supported byte limit")]
    StreamTooLarge,
    #[error("migration stream has an invalid or oversized line")]
    InvalidStreamLine,
    #[error("migration stream is missing a final LF")]
    MissingFinalLf,
    #[error("migration stream digest or byte count does not match the manifest")]
    StreamCommitmentMismatch,
    #[error("migration stream record count does not match the manifest")]
    RecordCountMismatch,
    #[error("migration stream could not be read")]
    StreamRead,
}

/// Verify the exact manifest bytes and the concatenation of exact stream
/// bytes in order. The reader must yield the package stream without decoding
/// and re-encoding rows or changing line endings.
pub fn verify_package<R: Read>(
    manifest_bytes: &[u8],
    mut stream: R,
) -> Result<VerifiedPackage, PackageVerificationError> {
    let mut verifier = PackageStreamVerifier::new(manifest_bytes)?;
    let mut buffer = [0_u8; 64 * 1024];

    loop {
        let read = stream
            .read(&mut buffer)
            .map_err(|_| PackageVerificationError::StreamRead)?;
        if read == 0 {
            break;
        }
        verifier.update(&buffer[..read])?;
    }

    verifier.finish()
}

impl PackageStreamVerifier {
    pub fn new(manifest_bytes: &[u8]) -> Result<Self, PackageVerificationError> {
        Ok(Self::from_parsed_manifest(parse_manifest(manifest_bytes)?))
    }

    /// Rebuild a verifier from manifest metadata parsed and persisted by a
    /// trusted adapter. This lets a server discard raw manifest bytes after
    /// hashing/parsing while still verifying the exact ordered stream.
    pub fn from_parsed_manifest(manifest: ParsedManifest) -> Self {
        Self {
            manifest,
            stream_hasher: Sha256::new(),
            byte_count: 0,
            record_count: 0,
            line_bytes: 0,
        }
    }

    /// Add the next exact ordered bytes from the package stream.
    pub fn update(&mut self, bytes: &[u8]) -> Result<(), PackageVerificationError> {
        self.byte_count = self
            .byte_count
            .checked_add(bytes.len() as u64)
            .filter(|count| *count <= MAX_PACKAGE_BYTES)
            .ok_or(PackageVerificationError::StreamTooLarge)?;
        self.stream_hasher.update(bytes);

        for byte in bytes {
            if *byte == b'\n' {
                if self.line_bytes == 0 || self.line_bytes >= MAX_RECORD_BYTES {
                    return Err(PackageVerificationError::InvalidStreamLine);
                }
                self.record_count = self
                    .record_count
                    .checked_add(1)
                    .ok_or(PackageVerificationError::RecordCountMismatch)?;
                self.line_bytes = 0;
            } else {
                self.line_bytes = self
                    .line_bytes
                    .checked_add(1)
                    .filter(|length| *length <= MAX_RECORD_BYTES)
                    .ok_or(PackageVerificationError::InvalidStreamLine)?;
            }
        }
        Ok(())
    }

    pub fn finish(self) -> Result<VerifiedPackage, PackageVerificationError> {
        if self.line_bytes != 0 {
            return Err(PackageVerificationError::MissingFinalLf);
        }
        let stream_sha256: [u8; 32] = self.stream_hasher.finalize().into();
        if self.byte_count != self.manifest.stream_bytes
            || stream_sha256 != self.manifest.run.stream_sha256
        {
            return Err(PackageVerificationError::StreamCommitmentMismatch);
        }
        if self.record_count != self.manifest.run.expected_record_count {
            return Err(PackageVerificationError::RecordCountMismatch);
        }
        Ok(VerifiedPackage {
            run: self.manifest.run,
            stream_bytes: self.byte_count,
            record_count: self.record_count,
            exported_at: self.manifest.exported_at,
        })
    }
}

/// Parse the frozen v1 manifest schema and hash its exact source bytes.
/// Duplicate JSON keys retain serde_json's existing last-value behavior to
/// avoid changing the CLI's v1 interpretation during extraction.
pub fn parse_manifest(manifest_bytes: &[u8]) -> Result<ParsedManifest, PackageVerificationError> {
    if manifest_bytes.len() > MAX_MANIFEST_BYTES {
        return Err(PackageVerificationError::ManifestTooLarge);
    }
    let value: Value = serde_json::from_slice(manifest_bytes)
        .map_err(|_| PackageVerificationError::InvalidManifest)?;
    require_exact_fields(
        &value,
        &[
            "format",
            "snapshot_id",
            "source_system_id",
            "target_registry_id",
            "target_organization_id",
            "exported_at",
            "record_count",
            "stream_bytes",
            "stream_sha256",
        ],
    )?;
    if string_field(&value, "format")? != PACKAGE_FORMAT {
        return Err(PackageVerificationError::UnsupportedFormat);
    }
    let snapshot_id = bounded_string(&value, "snapshot_id", 256)?;
    let source_system_id = bounded_string(&value, "source_system_id", 128)?;
    let registry_id = bounded_string(&value, "target_registry_id", 128)?;
    let organization_id = bounded_string(&value, "target_organization_id", 128)?;
    let exported_at = string_field(&value, "exported_at")?.to_owned();
    let record_count = unsigned_field(&value, "record_count")?;
    let stream_bytes = unsigned_field(&value, "stream_bytes")?;
    let stream_sha256 = string_field(&value, "stream_sha256")?;
    if exported_at.trim().is_empty()
        || exported_at.len() > 128
        || record_count > i64::MAX as u64
        || stream_bytes > MAX_PACKAGE_BYTES
        || !is_lower_hex_digest(stream_sha256)
    {
        return Err(PackageVerificationError::InvalidMetadata);
    }
    let mut decoded_stream_sha256 = [0_u8; 32];
    for (index, pair) in stream_sha256.as_bytes().chunks_exact(2).enumerate() {
        decoded_stream_sha256[index] = (hex_nibble(pair[0])? << 4) | hex_nibble(pair[1])?;
    }
    Ok(ParsedManifest {
        run: MigrationRun {
            manifest_sha256: Sha256::digest(manifest_bytes).into(),
            stream_sha256: decoded_stream_sha256,
            snapshot_id,
            source_system_id,
            registry_id,
            organization_id,
            expected_record_count: record_count,
        },
        stream_bytes,
        exported_at,
    })
}

fn require_exact_fields(value: &Value, expected: &[&str]) -> Result<(), PackageVerificationError> {
    let object = value
        .as_object()
        .ok_or(PackageVerificationError::InvalidManifest)?;
    let actual = object
        .keys()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    let expected = expected
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    if actual != expected {
        return Err(PackageVerificationError::InvalidManifest);
    }
    Ok(())
}

fn string_field<'a>(value: &'a Value, key: &str) -> Result<&'a str, PackageVerificationError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or(PackageVerificationError::InvalidMetadata)
}

fn unsigned_field(value: &Value, key: &str) -> Result<u64, PackageVerificationError> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .ok_or(PackageVerificationError::InvalidMetadata)
}

fn bounded_string(
    value: &Value,
    key: &str,
    max_bytes: usize,
) -> Result<String, PackageVerificationError> {
    let field = string_field(value, key)?;
    if field.trim().is_empty() || field.len() > max_bytes || field.chars().any(char::is_control) {
        return Err(PackageVerificationError::InvalidMetadata);
    }
    Ok(field.to_owned())
}

fn is_lower_hex_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn hex_nibble(value: u8) -> Result<u8, PackageVerificationError> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        _ => Err(PackageVerificationError::InvalidMetadata),
    }
}

#[cfg(test)]
fn encode_hex(value: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(value.len() * 2);
    for byte in value {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

#[cfg(test)]
mod tests {
    use std::io::{self, Cursor};

    use super::*;

    fn manifest(stream: &[u8], count: u64) -> Vec<u8> {
        let digest = encode_hex(&Sha256::digest(stream));
        serde_json::json!({
            "format": PACKAGE_FORMAT,
            "snapshot_id": "snapshot-1",
            "source_system_id": "source-1",
            "target_registry_id": "registry-1",
            "target_organization_id": "org-1",
            "exported_at": "2026-10-10T00:00:00Z",
            "record_count": count,
            "stream_bytes": stream.len(),
            "stream_sha256": digest,
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn verifies_exact_manifest_and_stream_commitments() {
        let bytes = b"{\"id\":1}\n{\"id\":2}\n";
        let manifest_bytes = manifest(bytes, 2);

        let verified = verify_package(&manifest_bytes, Cursor::new(bytes)).unwrap();

        assert_eq!(
            verified.run.manifest_sha256,
            Sha256::digest(&manifest_bytes).as_slice()
        );
        assert_eq!(verified.run.stream_sha256, Sha256::digest(bytes).as_slice());
        assert_eq!(verified.stream_bytes, bytes.len() as u64);
        assert_eq!(verified.record_count, 2);
        assert_eq!(verified.run.expected_record_count, 2);
    }

    #[test]
    fn incremental_verifier_preserves_bytes_across_chunk_and_line_boundaries() {
        let bytes = b"{\"id\":1}\n{\"id\":2}\n";
        let manifest_bytes = manifest(bytes, 2);
        let mut verifier = PackageStreamVerifier::new(&manifest_bytes).unwrap();
        verifier.update(&bytes[..5]).unwrap();
        verifier.update(&bytes[5..9]).unwrap();
        verifier.update(&bytes[9..]).unwrap();

        let verified = verifier.finish().unwrap();
        assert_eq!(verified.run.stream_sha256, Sha256::digest(bytes).as_slice());
        assert_eq!(verified.record_count, 2);
    }

    #[test]
    fn rejects_changed_stream_bytes_and_line_endings() {
        let manifest_bytes = manifest(b"{\"id\":1}\n", 1);

        assert!(matches!(
            verify_package(&manifest_bytes, Cursor::new(b"{\"id\":2}\n")),
            Err(PackageVerificationError::StreamCommitmentMismatch)
        ));
        assert!(matches!(
            verify_package(&manifest_bytes, Cursor::new(b"{\"id\":1}")),
            Err(PackageVerificationError::MissingFinalLf)
        ));
    }

    #[test]
    fn rejects_manifest_count_mismatch_and_unknown_fields() {
        let bytes = b"{\"id\":1}\n";
        let count_mismatch = manifest(bytes, 2);
        assert!(matches!(
            verify_package(&count_mismatch, Cursor::new(bytes)),
            Err(PackageVerificationError::RecordCountMismatch)
        ));

        let mut unknown: serde_json::Value = serde_json::from_slice(&manifest(bytes, 1)).unwrap();
        unknown["extra"] = serde_json::json!(true);
        assert!(matches!(
            verify_package(unknown.to_string().as_bytes(), Cursor::new(bytes)),
            Err(PackageVerificationError::InvalidManifest)
        ));
    }

    #[test]
    fn rejects_empty_lines_oversized_lines_and_stream_read_failures() {
        let empty_line = manifest(b"\n", 1);
        assert!(matches!(
            verify_package(&empty_line, Cursor::new(b"\n")),
            Err(PackageVerificationError::InvalidStreamLine)
        ));

        let mut oversized = vec![b'a'; MAX_RECORD_BYTES + 1];
        oversized.push(b'\n');
        let oversized_manifest = manifest(&oversized, 1);
        assert!(matches!(
            verify_package(&oversized_manifest, Cursor::new(oversized)),
            Err(PackageVerificationError::InvalidStreamLine)
        ));

        struct BrokenReader;
        impl Read for BrokenReader {
            fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("synthetic read error"))
            }
        }
        let empty = manifest(b"", 0);
        assert!(matches!(
            verify_package(&empty, BrokenReader),
            Err(PackageVerificationError::StreamRead)
        ));
    }
}
