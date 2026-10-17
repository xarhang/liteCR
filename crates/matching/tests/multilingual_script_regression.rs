//! Locale-tagged synthetic controls for the currently named liteCR languages.
//!
//! The strings below are codepoint mechanics, not culturally reviewed names.
//! This test checks the public advisory matcher contract only; it does not
//! estimate language-specific identity accuracy or acceptable error rates.

use litecr_matching::{
    CANDIDATE_VERSION, ENGINE_VERSION, EvidenceState, FEATURE_VERSION, MatchAssertion, MatchRecord,
    NORMALIZATION_VERSION, POLICY_VERSION, PartialDate, Pass, match_records,
};

struct ScriptCase {
    language: &'static str,
    script: &'static str,
    equivalent_query: &'static str,
    equivalent_candidate: &'static str,
    distinct_mark_neighbor: &'static str,
}

fn record(
    record_id: &str,
    name: &str,
    language: &str,
    script: &str,
    date: Option<PartialDate>,
) -> MatchRecord {
    MatchRecord {
        record_id: record_id.into(),
        registry_id: "synthetic-registry".into(),
        organization_id: "synthetic-organization".into(),
        source_version: Some("synthetic-v1".into()),
        receive_version: Some(1),
        name_assertions: vec![MatchAssertion {
            field_path: "name.original".into(),
            original_value: name.into(),
            language_tag: Some(language.into()),
            script: Some(script.into()),
            representation: Some("source_original".into()),
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
        calendar: Some("gregorian".into()),
        estimated: false,
    }
}

#[test]
fn locale_tagged_original_name_and_partial_date_controls_cover_requested_locales() {
    // Hand-authored canonical-equivalence controls already used by the
    // mechanics suite, plus a constructed English/Latin equivalence pair.
    // Negatives differ by a retained mark or diacritic and share only the
    // deliberately partial birth year with the query.
    let cases = [
        ScriptCase {
            language: "lo",
            script: "Laoo",
            equivalent_query: "ກຸ້",
            equivalent_candidate: "ກຸ້",
            distinct_mark_neighbor: "ກຸ",
        },
        ScriptCase {
            language: "en",
            script: "Latn",
            equivalent_query: "Café",
            equivalent_candidate: "Cafe\u{0301}",
            distinct_mark_neighbor: "Cafe",
        },
        ScriptCase {
            language: "th",
            script: "Thai",
            equivalent_query: "กุ่",
            equivalent_candidate: "กุ่",
            distinct_mark_neighbor: "กุ",
        },
        ScriptCase {
            language: "vi",
            script: "Latn",
            equivalent_query: "Ánh",
            equivalent_candidate: "A\u{0301}nh",
            distinct_mark_neighbor: "Anh",
        },
        ScriptCase {
            language: "hi-IN",
            script: "Deva",
            equivalent_query: "क़लम",
            equivalent_candidate: "क़लम",
            distinct_mark_neighbor: "कलम",
        },
    ];

    for (index, case) in cases.iter().enumerate() {
        let query_id = format!("script-query-{index}");
        let positive_id = format!("script-positive-{index}");
        let hard_negative_id = format!("script-hard-negative-{index}");
        let records = [
            record(
                &query_id,
                case.equivalent_query,
                case.language,
                case.script,
                Some(date("1984", "year")),
            ),
            record(
                &positive_id,
                case.equivalent_candidate,
                case.language,
                case.script,
                Some(date("1984-06-12", "day")),
            ),
            record(
                &hard_negative_id,
                case.distinct_mark_neighbor,
                case.language,
                case.script,
                Some(date("1984-11-03", "day")),
            ),
        ];

        // Query retrieval is bounded and includes the exact-name positive and
        // the distinct-mark record admitted by a weak block, never exact name.
        let result = match_records(&query_id, &records, 2)
            .unwrap_or_else(|error| panic!("{} matcher fixture failed: {error:?}", case.language));
        assert_eq!(result.candidates.len(), 2, "{} candidates", case.language);
        assert!(
            !result.retrieval.truncated,
            "{} must fit the explicit candidate limit",
            case.language
        );
        assert_eq!(result.versions.engine, ENGINE_VERSION);
        assert_eq!(result.versions.normalization, NORMALIZATION_VERSION);
        assert_eq!(result.versions.candidate, CANDIDATE_VERSION);
        assert_eq!(result.versions.feature, FEATURE_VERSION);
        assert_eq!(result.versions.policy, POLICY_VERSION);
        assert_eq!(
            result,
            match_records(&query_id, &records, 2).expect("repeat run must succeed"),
            "{} result/version lineage must be deterministic",
            case.language
        );

        let positive = result
            .candidates
            .iter()
            .find(|candidate| candidate.candidate_record_id == positive_id)
            .unwrap_or_else(|| panic!("{} canonical positive was not retrieved", case.language));
        assert!(
            positive.passes.contains(&Pass::ExactName),
            "{} exact-name pass",
            case.language
        );
        assert_eq!(
            positive
                .fields
                .iter()
                .find(|field| field.field == "name")
                .unwrap()
                .state,
            EvidenceState::Agreement,
            "{} canonical equivalent original names",
            case.language
        );
        assert_eq!(
            positive.candidate_source_version.as_deref(),
            Some("synthetic-v1"),
            "{} candidate source lineage",
            case.language
        );
        assert_eq!(
            positive
                .fields
                .iter()
                .find(|field| field.field == "birth_date")
                .unwrap()
                .state,
            EvidenceState::PartialAgreement,
            "{} year-to-day date evidence",
            case.language
        );

        let negative = result
            .candidates
            .iter()
            .find(|candidate| candidate.candidate_record_id == hard_negative_id)
            .unwrap_or_else(|| {
                panic!(
                    "{} partial-date hard negative was not retrieved",
                    case.language
                )
            });
        assert!(
            negative.passes.contains(&Pass::PartialBirthYear),
            "{} year pass",
            case.language
        );
        assert!(
            !negative.passes.contains(&Pass::ExactName),
            "{} mark must remain significant",
            case.language
        );
        assert!(
            matches!(
                negative
                    .fields
                    .iter()
                    .find(|field| field.field == "name")
                    .unwrap()
                    .state,
                EvidenceState::Unknown | EvidenceState::CorrelatedWeak
            ),
            "{} mark-neighbor is not name agreement",
            case.language
        );
        assert_eq!(
            records[1].name_assertions[0].language_tag.as_deref(),
            Some(case.language),
            "{} locale metadata",
            case.language
        );
        assert_eq!(
            records[1].name_assertions[0].script.as_deref(),
            Some(case.script),
            "{} script metadata",
            case.language
        );
        assert_eq!(
            records[1].name_assertions[0].original_value, case.equivalent_candidate,
            "{} source-original value remains intact",
            case.language
        );
    }
}
