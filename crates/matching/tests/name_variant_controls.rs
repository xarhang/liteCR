use litecr_matching::{AdvisoryOutcome, MatchAssertion, MatchRecord, Pass, match_records};
use serde::Deserialize;

#[derive(Deserialize)]
struct Controls {
    cases: Vec<Case>,
}
#[derive(Deserialize)]
struct Case {
    case: String,
    left: String,
    right: String,
    retrieve: bool,
}

fn record(id: &str, name: &str) -> MatchRecord {
    MatchRecord {
        record_id: id.into(),
        registry_id: "synthetic".into(),
        organization_id: "synthetic".into(),
        source_version: Some("v1".into()),
        receive_version: Some(1),
        name_assertions: vec![MatchAssertion {
            field_path: "name.original".into(),
            original_value: name.into(),
            language_tag: None,
            script: None,
            representation: Some("source_original".into()),
            normalization_version: None,
        }],
        assertions: vec![],
        date_of_birth: None,
        identifiers: vec![],
        contacts: vec![],
        address_token: None,
    }
}

#[test]
fn spelling_and_incomplete_name_controls_are_symmetric_weak_and_script_safe() {
    let controls: Controls = serde_json::from_str(include_str!(
        "../../../fixtures/matching/name-variant-controls-v1.json"
    ))
    .unwrap();
    for case in controls.cases {
        for (a, b) in [(&case.left, &case.right), (&case.right, &case.left)] {
            let result =
                match_records("query", &[record("query", a), record("candidate", b)], 10).unwrap();
            assert_eq!(
                !result.candidates.is_empty(),
                case.retrieve,
                "{}: {a:?} / {b:?}",
                case.case
            );
            for candidate in result.candidates {
                assert!(
                    !candidate.passes.contains(&Pass::ExactName),
                    "{} must stay non-exact",
                    case.case
                );
                assert_eq!(candidate.score_points, 0);
                assert_eq!(candidate.outcome, AdvisoryOutcome::InsufficientEvidence);
                assert!(
                    candidate
                        .fields
                        .iter()
                        .any(|f| f.rationale_code == "DEMOGRAPHICS_NOT_PERSON_SPECIFIC")
                );
            }
        }
    }
}

#[test]
fn derived_names_and_short_initials_do_not_open_tolerant_retrieval() {
    for representation in ["transliterated", "normalized", "unknown"] {
        let mut other = record("candidate", "Somchay Phom");
        other.name_assertions[0].representation = Some(representation.into());
        assert!(
            match_records("query", &[record("query", "Somchai Phommasane"), other], 10)
                .unwrap()
                .candidates
                .is_empty()
        );
    }
}
