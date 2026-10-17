use litecr_application::migration::{MigrationRecord, MigrationRun};
use litecr_infrastructure::migration::{MigrationRowRefusal, map_supported_record};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn input(row: Value) -> (MigrationRun, MigrationRecord) {
    let raw = serde_json::to_vec(&row).unwrap();
    let canonical = serde_json::to_vec(&row).unwrap();
    let record = MigrationRecord {
        source_system_id: "synthetic-his".into(),
        source_record_key: "record-1".into(),
        source_version: "v1".into(),
        payload_sha256: Sha256::digest(canonical).into(),
        raw_ndjson: [raw, b"\n".to_vec()].concat(),
    };
    let run = MigrationRun {
        manifest_sha256: [1; 32],
        stream_sha256: [2; 32],
        snapshot_id: "synthetic-snapshot".into(),
        source_system_id: "synthetic-his".into(),
        registry_id: "synthetic-registry".into(),
        organization_id: "synthetic-organization".into(),
        expected_record_count: 1,
    };
    (run, record)
}

fn supported_row() -> Value {
    json!({
        "source_system_id": "synthetic-his",
        "source_record_key": "record-1",
        "source_version": "v1",
        "public_id": "public-1",
        "cruid": "legacy-1",
        "aliases": ["legacy-alt-1"],
        "assertions": [
            {"field_path":"name.original","original_value":"ຄຳ ຕົວຢ່າງ","language_tag":"lo","script":"Lao"},
            {"field_path":"birthDate","original_value":"1984-06","partial_date_precision":"month"}
        ],
        "identifiers": [{"authority":"urn:synthetic:clinic","type":"MRN","value":"0007","authoritative":true}]
    })
}

#[test]
fn maps_supported_source_fields_and_accepts_omitted_relationships() {
    let (run, record) = input(supported_row());
    let mapped = map_supported_record(&run, &record).unwrap();
    assert_eq!(mapped.preserved_public_id.as_deref(), Some("public-1"));
    assert_eq!(mapped.preserved_cruid.as_deref(), Some("legacy-1"));
    assert_eq!(mapped.aliases, ["legacy-alt-1"]);
    assert_eq!(mapped.command.assertions[1].original_value, "1984-06");
    assert_eq!(
        mapped.command.assertions[0].language_tag.as_deref(),
        Some("lo")
    );
    assert_eq!(mapped.command.identifiers[0].value, "0007");
    // Partial-date precision is retained in the complete source row in the journal;
    // the source assertion preserves the exact original date string.
}

#[test]
fn refuses_relationship_facts_and_unknown_fields_before_database_work() {
    let mut row = supported_row();
    row["relationships"] = json!([{
        "kind":"link",
        "target_source_system_id":"synthetic-his",
        "target_source_record_key":"record-2"
    }]);
    let (run, record) = input(row);
    assert_eq!(
        map_supported_record(&run, &record),
        Err(MigrationRowRefusal::HistoricalRelationshipsUnsupported)
    );

    let mut row = supported_row();
    row["new_unmodeled_field"] = json!(true);
    let (run, record) = input(row);
    assert_eq!(
        map_supported_record(&run, &record),
        Err(MigrationRowRefusal::UnsupportedField)
    );
}

#[test]
fn rejects_invalid_partial_date_precision_and_ambiguous_aliases() {
    let mut row = supported_row();
    row["assertions"][1]["partial_date_precision"] = json!("week");
    let (run, record) = input(row);
    assert_eq!(
        map_supported_record(&run, &record),
        Err(MigrationRowRefusal::RegistryValidation)
    );

    let mut row = supported_row();
    row["aliases"] = json!(["public-1"]);
    let (run, record) = input(row);
    assert_eq!(
        map_supported_record(&run, &record),
        Err(MigrationRowRefusal::InvalidAliases)
    );
}
