//! Synthetic Unicode and script-boundary checks for candidate retrieval/evidence.
//!
//! These fixtures exercise deterministic invariants only. They are not a labeled
//! cohort and cannot establish identity-matching accuracy or acceptable error rates.

use litecr_matching::{
    EvidenceState, MatchAssertion, MatchRecord, PartialDate, Pass, match_records,
};

fn record(id: &str, name: &str, date: Option<PartialDate>, script: &str) -> MatchRecord {
    MatchRecord {
        record_id: id.into(),
        registry_id: "synthetic-registry".into(),
        organization_id: "synthetic-organization".into(),
        source_version: Some("synthetic-v1".into()),
        receive_version: Some(1),
        name_assertions: vec![MatchAssertion {
            field_path: "name.original".into(),
            original_value: name.into(),
            language_tag: None,
            script: Some(script.into()),
            representation: Some("original".into()),
            normalization_version: None,
        }],
        assertions: vec![],
        date_of_birth: date,
        identifiers: vec![],
        contacts: vec![],
        address_token: None,
    }
}

fn date(value: &str, precision: &str) -> PartialDate {
    PartialDate {
        original_value: value.into(),
        precision: precision.into(),
        calendar: None,
        estimated: false,
    }
}

fn name_evidence(
    candidate: &litecr_matching::CandidateEvidence,
) -> &litecr_matching::FieldEvidence {
    candidate
        .fields
        .iter()
        .find(|field| field.field == "name")
        .expect("matcher must return name evidence")
}

#[test]
fn canonically_equivalent_original_names_are_retrieved_as_exact_name_candidates() {
    // Each pair differs only by canonical Unicode representation. Lao and Thai
    // use the same two marks in different canonical combining-class order; the
    // remaining pairs exercise combining accents/nukta/hamza/breve.
    let pairs = [
        ("lo", "ກຸ້", "ກຸ້", "Lao"),
        ("th", "กุ่", "กุ่", "Thai"),
        ("vi", "Ánh", "A\u{0301}nh", "Latin"),
        ("hi", "क़लम", "क़लम", "Devanagari"),
        ("ar", "أحمد", "ا\u{0654}حمد", "Arabic"),
        ("cy", "Йона", "И\u{0306}она", "Cyrillic"),
        ("latin", "José", "Jose\u{0301}", "Latin"),
    ];

    for (case, query_name, candidate_name, script) in pairs {
        let query_id = format!("{case}-query");
        let candidate_id = format!("{case}-candidate");
        let records = [
            record(&query_id, query_name, None, script),
            record(&candidate_id, candidate_name, None, script),
        ];
        let result = match_records(&query_id, &records, 10)
            .unwrap_or_else(|error| panic!("{case} fixture failed: {error:?}"));

        assert_eq!(result.candidates.len(), 1, "{case} candidate retrieval");
        let candidate = &result.candidates[0];
        assert_eq!(
            candidate.candidate_record_id, candidate_id,
            "{case} candidate"
        );
        assert!(
            candidate.passes.contains(&Pass::ExactName),
            "{case} must be retrieved through the canonical-equivalent name block"
        );
        assert_eq!(
            name_evidence(candidate).state,
            EvidenceState::Agreement,
            "{case} canonical equivalent must have exact-name agreement"
        );
        assert_eq!(
            candidate
                .fields
                .iter()
                .find(|field| field.field == "birth_date")
                .unwrap()
                .state,
            EvidenceState::Unknown,
            "{case} name retrieval must work without a birth date"
        );
        assert_eq!(
            records[1].name_assertions[0].original_value, candidate_name,
            "{case} original assertion must remain unchanged"
        );
    }
}

#[test]
fn khmer_marks_and_script_distinctions_are_preserved_without_invented_transliteration() {
    // Khmer has no canonically decomposable precomposed forms in these fixtures;
    // an orthographic mark remains significant and is not stripped as punctuation.
    let query = record("km-query", "ក", Some(date("1980", "year")), "Khmer");
    let marked_neighbor = record(
        "km-marked-neighbor",
        "ក៏",
        Some(date("1980-05-12", "day")),
        "Khmer",
    );
    let result = match_records("km-query", &[query, marked_neighbor], 10).unwrap();
    assert_eq!(
        result.candidates.len(),
        1,
        "same birth year retrieves the review candidate"
    );
    let candidate = &result.candidates[0];
    assert_eq!(candidate.candidate_record_id, "km-marked-neighbor");
    assert!(candidate.passes.contains(&Pass::PartialBirthYear));
    assert!(!candidate.passes.contains(&Pass::ExactName));
    assert_eq!(name_evidence(candidate).state, EvidenceState::Unknown);
    assert_eq!(
        candidate
            .fields
            .iter()
            .find(|field| field.field == "birth_date")
            .unwrap()
            .state,
        EvidenceState::PartialAgreement
    );

    // The spelling is a synthetic Latin/Cyrillic transliteration-like pair, but
    // no transliteration assertion is supplied. Matching must not infer one.
    let latin = record("latin-query", "Mira", Some(date("1991", "year")), "Latin");
    let cyrillic = record(
        "cyrillic-neighbor",
        "Мира",
        Some(date("1991-08-03", "day")),
        "Cyrillic",
    );
    let result = match_records("latin-query", &[latin, cyrillic], 10).unwrap();
    assert_eq!(
        result.candidates.len(),
        1,
        "birth year may retrieve for human review"
    );
    let candidate = &result.candidates[0];
    assert!(candidate.passes.contains(&Pass::PartialBirthYear));
    assert!(!candidate.passes.contains(&Pass::ExactName));
    assert_eq!(name_evidence(candidate).state, EvidenceState::Unknown);
}

#[test]
fn diacritic_near_neighbors_are_not_collapsed_into_exact_name_matches() {
    let unaccented = record("latin-query", "Jose", Some(date("1977", "year")), "Latin");
    let accented = record(
        "latin-neighbor",
        "José",
        Some(date("1977-11-02", "day")),
        "Latin",
    );
    let result = match_records("latin-query", &[unaccented, accented], 10).unwrap();

    assert_eq!(
        result.candidates.len(),
        1,
        "shared birth year may retrieve for review"
    );
    let candidate = &result.candidates[0];
    assert!(candidate.passes.contains(&Pass::PartialBirthYear));
    assert!(!candidate.passes.contains(&Pass::ExactName));
    assert_eq!(
        name_evidence(candidate).state,
        EvidenceState::CorrelatedWeak
    );
    assert_eq!(name_evidence(candidate).points, 0);
}

#[test]
fn unicode_whitespace_runs_collapse_but_token_boundaries_remain() {
    let query = record("space-query", " Mi\u{00a0}  ra ", None, "Latin");
    let candidate = record("space-candidate", "Mi\t ra", None, "Latin");
    let equivalent = [query.clone(), candidate];
    let result = match_records("space-query", &equivalent, 10).unwrap();

    assert_eq!(result.candidates.len(), 1);
    let candidate = &result.candidates[0];
    assert!(candidate.passes.contains(&Pass::ExactName));
    assert_eq!(name_evidence(candidate).state, EvidenceState::Agreement);
    assert_eq!(
        equivalent[0].name_assertions[0].original_value,
        " Mi\u{00a0}  ra "
    );

    // Removing all whitespace merged distinct tokenizations and could create
    // exact-name evidence. Retrieval may shortlist it with zero name points.
    let compact = record("compact-query", "Mira", None, "Latin");
    let separated = record("separated-candidate", "Mi ra", None, "Latin");
    let result = match_records("compact-query", &[compact, separated], 10).unwrap();
    assert_eq!(result.candidates.len(), 1);
    assert!(!result.candidates[0].passes.contains(&Pass::ExactName));
    assert_eq!(name_evidence(&result.candidates[0]).points, 0);
}

#[test]
fn punctuation_fold_is_exercised_for_the_currently_claimed_ui_scripts() {
    // Deliberately synthetic labels, not examples of culturally valid names.
    // This checks only the documented ASCII-punctuation folding mechanics for
    // the scripts currently shown by the UI locale samples.
    let cases = [
        ("en", "Example-Alpha", "ExampleAlpha", "Latin"),
        ("lo", "ທົດ-ສອບ", "ທົດສອບ", "Lao"),
        ("th", "ตัว-อย่าง", "ตัวอย่าง", "Thai"),
        ("vi", "Mau-Thu", "MauThu", "Latin"),
        ("hi-IN", "नमूना-कख", "नमूनाकख", "Devanagari"),
        ("ar", "مثال-أب", "مثالأب", "Arabic"),
    ];

    for (case, query_name, candidate_name, script) in cases {
        let records = [
            record(&format!("{case}-punct-query"), query_name, None, script),
            record(
                &format!("{case}-punct-candidate"),
                candidate_name,
                None,
                script,
            ),
        ];
        let result = match_records(&records[0].record_id, &records, 10)
            .unwrap_or_else(|error| panic!("{case} fixture failed: {error:?}"));

        assert_eq!(result.candidates.len(), 1, "{case} candidate retrieval");
        let candidate = &result.candidates[0];
        assert!(candidate.passes.contains(&Pass::ExactName), "{case} pass");
        assert_eq!(
            name_evidence(candidate).state,
            EvidenceState::Agreement,
            "{case}"
        );
        assert_eq!(records[1].name_assertions[0].original_value, candidate_name);
    }
}

#[test]
fn script_marks_and_diacritics_remain_distinct_in_locale_sample_scripts() {
    // These constructed near-neighbors ensure that normalization does not
    // strip script marks. The matching birth year intentionally supplies the
    // only retrieval block, so this is a negative exact-name control.
    let cases = [
        ("lo", "ກາ", "ກ່າ", "Lao"),
        ("th", "กข", "ก่ข", "Thai"),
        ("vi", "An", "Án", "Latin"),
        ("hi-IN", "कलम", "क़लम", "Devanagari"),
        ("ar", "امل", "أمل", "Arabic"),
        ("km", "កក", "ក៏ក", "Khmer"),
    ];

    for (case, query_name, candidate_name, script) in cases {
        let query = record(
            &format!("{case}-mark-query"),
            query_name,
            Some(date("1984", "year")),
            script,
        );
        let candidate = record(
            &format!("{case}-mark-candidate"),
            candidate_name,
            Some(date("1984-06-12", "day")),
            script,
        );
        let records = [query, candidate];
        let result = match_records(&records[0].record_id, &records, 10)
            .unwrap_or_else(|error| panic!("{case} fixture failed: {error:?}"));

        assert_eq!(result.candidates.len(), 1, "{case} date-block retrieval");
        let candidate = &result.candidates[0];
        assert!(
            candidate.passes.contains(&Pass::PartialBirthYear),
            "{case} date pass"
        );
        assert!(
            !candidate.passes.contains(&Pass::ExactName),
            "{case} name pass"
        );
        assert_eq!(
            name_evidence(candidate).state,
            EvidenceState::Unknown,
            "{case} name evidence"
        );
    }
}
