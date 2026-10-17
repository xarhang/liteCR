//! Deterministic, versioned, advisory-only matching primitives.
#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use unicode_normalization::UnicodeNormalization;

pub const ENGINE_VERSION: &str = "litecr-match-v2";
pub const NORMALIZATION_VERSION: &str =
    "unicode-nfc-case-collapsed-whitespace-ascii-punctuation-fold-v4-exact-authoritative-ids";
pub const CANDIDATE_VERSION: &str = "union-blocks-prioritized-v7-bounded-name-variants";
pub const FEATURE_VERSION: &str = "explainable-fields-v7-weak-name-retrieval";
pub const POLICY_VERSION: &str = "advisory-review-v4-weak-name-ambiguity";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MatchAssertion {
    pub field_path: String,
    pub original_value: String,
    #[serde(default)]
    pub language_tag: Option<String>,
    #[serde(default)]
    pub script: Option<String>,
    #[serde(default)]
    pub representation: Option<String>,
    #[serde(default)]
    pub normalization_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MatchIdentifier {
    pub authority: String,
    pub identifier_type: String,
    pub value: String,
    #[serde(default)]
    pub authoritative: bool,
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MatchRecord {
    pub record_id: String,
    #[serde(default)]
    pub registry_id: String,
    #[serde(default)]
    pub organization_id: String,
    #[serde(default)]
    pub source_version: Option<String>,
    #[serde(default)]
    pub receive_version: Option<i64>,
    #[serde(default)]
    pub name_assertions: Vec<MatchAssertion>,
    #[serde(default)]
    pub assertions: Vec<MatchAssertion>,
    #[serde(default)]
    pub date_of_birth: Option<PartialDate>,
    #[serde(default)]
    pub identifiers: Vec<MatchIdentifier>,
    #[serde(default)]
    pub contacts: Vec<ContactToken>,
    #[serde(default)]
    pub address_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PartialDate {
    pub original_value: String,
    pub precision: String,
    #[serde(default)]
    pub calendar: Option<String>,
    #[serde(default)]
    pub estimated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContactToken {
    pub kind: String,
    pub synthetic_token: String,
    #[serde(default)]
    pub shared_or_recycled_possible: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Pass {
    ExactName,
    PartialBirthYear,
    AuthoritativeIdentifier,
    NameVariant,
    IncompleteName,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceState {
    Agreement,
    PartialAgreement,
    Conflict,
    Unknown,
    CorrelatedWeak,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FieldEvidence {
    pub field: String,
    pub state: EvidenceState,
    pub points: i16,
    pub rationale_code: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AdvisoryOutcome {
    Review,
    InsufficientEvidence,
    ConflictReview,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CandidateEvidence {
    pub candidate_record_id: String,
    pub candidate_source_version: Option<String>,
    pub candidate_receive_version: Option<i64>,
    pub passes: Vec<Pass>,
    pub score_points: i16,
    pub score_type: String,
    pub outcome: AdvisoryOutcome,
    pub fields: Vec<FieldEvidence>,
    pub hard_conflicts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MatcherVersions {
    pub engine: String,
    pub normalization: String,
    pub candidate: String,
    pub feature: String,
    pub policy: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RetrievalTelemetry {
    pub per_pass_candidates: BTreeMap<Pass, usize>,
    pub union_before_limit: usize,
    pub returned_candidates: usize,
    pub truncated: bool,
    pub limit: usize,
    /// False only for evidence predating persisted retrieval telemetry. Its counts
    /// are lower bounds reconstructed from the retained candidate rows.
    #[serde(default = "default_telemetry_complete")]
    pub complete: bool,
}

fn default_telemetry_complete() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MatchResult {
    pub versions: MatcherVersions,
    /// Database-assigned immutable evidence snapshot ID. Pure matcher calls
    /// leave it empty; persisted matching adapters populate it before return.
    #[serde(default)]
    pub evidence_run_id: Option<String>,
    pub query_record_id: String,
    pub query_source_version: Option<String>,
    pub query_receive_version: Option<i64>,
    pub candidates: Vec<CandidateEvidence>,
    pub retrieval: RetrievalTelemetry,
    pub mutation_authority: String,
    /// True when this response is the exact persisted matcher output. False for
    /// legacy runs reconstructed from candidate rows without a full snapshot.
    #[serde(default = "default_snapshot_complete")]
    pub snapshot_complete: bool,
}

fn default_snapshot_complete() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub max_records: usize,
    pub max_candidates: usize,
    pub max_assertions_per_record: usize,
    pub max_value_bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_records: 10_000,
            max_candidates: 200,
            max_assertions_per_record: 256,
            max_value_bytes: 16_384,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchError {
    InvalidLimit,
    TooManyRecords,
    TooManyAssertions,
    ValueTooLarge,
    DuplicateRecordId,
    ScopeMismatch,
    QueryNotFound,
}

pub fn match_records(
    query_id: &str,
    records: &[MatchRecord],
    limit: usize,
) -> Result<MatchResult, MatchError> {
    match_records_bounded(query_id, records, limit, Limits::default())
}

pub fn match_records_bounded(
    query_id: &str,
    records: &[MatchRecord],
    limit: usize,
    bounds: Limits,
) -> Result<MatchResult, MatchError> {
    if limit == 0 || limit > bounds.max_candidates {
        return Err(MatchError::InvalidLimit);
    }
    if records.len() > bounds.max_records {
        return Err(MatchError::TooManyRecords);
    }
    let mut ids = BTreeSet::new();
    for r in records {
        if !ids.insert(r.record_id.as_str()) {
            return Err(MatchError::DuplicateRecordId);
        }
        if r.name_assertions.len() + r.assertions.len() > bounds.max_assertions_per_record {
            return Err(MatchError::TooManyAssertions);
        }
        if r.name_assertions
            .iter()
            .chain(&r.assertions)
            .any(|a| a.original_value.len() > bounds.max_value_bytes)
        {
            return Err(MatchError::ValueTooLarge);
        }
    }
    let query = records
        .iter()
        .find(|r| r.record_id == query_id)
        .ok_or(MatchError::QueryNotFound)?;
    if records
        .iter()
        .any(|r| r.registry_id != query.registry_id || r.organization_id != query.organization_id)
    {
        return Err(MatchError::ScopeMismatch);
    }

    let mut by_pass: BTreeMap<Pass, BTreeSet<&str>> = BTreeMap::new();
    for other in records.iter().filter(|r| r.record_id != query_id) {
        if same_normalized_name(query, other) {
            by_pass
                .entry(Pass::ExactName)
                .or_default()
                .insert(&other.record_id);
        } else if let Some(pass) = tolerant_name_pass(query, other) {
            by_pass.entry(pass).or_default().insert(&other.record_id);
        }
        if same_known_birth_year(query, other) {
            by_pass
                .entry(Pass::PartialBirthYear)
                .or_default()
                .insert(&other.record_id);
        }
        if shares_authoritative_identifier(query, other) {
            by_pass
                .entry(Pass::AuthoritativeIdentifier)
                .or_default()
                .insert(&other.record_id);
        }
    }
    let mut passes_for: BTreeMap<&str, Vec<Pass>> = BTreeMap::new();
    for (pass, candidate_ids) in &by_pass {
        for id in candidate_ids {
            passes_for.entry(id).or_default().push(*pass);
        }
    }
    let union_before_limit = passes_for.len();
    let per_pass_candidates = by_pass.iter().map(|(p, ids)| (*p, ids.len())).collect();
    // Prefer stronger blocking evidence before applying the visible global limit.
    // Otherwise lexicographic record IDs can crowd out a direct authoritative-ID
    // hit with arbitrarily many weaker birth-year-only candidates.
    let mut ordered: Vec<_> = passes_for.iter().collect();
    ordered.sort_by_key(|(id, passes)| {
        let strength = if passes.contains(&Pass::AuthoritativeIdentifier) {
            0
        } else if passes.contains(&Pass::ExactName) {
            1
        } else if passes.contains(&Pass::PartialBirthYear) {
            2
        } else {
            3
        };
        (strength, std::cmp::Reverse(passes.len()), **id)
    });
    let selected: Vec<&str> = ordered.into_iter().take(limit).map(|(id, _)| *id).collect();
    let candidates = selected
        .iter()
        .map(|id| {
            let record = records
                .iter()
                .find(|r| r.record_id == *id)
                .expect("candidate came from input");
            let mut passes = passes_for.remove(id).unwrap_or_default();
            passes.sort_by_key(|p| *p as u8);
            score_pair(query, record, passes)
        })
        .collect::<Vec<_>>();
    Ok(MatchResult {
        versions: MatcherVersions {
            engine: ENGINE_VERSION.into(),
            normalization: NORMALIZATION_VERSION.into(),
            candidate: CANDIDATE_VERSION.into(),
            feature: FEATURE_VERSION.into(),
            policy: POLICY_VERSION.into(),
        },
        evidence_run_id: None,
        query_record_id: query.record_id.clone(),
        query_source_version: query.source_version.clone(),
        query_receive_version: query.receive_version,
        retrieval: RetrievalTelemetry {
            per_pass_candidates,
            union_before_limit,
            returned_candidates: candidates.len(),
            truncated: union_before_limit > limit,
            limit,
            complete: true,
        },
        candidates,
        mutation_authority: "registry-only; advisory output cannot link or merge identities".into(),
        snapshot_complete: true,
    })
}

fn assertions(r: &MatchRecord) -> impl Iterator<Item = &MatchAssertion> {
    r.name_assertions.iter().chain(r.assertions.iter())
}
fn is_name(a: &MatchAssertion) -> bool {
    // This broad hint is used only to report that a name-like source value was
    // present. Candidate generation and Agreement evidence use the closed path
    // policy below.
    a.field_path.to_ascii_lowercase().contains("name")
}

/// Whether a source assertion path explicitly denotes the raw name value used
/// for exact blocking and name evidence. Unrecognized, serialized FHIR, and
/// derived paths stay available as source provenance but are never name evidence.
pub fn is_matchable_original_name_field_path(field_path: &str) -> bool {
    field_path.eq_ignore_ascii_case("name") || field_path.eq_ignore_ascii_case("name.original")
}

fn is_source_original_name(a: &MatchAssertion) -> bool {
    // Missing metadata is accepted for legacy pure-matcher inputs. Unknown or
    // derived representation tags must never become exact-name identity evidence.
    matches!(
        a.representation.as_deref(),
        None | Some("original" | "source_original")
    )
}
// Fold canonically equivalent spellings to NFC, then fold case, Unicode
// whitespace runs, and ASCII punctuation. A whitespace boundary remains a
// boundary so distinct tokenizations such as "Mira" and "Mi ra" do not collide.
// Combining marks and script-specific signs remain; this is not compatibility
// folding or transliteration.
/// Versioned key function shared with the PostgreSQL retrieval index writer.
/// Keep this behavior aligned with `NORMALIZATION_VERSION`.
pub fn normalized_assertion_key(s: &str) -> String {
    let folded = s
        .chars()
        .nfc()
        .flat_map(char::to_lowercase)
        .nfc()
        .filter(|c| !c.is_ascii_punctuation())
        .collect::<String>();
    folded.split_whitespace().collect::<Vec<_>>().join(" ")
}
fn normalized(s: &str) -> String {
    normalized_assertion_key(s)
}
fn normalized_identifier(s: &str) -> Option<String> {
    // The authority and identifier type determine which transformations are
    // safe. Until such a policy is carried by the contract, use normalization
    // only to reject empty/punctuation-only values and compare the source value
    // exactly. Applying name normalization here could turn distinct IDs such
    // as `AB-12` and `AB12` (or `00123` and `123`) into a false authoritative
    // agreement.
    authoritative_identifier_key(s)
}

/// Index key for authoritative identifiers. Preserve the exact source spelling
/// until an issuer/type-specific equivalence policy is defined. The name
/// normalizer is used only to reject empty or punctuation-only inputs.
pub fn authoritative_identifier_key(s: &str) -> Option<String> {
    (!normalized(s).is_empty()).then(|| s.to_owned())
}
fn same_authoritative_identifier(a: &MatchIdentifier, b: &MatchIdentifier) -> bool {
    a.authoritative
        && b.authoritative
        && a.authority == b.authority
        && a.identifier_type == b.identifier_type
        && normalized_identifier(&a.value)
            .zip(normalized_identifier(&b.value))
            .is_some_and(|(x, y)| x == y)
}
fn same_normalized_name(a: &MatchRecord, b: &MatchRecord) -> bool {
    assertions(a)
        .filter(|x| {
            is_matchable_original_name_field_path(&x.field_path) && is_source_original_name(x)
        })
        .any(|x| {
            assertions(b)
                .filter(|y| {
                    is_matchable_original_name_field_path(&y.field_path)
                        && is_source_original_name(y)
                })
                .any(|y| {
                    normalized(&x.original_value).len() >= 2
                        && normalized(&x.original_value) == normalized(&y.original_value)
                })
        })
}
// Retrieval only: never turn a spelling similarity or a partial name into an
// exact-name agreement. Keep the original script and source assertions intact.
fn tolerant_name_pass(a: &MatchRecord, b: &MatchRecord) -> Option<Pass> {
    let original_names = |r: &MatchRecord| {
        assertions(r)
            .filter(|x| {
                is_matchable_original_name_field_path(&x.field_path) && is_source_original_name(x)
            })
            .map(|x| normalized(&x.original_value))
            .filter(|name| (4..=128).contains(&name.chars().count()))
            .collect::<Vec<_>>()
    };
    let left = original_names(a);
    let right = original_names(b);
    let mut partial = false;
    for left_name in &left {
        for right_name in &right {
            // Optional source labels are provenance, not a prerequisite for
            // native-script retrieval. Compare actual Unicode script extensions.
            let x_script = unicode_script::ScriptExtension::for_str(left_name);
            let y_script = unicode_script::ScriptExtension::for_str(right_name);
            let same_script = !x_script.is_common()
                && !x_script.is_inherited()
                && !y_script.is_common()
                && !y_script.is_inherited()
                && !x_script.intersection(y_script).is_empty();
            if !same_script || left_name == right_name {
                continue;
            }
            let x: Vec<char> = left_name.chars().collect();
            let y: Vec<char> = right_name.chars().collect();
            let cutoff = if x.len().min(y.len()) >= 8 { 2 } else { 1 };
            if bounded_name_distance(&x, &y, cutoff) {
                return Some(Pass::NameVariant);
            }
            if token_prefix_subset(left_name, right_name)
                || token_prefix_subset(right_name, left_name)
            {
                partial = true;
            }
        }
    }
    partial.then_some(Pass::IncompleteName)
}

// Banded optimal-string-alignment distance over Unicode scalars, including an
// adjacent transposition. This is a versioned heuristic, not linguistic or
// grapheme equivalence; marks are preserved and may consume an edit.
fn bounded_name_distance(a: &[char], b: &[char], cutoff: usize) -> bool {
    if a.len().abs_diff(b.len()) > cutoff {
        return false;
    }
    let infinity = cutoff + 1;
    let mut previous_previous = vec![infinity; b.len() + 1];
    let mut previous: Vec<_> = (0..=b.len()).map(|j| j.min(infinity)).collect();
    for i in 1..=a.len() {
        let mut current = vec![infinity; b.len() + 1];
        current[0] = i.min(infinity);
        for j in i.saturating_sub(cutoff).max(1)..=b.len().min(i + cutoff) {
            current[j] = (previous[j] + 1)
                .min(current[j - 1] + 1)
                .min(previous[j - 1] + usize::from(a[i - 1] != b[j - 1]));
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                current[j] = current[j].min(previous_previous[j - 2] + 1);
            }
        }
        previous_previous = previous;
        previous = current;
    }
    previous[b.len()] <= cutoff
}

// Whitespace-delimited parts only; no invented segmentation for Lao/Thai.
// Each supplied part must occupy a distinct full-name part. Initials and very
// short parts do not qualify. Reordering is permitted without inferring roles.
fn token_prefix_compatible(short: &str, full: &str) -> bool {
    if full.starts_with(short) {
        return true;
    }
    let short: Vec<_> = short.chars().collect();
    let full: Vec<_> = full.chars().collect();
    short.len() >= 4
        && (short.len().saturating_sub(1)..=(short.len() + 1).min(full.len()))
            .any(|n| bounded_name_distance(&short, &full[..n], 1))
}

fn token_prefix_subset(short: &str, full: &str) -> bool {
    let short: Vec<_> = short.split_whitespace().collect();
    let full: Vec<_> = full.split_whitespace().collect();
    if short.is_empty()
        || short.len() > full.len()
        || full.len() > 8
        || short.iter().any(|s| s.chars().count() < 3)
    {
        return false;
    }
    fn assign(
        i: usize,
        short: &[&str],
        full: &[&str],
        seen: &mut [bool],
        owners: &mut [Option<usize>],
    ) -> bool {
        for j in 0..full.len() {
            if !seen[j] && token_prefix_compatible(short[i], full[j]) {
                seen[j] = true;
                if owners[j].is_none() || assign(owners[j].unwrap(), short, full, seen, owners) {
                    owners[j] = Some(i);
                    return true;
                }
            }
        }
        false
    }
    let mut owners = vec![None; full.len()];
    (0..short.len()).all(|i| assign(i, &short, &full, &mut vec![false; full.len()], &mut owners))
}

#[derive(Debug, Clone, Copy)]
struct ValidatedDate<'a> {
    original_value: &'a str,
    year: &'a str,
    precision: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DateRelation {
    Exact,
    SameYear,
    DifferentYear,
    KnownComponentConflict,
}

fn valid_date(date: &PartialDate) -> Option<ValidatedDate<'_>> {
    let value = date.original_value.as_str();
    let (year, month, day) = match value.len() {
        4 if value.bytes().all(|b| b.is_ascii_digit()) => (value, None, None),
        7 if value.as_bytes().get(4) == Some(&b'-') => {
            let year = &value[..4];
            let month = &value[5..];
            let month_number = month.parse::<u8>().ok()?;
            if !year.bytes().all(|b| b.is_ascii_digit())
                || !month.bytes().all(|b| b.is_ascii_digit())
                || !(1..=12).contains(&month_number)
            {
                return None;
            }
            (year, Some(month), None)
        }
        10 if value.as_bytes().get(4) == Some(&b'-') && value.as_bytes().get(7) == Some(&b'-') => {
            let year = &value[..4];
            let month = &value[5..7];
            let day = &value[8..10];
            if !year.bytes().all(|b| b.is_ascii_digit())
                || !month.bytes().all(|b| b.is_ascii_digit())
                || !day.bytes().all(|b| b.is_ascii_digit())
            {
                return None;
            }
            let year_number = year.parse::<u16>().ok()?;
            let month_number = month.parse::<u8>().ok()?;
            let day_number = day.parse::<u8>().ok()?;
            let max_day = match month_number {
                1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
                4 | 6 | 9 | 11 => 30,
                2 if year_number % 400 == 0 || (year_number % 4 == 0 && year_number % 100 != 0) => {
                    29
                }
                2 => 28,
                _ => return None,
            };
            if !(1..=max_day).contains(&day_number) {
                return None;
            }
            (year, Some(month), Some(day))
        }
        _ => return None,
    };
    let precision_matches = match date.precision.as_str() {
        "year" => month.is_none(),
        "month" => month.is_some() && day.is_none(),
        "day" => day.is_some(),
        _ => false,
    };
    if year.parse::<u16>().ok()? == 0 {
        return None;
    }
    precision_matches.then_some(ValidatedDate {
        original_value: value,
        year,
        precision: date.precision.as_str(),
    })
}

fn calendars_comparable(a: &PartialDate, b: &PartialDate) -> bool {
    match (&a.calendar, &b.calendar) {
        (None, None) => true,
        (Some(left), Some(right)) => {
            let left = left.trim();
            let right = right.trim();
            !left.is_empty()
                && !right.is_empty()
                && (left.eq_ignore_ascii_case("gregorian")
                    || left.eq_ignore_ascii_case("iso8601")
                    || left.eq_ignore_ascii_case("iso-8601"))
                && left.eq_ignore_ascii_case(right)
        }
        _ => false,
    }
}

fn date_context(record: &MatchRecord) -> Option<Cow<'_, PartialDate>> {
    if let Some(date) = record.date_of_birth.as_ref() {
        return Some(Cow::Borrowed(date));
    }
    let value = first_value(record, |assertion| {
        assertion.field_path.eq_ignore_ascii_case("birth_date")
            || assertion.field_path.eq_ignore_ascii_case("date_of_birth")
    })?;
    let precision = match value.len() {
        4 => "year",
        7 => "month",
        10 => "day",
        _ => "unknown",
    };
    Some(Cow::Owned(PartialDate {
        original_value: value.into(),
        precision: precision.into(),
        calendar: None,
        estimated: false,
    }))
}

fn date_relation(a: &PartialDate, b: &PartialDate) -> Option<DateRelation> {
    if a.estimated || b.estimated || !calendars_comparable(a, b) {
        return None;
    }
    let left = valid_date(a)?;
    let right = valid_date(b)?;
    Some(if left.year != right.year {
        DateRelation::DifferentYear
    } else if left.precision == right.precision && left.original_value == right.original_value {
        DateRelation::Exact
    } else if (left.original_value.len() >= 7
        && right.original_value.len() >= 7
        && left.original_value[5..7] != right.original_value[5..7])
        || (left.original_value.len() == 10
            && right.original_value.len() == 10
            && left.original_value[8..10] != right.original_value[8..10])
    {
        DateRelation::KnownComponentConflict
    } else {
        DateRelation::SameYear
    })
}

fn same_known_birth_year(a: &MatchRecord, b: &MatchRecord) -> bool {
    date_context(a)
        .zip(date_context(b))
        .and_then(|(left, right)| date_relation(left.as_ref(), right.as_ref()))
        .is_some_and(|relation| {
            matches!(
                relation,
                DateRelation::Exact | DateRelation::SameYear | DateRelation::KnownComponentConflict
            )
        })
}
fn shares_authoritative_identifier(a: &MatchRecord, b: &MatchRecord) -> bool {
    a.identifiers.iter().any(|x| {
        b.identifiers
            .iter()
            .any(|y| same_authoritative_identifier(x, y))
    })
}
fn first_value(r: &MatchRecord, pred: impl Fn(&MatchAssertion) -> bool) -> Option<&str> {
    assertions(r)
        .find(|a| pred(a))
        .map(|a| a.original_value.as_str())
}
fn score_pair(a: &MatchRecord, b: &MatchRecord, passes: Vec<Pass>) -> CandidateEvidence {
    let mut fields = Vec::new();
    let mut hard_conflicts = Vec::new();
    let name_a = first_value(a, is_name);
    let name_b = first_value(b, is_name);
    fields.push(if same_normalized_name(a, b) {
        FieldEvidence {
            field: "name".into(),
            state: EvidenceState::Agreement,
            points: 35,
            rationale_code: "NAME_NORMALIZED_EXACT".into(),
        }
    } else if let Some(pass) = passes
        .iter()
        .find(|p| matches!(p, Pass::NameVariant | Pass::IncompleteName))
    {
        FieldEvidence {
            field: "name".into(),
            state: EvidenceState::CorrelatedWeak,
            points: 0,
            rationale_code: match pass {
                Pass::NameVariant => "NAME_VARIANT_RETRIEVAL_ONLY",
                _ => "NAME_INCOMPLETE_RETRIEVAL_ONLY",
            }
            .into(),
        }
    } else {
        match (name_a, name_b) {
            (Some(_), Some(_)) => FieldEvidence {
                field: "name".into(),
                state: EvidenceState::Unknown,
                points: 0,
                rationale_code: "NAME_NO_EXACT_BLOCK".into(),
            },
            _ => FieldEvidence {
                field: "name".into(),
                state: EvidenceState::Unknown,
                points: 0,
                rationale_code: "NAME_MISSING".into(),
            },
        }
    });
    let dob_pair = date_context(a)
        .zip(date_context(b))
        .and_then(|(left, right)| date_relation(left.as_ref(), right.as_ref()));
    fields.push(match dob_pair {
        Some(DateRelation::Exact) => FieldEvidence {
            field: "birth_date".into(),
            state: EvidenceState::Agreement,
            points: 30,
            rationale_code: "DOB_SAME_PRECISION_VALUE".into(),
        },
        Some(DateRelation::SameYear) => FieldEvidence {
            field: "birth_date".into(),
            state: EvidenceState::PartialAgreement,
            points: 10,
            rationale_code: "DOB_PARTIAL_YEAR_AGREEMENT".into(),
        },
        Some(DateRelation::KnownComponentConflict) => {
            hard_conflicts.push("DOB_KNOWN_COMPONENT_CONFLICT".into());
            FieldEvidence {
                field: "birth_date".into(),
                state: EvidenceState::Conflict,
                points: -25,
                rationale_code: "DOB_KNOWN_COMPONENT_CONFLICT".into(),
            }
        }
        Some(DateRelation::DifferentYear) => {
            hard_conflicts.push("DOB_YEAR_CONFLICT".into());
            FieldEvidence {
                field: "birth_date".into(),
                state: EvidenceState::Conflict,
                points: -25,
                rationale_code: "DOB_YEAR_CONFLICT".into(),
            }
        }
        _ => FieldEvidence {
            field: "birth_date".into(),
            state: EvidenceState::Unknown,
            points: 0,
            rationale_code: "DOB_UNKNOWN_OR_UNPARSEABLE".into(),
        },
    });
    let id_collision = a.identifiers.iter().any(|x| {
        b.identifiers.iter().any(|y| {
            same_authoritative_identifier(x, y)
                && (x.status.as_deref() == Some("conflicting")
                    || y.status.as_deref() == Some("conflicting"))
        })
    });
    let id_match = a.identifiers.iter().any(|x| {
        b.identifiers
            .iter()
            .any(|y| same_authoritative_identifier(x, y))
    });
    if id_collision {
        hard_conflicts.push("AUTHORITATIVE_IDENTIFIER_COLLISION".into());
    }
    fields.push(if id_collision {
        FieldEvidence {
            field: "authoritative_identifier".into(),
            state: EvidenceState::Conflict,
            points: -80,
            rationale_code: "AUTHORITY_ID_EXPLICIT_COLLISION".into(),
        }
    } else if id_match {
        FieldEvidence {
            field: "authoritative_identifier".into(),
            state: EvidenceState::Agreement,
            points: 80,
            rationale_code: "AUTHORITY_TYPE_VALUE_EXACT".into(),
        }
    } else {
        FieldEvidence {
            field: "authoritative_identifier".into(),
            state: EvidenceState::Unknown,
            points: 0,
            rationale_code: "NO_SHARED_AUTHORITY_ID".into(),
        }
    });
    let contact_shared = a.contacts.iter().any(|x| {
        b.contacts
            .iter()
            .any(|y| x.kind == y.kind && x.synthetic_token == y.synthetic_token)
    });
    if contact_shared {
        fields.push(FieldEvidence {
            field: "contact".into(),
            state: EvidenceState::CorrelatedWeak,
            points: 0,
            rationale_code: "SHARED_CONTACT_NOT_INDEPENDENT".into(),
        });
    }
    if a.address_token.is_some() && a.address_token == b.address_token {
        fields.push(FieldEvidence {
            field: "address".into(),
            state: EvidenceState::CorrelatedWeak,
            points: 0,
            rationale_code: "SHARED_ADDRESS_NOT_INDEPENDENT".into(),
        });
    }
    // Identical demographics are also possible for relatives or twins. A
    // shortlist hit must not imply that this pair is individually identified.
    // Keep the raw score for historical comparability, but require additional
    // evidence when name and exact supplied DOB are the only corroboration.
    let demographic_ambiguity = !id_match
        && ((same_normalized_name(a, b) && dob_pair == Some(DateRelation::Exact))
            || passes
                .iter()
                .any(|p| matches!(p, Pass::NameVariant | Pass::IncompleteName)));
    if demographic_ambiguity {
        fields.push(FieldEvidence {
            field: "identity_disambiguation".into(),
            state: EvidenceState::Unknown,
            points: 0,
            rationale_code: "DEMOGRAPHICS_NOT_PERSON_SPECIFIC".into(),
        });
    }
    let score_points = fields.iter().map(|f| f.points).sum();
    let outcome = if !hard_conflicts.is_empty() {
        AdvisoryOutcome::ConflictReview
    } else if score_points == 0 || demographic_ambiguity {
        AdvisoryOutcome::InsufficientEvidence
    } else {
        AdvisoryOutcome::Review
    };
    CandidateEvidence {
        candidate_record_id: b.record_id.clone(),
        candidate_source_version: b.source_version.clone(),
        candidate_receive_version: b.receive_version,
        passes,
        score_points,
        score_type: "raw_heuristic_points_not_probability".into(),
        outcome,
        fields,
        hard_conflicts,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn banded_name_distance_matches_full_reference_for_small_strings() {
        fn reference(a: &[char], b: &[char]) -> usize {
            let mut d = vec![vec![0; b.len() + 1]; a.len() + 1];
            for (i, row) in d.iter_mut().enumerate() {
                row[0] = i;
            }
            for j in 0..=b.len() {
                d[0][j] = j;
            }
            for i in 1..=a.len() {
                for j in 1..=b.len() {
                    d[i][j] = (d[i - 1][j] + 1)
                        .min(d[i][j - 1] + 1)
                        .min(d[i - 1][j - 1] + usize::from(a[i - 1] != b[j - 1]));
                    if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                        d[i][j] = d[i][j].min(d[i - 2][j - 2] + 1);
                    }
                }
            }
            d[a.len()][b.len()]
        }
        let mut values = vec![vec![]];
        for length in 1..=4 {
            for n in 0..3_usize.pow(length) {
                let mut value = n;
                let mut chars = Vec::new();
                for _ in 0..length {
                    chars.push(['a', 'ກ', 'é'][value % 3]);
                    value /= 3;
                }
                values.push(chars);
            }
        }
        for a in &values {
            for b in &values {
                for k in 0..=2 {
                    assert_eq!(
                        bounded_name_distance(a, b, k),
                        reference(a, b) <= k,
                        "{a:?}/{b:?}/{k}"
                    );
                }
            }
        }
    }
    fn rec(id: &str, name: &str, date: Option<&str>) -> MatchRecord {
        MatchRecord {
            record_id: id.into(),
            registry_id: "r".into(),
            organization_id: "o".into(),
            source_version: Some("v1".into()),
            receive_version: Some(1),
            name_assertions: vec![MatchAssertion {
                field_path: "name.original".into(),
                original_value: name.into(),
                language_tag: None,
                script: None,
                representation: Some("source_original".into()),
                normalization_version: Some("raw-v1".into()),
            }],
            assertions: vec![],
            date_of_birth: date.map(|x| PartialDate {
                original_value: x.into(),
                precision: match x.len() {
                    4 => "year",
                    7 => "month",
                    _ => "day",
                }
                .into(),
                calendar: None,
                estimated: false,
            }),
            identifiers: vec![],
            contacts: vec![],
            address_token: None,
        }
    }
    #[test]
    fn derived_or_unknown_name_representations_do_not_create_exact_candidates() {
        for representation in ["transliteration", "source_variant", "future-unknown"] {
            let mut query = rec("a", "Mira", None);
            let mut candidate = rec("b", "Mira", None);
            query.name_assertions[0].representation = Some(representation.into());
            candidate.name_assertions[0].representation = Some(representation.into());
            query.name_assertions[0].normalization_version = Some("generator-v1".into());
            candidate.name_assertions[0].normalization_version = Some("generator-v2".into());

            let result = match_records("a", &[query, candidate], 10).unwrap();
            assert!(
                result.candidates.is_empty(),
                "{representation} equality must not create exact identity evidence"
            );
        }

        let query = rec("a", "Mira", None);
        let mut candidate = rec("b", "Mira", None);
        candidate.name_assertions[0].representation = Some("transliteration".into());
        let result = match_records("a", &[query, candidate], 10).unwrap();
        assert!(
            result.candidates.is_empty(),
            "source-original equality to a transliteration must not be an exact block"
        );
    }

    #[test]
    fn source_original_name_representations_keep_exact_name_evidence() {
        let query = rec("a", "Mira", None);
        let candidate = rec("b", "Mira", None);
        let result = match_records("a", &[query, candidate], 10).unwrap();

        assert_eq!(result.candidates.len(), 1);
        assert!(result.candidates[0].passes.contains(&Pass::ExactName));
        let name = result.candidates[0]
            .fields
            .iter()
            .find(|field| field.field == "name")
            .unwrap();
        assert_eq!(name.state, EvidenceState::Agreement);
        assert_eq!(name.rationale_code, "NAME_NORMALIZED_EXACT");
        assert_eq!(result.versions.candidate, CANDIDATE_VERSION);
        assert_eq!(result.versions.feature, FEATURE_VERSION);
    }

    #[test]
    fn only_explicit_original_name_paths_create_exact_name_evidence() {
        for field_path in [
            "name.transliteration",
            "fhir.name",
            "custom.name.value",
            "name.display",
        ] {
            let mut query = rec("query", "Same Person", Some("1986-04"));
            let mut candidate = rec("candidate", "Same Person", Some("1986-04-19"));
            query.name_assertions[0].representation = None;
            query.name_assertions[0].normalization_version = None;
            candidate.name_assertions[0].field_path = field_path.to_owned();
            candidate.name_assertions[0].representation = None;
            candidate.name_assertions[0].normalization_version = None;
            candidate.assertions.push(MatchAssertion {
                field_path: "birth_date".into(),
                original_value: "1986-04-19".into(),
                language_tag: None,
                script: None,
                representation: None,
                normalization_version: None,
            });

            let result = match_records("query", &[query, candidate], 10).unwrap();
            assert_eq!(
                result.candidates.len(),
                1,
                "DOB should retrieve {field_path}"
            );
            let evidence = &result.candidates[0];
            assert!(evidence.passes.contains(&Pass::PartialBirthYear));
            assert!(!evidence.passes.contains(&Pass::ExactName), "{field_path}");
            let name = evidence
                .fields
                .iter()
                .find(|field| field.field == "name")
                .unwrap();
            assert_eq!(name.state, EvidenceState::Unknown, "{field_path}");
            assert_eq!(name.rationale_code, "NAME_NO_EXACT_BLOCK", "{field_path}");
        }

        let mut query = rec("query", "Same Person", Some("1986-04"));
        let mut candidate = rec("candidate", "Same Person", Some("1986-04-19"));
        query.name_assertions[0].representation = None;
        query.name_assertions[0].normalization_version = None;
        candidate.name_assertions[0].representation = None;
        candidate.name_assertions[0].normalization_version = None;
        let result = match_records("query", &[query, candidate], 10).unwrap();
        assert!(result.candidates[0].passes.contains(&Pass::ExactName));
        let name = result.candidates[0]
            .fields
            .iter()
            .find(|field| field.field == "name")
            .unwrap();
        assert_eq!(name.state, EvidenceState::Agreement);
        assert_eq!(name.rationale_code, "NAME_NORMALIZED_EXACT");
    }

    #[test]
    fn partial_date_is_partial_not_fabricated_day() {
        let rs = vec![
            rec("a", "Som", Some("1986-04")),
            rec("b", "Som", Some("1986-04-19")),
        ];
        let m = match_records("a", &rs, 10).unwrap();
        assert_eq!(
            m.candidates[0].fields[1].state,
            EvidenceState::PartialAgreement
        );
        assert_eq!(m.candidates[0].fields[1].points, 10);
    }
    #[test]
    fn source_date_assertions_without_typed_metadata_use_strict_precision_parsing() {
        let mut query = rec("a", "Same Name", None);
        query.assertions.push(MatchAssertion {
            field_path: "birth_date".into(),
            original_value: "1980".into(),
            language_tag: None,
            script: None,
            representation: None,
            normalization_version: None,
        });
        let mut candidate = rec("b", "Same Name", None);
        candidate.assertions.push(MatchAssertion {
            field_path: "birth_date".into(),
            original_value: "1980-06".into(),
            language_tag: None,
            script: None,
            representation: None,
            normalization_version: None,
        });
        let result = match_records("a", &[query, candidate], 10).unwrap();
        assert!(
            result.candidates[0]
                .passes
                .contains(&Pass::PartialBirthYear)
        );
        assert_eq!(
            result.candidates[0].fields[1].state,
            EvidenceState::PartialAgreement
        );

        let mut query = rec("a", "Same Name", None);
        query.assertions.push(MatchAssertion {
            field_path: "birth_date".into(),
            original_value: "1980junk".into(),
            language_tag: None,
            script: None,
            representation: None,
            normalization_version: None,
        });
        let mut candidate = rec("b", "Same Name", None);
        candidate.assertions.push(MatchAssertion {
            field_path: "birth_date".into(),
            original_value: "1981junk".into(),
            language_tag: None,
            script: None,
            representation: None,
            normalization_version: None,
        });
        let result = match_records("a", &[query, candidate], 10).unwrap();
        assert_eq!(result.candidates[0].fields[1].state, EvidenceState::Unknown);
        assert!(result.candidates[0].hard_conflicts.is_empty());
    }
    #[test]
    fn birth_dates_with_incomparable_calendars_are_not_agreement_or_conflict() {
        let mut query = rec("a", "Same Name", Some("1980"));
        query.date_of_birth.as_mut().unwrap().calendar = Some("gregorian".into());
        let mut other = rec("b", "Same Name", Some("1981"));
        other.date_of_birth.as_mut().unwrap().calendar = Some("buddhist".into());

        let result = match_records("a", &[query, other], 10).unwrap();
        let candidate = &result.candidates[0];
        assert!(!candidate.passes.contains(&Pass::PartialBirthYear));
        assert_eq!(candidate.fields[1].state, EvidenceState::Unknown);
        assert!(
            !candidate
                .hard_conflicts
                .contains(&"DOB_YEAR_CONFLICT".into())
        );
    }

    #[test]
    fn estimated_birth_dates_do_not_count_as_exact_evidence_or_conflict() {
        let query = rec("a", "Same Name", Some("1980"));
        let mut other = rec("b", "Same Name", Some("1980"));
        other.date_of_birth.as_mut().unwrap().estimated = true;

        let result = match_records("a", &[query, other], 10).unwrap();
        assert_eq!(result.candidates[0].fields[1].state, EvidenceState::Unknown);
        assert!(
            !result.candidates[0]
                .hard_conflicts
                .contains(&"DOB_YEAR_CONFLICT".into())
        );
        assert!(
            !result.candidates[0]
                .passes
                .contains(&Pass::PartialBirthYear)
        );
    }

    #[test]
    fn malformed_or_precision_inconsistent_birth_dates_are_ignored() {
        let mut query = rec("a", "Same Name", Some("1980junk"));
        query.date_of_birth.as_mut().unwrap().precision = "year".into();
        let mut other = rec("b", "Same Name", Some("1981junk"));
        other.date_of_birth.as_mut().unwrap().precision = "year".into();

        let result = match_records("a", &[query, other], 10).unwrap();
        assert_eq!(result.candidates[0].fields[1].state, EvidenceState::Unknown);
        assert!(result.candidates[0].hard_conflicts.is_empty());
        assert!(
            !result.candidates[0]
                .passes
                .contains(&Pass::PartialBirthYear)
        );
        assert!(
            valid_date(&PartialDate {
                original_value: "1980-+1".into(),
                precision: "month".into(),
                calendar: None,
                estimated: false,
            })
            .is_none()
        );
        assert!(
            valid_date(&PartialDate {
                original_value: "0000".into(),
                precision: "year".into(),
                calendar: None,
                estimated: false,
            })
            .is_none()
        );
    }

    #[test]
    fn same_calendar_iso_birth_years_preserve_partial_agreement_and_reject_invalid_day() {
        let query = rec("a", "Same Name", Some("1980"));
        let other = rec("b", "Same Name", Some("1980-02-29"));
        let result = match_records("a", &[query, other], 10).unwrap();
        assert!(
            result.candidates[0]
                .passes
                .contains(&Pass::PartialBirthYear)
        );
        assert_eq!(
            result.candidates[0].fields[1].state,
            EvidenceState::PartialAgreement
        );

        let query = rec("a", "Same Name", Some("1980-02-29"));
        let other = rec("b", "Same Name", Some("1981-02-29"));
        let result = match_records("a", &[query, other], 10).unwrap();
        assert_eq!(result.candidates[0].fields[1].state, EvidenceState::Unknown);
        assert!(
            !result.candidates[0]
                .hard_conflicts
                .contains(&"DOB_YEAR_CONFLICT".into())
        );
    }
    #[test]
    fn scope_limit_and_determinism_are_enforced() {
        let rs = vec![
            rec("a", "Noy", None),
            rec("c", "Noy", None),
            rec("b", "Noy", None),
        ];
        let a = match_records("a", &rs, 1).unwrap();
        let b = match_records("a", &rs, 1).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.candidates[0].candidate_record_id, "b");
        assert!(a.retrieval.truncated);
        let mut bad = rs;
        bad[1].organization_id = "foreign".into();
        assert_eq!(match_records("a", &bad, 1), Err(MatchError::ScopeMismatch));
    }

    #[test]
    fn empty_query_scope_rejects_foreign_nonempty_candidate_scope() {
        let mut query = rec("a", "Noy", None);
        query.registry_id.clear();
        query.organization_id.clear();

        let mut candidate = rec("b", "Noy", None);
        candidate.registry_id = "registry-1".into();
        candidate.organization_id = "organization-1".into();

        assert_eq!(
            match_records("a", &[query, candidate], 10),
            Err(MatchError::ScopeMismatch)
        );
    }

    #[test]
    fn empty_query_scope_rejects_mixed_candidate_scopes() {
        let mut query = rec("a", "Noy", None);
        query.registry_id.clear();
        query.organization_id.clear();

        let mut candidate_a = rec("b", "Noy", None);
        candidate_a.registry_id = "registry-1".into();
        candidate_a.organization_id = "organization-1".into();

        let mut candidate_b = rec("c", "Noy", None);
        candidate_b.registry_id = "registry-2".into();
        candidate_b.organization_id = "organization-2".into();

        assert_eq!(
            match_records("a", &[query, candidate_a, candidate_b], 10),
            Err(MatchError::ScopeMismatch)
        );
    }

    #[test]
    fn empty_scope_records_can_still_be_matched_together() {
        let mut query = rec("a", "Noy", None);
        query.registry_id.clear();
        query.organization_id.clear();

        let mut candidate = rec("b", "Noy", None);
        candidate.registry_id.clear();
        candidate.organization_id.clear();

        let result = match_records("a", &[query, candidate], 10).unwrap();
        assert_eq!(result.candidates.len(), 1);
        assert_eq!(result.candidates[0].candidate_record_id, "b");
    }

    #[test]
    fn candidate_limit_keeps_authoritative_identifier_hit_ahead_of_birth_year_block() {
        let mut query = rec("query", "Query Person", Some("1980"));
        query.identifiers.push(MatchIdentifier {
            authority: "district-a".into(),
            identifier_type: "client-id".into(),
            value: "Q-123".into(),
            authoritative: true,
            status: Some("active".into()),
        });
        let mut records = vec![query];
        for i in 0..4 {
            records.push(rec(
                &format!("a-weak-{i}"),
                &format!("Different Person {i}"),
                Some("1980"),
            ));
        }
        let mut true_candidate = rec("z-true", "Other Name", Some("1980"));
        true_candidate.identifiers.push(MatchIdentifier {
            authority: "district-a".into(),
            identifier_type: "client-id".into(),
            value: "Q-123".into(),
            authoritative: true,
            status: Some("active".into()),
        });
        records.push(true_candidate);

        let result = match_records("query", &records, 1).unwrap();
        assert_eq!(result.retrieval.union_before_limit, 5);
        assert!(result.retrieval.truncated);
        assert_eq!(result.candidates[0].candidate_record_id, "z-true");
        assert_eq!(
            result.candidates[0].passes,
            vec![Pass::PartialBirthYear, Pass::AuthoritativeIdentifier]
        );
        assert_eq!(result.versions.engine, "litecr-match-v2");
        assert_eq!(result.versions.candidate, CANDIDATE_VERSION);
        assert_eq!(result.versions.feature, FEATURE_VERSION);
        assert_eq!(result.versions.policy, POLICY_VERSION);
    }
    #[test]
    fn shared_household_contact_has_no_score_and_never_auto_links() {
        let mut a = rec("a", "Twins A", Some("2001-02-03"));
        let mut b = rec("b", "Twins B", Some("2001-02-03"));
        a.contacts = vec![ContactToken {
            kind: "phone".into(),
            synthetic_token: "shared".into(),
            shared_or_recycled_possible: true,
        }];
        b.contacts = a.contacts.clone();
        let out = match_records("a", &[a, b], 2).unwrap();
        assert_eq!(
            out.candidates[0].outcome,
            AdvisoryOutcome::InsufficientEvidence
        );
        assert_eq!(
            out.candidates[0].score_type,
            "raw_heuristic_points_not_probability"
        );
        assert!(out.mutation_authority.starts_with("registry-only"));
        assert_eq!(
            out.candidates[0]
                .fields
                .iter()
                .find(|f| f.field == "contact")
                .unwrap()
                .state,
            EvidenceState::CorrelatedWeak
        );
    }
    #[test]
    fn indistinguishable_twin_demographics_require_person_specific_evidence() {
        let query = rec("query", "Same Given Same Family", Some("2001-02-03"));
        let own_observation = rec("own", "Same Given Same Family", Some("2001-02-03"));
        let twin = rec("twin", "Same Given Same Family", Some("2001-02-03"));
        let result = match_records("query", &[query, own_observation, twin], 10).unwrap();
        for candidate in result.candidates {
            assert_eq!(candidate.score_points, 65);
            assert_eq!(candidate.outcome, AdvisoryOutcome::InsufficientEvidence);
            assert!(candidate.fields.iter().any(|field| field.rationale_code
                == "DEMOGRAPHICS_NOT_PERSON_SPECIFIC"
                && field.points == 0));
            assert!(candidate.hard_conflicts.is_empty());
        }
    }

    #[test]
    fn shared_identifier_can_corroborate_one_twin_without_equating_the_other() {
        let identifier = |value: &str| MatchIdentifier {
            authority: "urn:synthetic:person-id".into(),
            identifier_type: "person-id".into(),
            value: value.into(),
            authoritative: true,
            status: Some("verified".into()),
        };
        let mut query = rec("query", "Same Given Same Family", Some("2001-02-03"));
        let mut own = rec("own", "Same Given Same Family", Some("2001-02-03"));
        let mut twin = rec("twin", "Same Given Same Family", Some("2001-02-03"));
        query.identifiers = vec![identifier("PERSON-A")];
        own.identifiers = vec![identifier("PERSON-A")];
        twin.identifiers = vec![identifier("PERSON-B")];
        let result = match_records("query", &[query, own, twin], 10).unwrap();
        let own = result
            .candidates
            .iter()
            .find(|c| c.candidate_record_id == "own")
            .unwrap();
        let twin = result
            .candidates
            .iter()
            .find(|c| c.candidate_record_id == "twin")
            .unwrap();
        assert_eq!(own.score_points, 145);
        assert_eq!(own.outcome, AdvisoryOutcome::Review);
        assert_eq!(twin.outcome, AdvisoryOutcome::InsufficientEvidence);
        assert!(
            !own.fields
                .iter()
                .any(|f| f.rationale_code == "DEMOGRAPHICS_NOT_PERSON_SPECIFIC")
        );
        assert!(
            result
                .mutation_authority
                .contains("advisory output cannot link or merge")
        );
    }
    #[test]
    fn siblings_sharing_family_name_and_household_do_not_get_full_name_agreement() {
        let mut query = rec("query", "Given One Family", Some("2001-02-03"));
        let mut sibling = rec("sibling", "Given Two Family", Some("2001-04-05"));
        query.address_token = Some("SYNTHETIC-HOUSEHOLD".into());
        sibling.address_token = query.address_token.clone();
        let result = match_records("query", &[query, sibling], 10).unwrap();
        let candidate = &result.candidates[0];
        assert_eq!(
            candidate
                .fields
                .iter()
                .find(|f| f.field == "name")
                .unwrap()
                .points,
            0
        );
        assert_eq!(
            candidate
                .fields
                .iter()
                .find(|f| f.field == "address")
                .unwrap()
                .points,
            0
        );
        assert!(!candidate.passes.contains(&Pass::ExactName));
    }
    #[test]
    fn known_birth_month_or_day_conflicts_are_not_positive_year_agreement() {
        for (left, right) in [
            ("2001-02-03", "2001-02-04"),
            ("2001-02", "2001-03-04"),
            ("2001-02", "2001-03"),
        ] {
            let query = rec("query", "Same Name", Some(left));
            let other = rec("other", "Same Name", Some(right));
            let result = match_records("query", &[query, other], 10).unwrap();
            let candidate = &result.candidates[0];
            assert!(candidate.passes.contains(&Pass::PartialBirthYear));
            assert_eq!(candidate.outcome, AdvisoryOutcome::ConflictReview);
            assert_eq!(candidate.fields[1].state, EvidenceState::Conflict);
            assert_eq!(candidate.fields[1].points, -25);
            assert!(
                candidate
                    .hard_conflicts
                    .contains(&"DOB_KNOWN_COMPONENT_CONFLICT".into())
            );
        }
        let result = match_records(
            "query",
            &[
                rec("query", "Same Name", Some("2001")),
                rec("other", "Same Name", Some("2001-02-04")),
            ],
            10,
        )
        .unwrap();
        assert_eq!(
            result.candidates[0].fields[1].state,
            EvidenceState::PartialAgreement
        );
        assert!(result.candidates[0].hard_conflicts.is_empty());
    }
    #[test]
    fn cross_script_values_are_not_transliterated_and_marks_survive() {
        let rs = vec![rec("a", "ສົມໃຈ", None), rec("b", "Somchai", None)];
        assert!(match_records("a", &rs, 10).unwrap().candidates.is_empty());
        let marked = vec![rec("a", "ກ້", None), rec("b", "ກ", None)];
        assert!(
            match_records("a", &marked, 10)
                .unwrap()
                .candidates
                .is_empty()
        );
    }
    #[test]
    fn explicit_authoritative_collision_routes_to_conflict_review() {
        let mut a = rec("a", "Name A", None);
        let mut b = rec("b", "Name B", None);
        let ident = |status: &str| MatchIdentifier {
            authority: "urn:auth".into(),
            identifier_type: "national".into(),
            value: "X-42".into(),
            authoritative: true,
            status: Some(status.into()),
        };
        a.identifiers = vec![ident("conflicting")];
        b.identifiers = vec![ident("conflicting")];
        let out = match_records("a", &[a, b], 10).unwrap();
        assert_eq!(out.candidates[0].outcome, AdvisoryOutcome::ConflictReview);
        assert!(
            out.candidates[0]
                .hard_conflicts
                .contains(&"AUTHORITATIVE_IDENTIFIER_COLLISION".into())
        );
        assert_eq!(
            out.candidates[0].score_type,
            "raw_heuristic_points_not_probability"
        );
    }
    #[test]
    fn empty_normalized_authoritative_identifiers_do_not_block_or_score() {
        for value in ["", "---", " ._ "] {
            let mut a = rec("a", "Name A", None);
            let mut b = rec("b", "Name B", None);
            let ident = || MatchIdentifier {
                authority: "urn:auth".into(),
                identifier_type: "national".into(),
                value: value.into(),
                authoritative: true,
                status: None,
            };
            a.identifiers = vec![ident()];
            b.identifiers = vec![ident()];
            let out = match_records("a", &[a, b], 10).unwrap();
            assert!(
                out.candidates
                    .iter()
                    .all(|c| !c.passes.contains(&Pass::AuthoritativeIdentifier)),
                "value {value:?} became an identifier block"
            );
            assert!(out.candidates.iter().all(|c| {
                c.fields
                    .iter()
                    .any(|f| f.field == "authoritative_identifier" && f.points == 0)
            }));
            assert_eq!(normalized_identifier(value), None);
        }
    }

    #[test]
    fn authoritative_identifiers_do_not_use_name_normalization() {
        for (left, right) in [("AB-12", "AB12"), ("00123", "123"), ("AB", "ab")] {
            let ident = |value: &str| MatchIdentifier {
                authority: "urn:auth".into(),
                identifier_type: "national".into(),
                value: value.into(),
                authoritative: true,
                status: None,
            };
            let mut query = rec("a", "Same Name", None);
            let mut candidate = rec("b", "Same Name", None);
            query.identifiers = vec![ident(left)];
            candidate.identifiers = vec![ident(right)];

            let result = match_records("a", &[query.clone(), candidate.clone()], 10).unwrap();
            let evidence = &result.candidates[0];
            assert!(!evidence.passes.contains(&Pass::AuthoritativeIdentifier));
            assert_eq!(
                evidence
                    .fields
                    .iter()
                    .find(|field| field.field == "authoritative_identifier")
                    .unwrap()
                    .state,
                EvidenceState::Unknown,
                "distinct source identifiers {left:?} and {right:?} must not agree"
            );

            query.name_assertions[0].original_value = "Query Name".into();
            candidate.name_assertions[0].original_value = "Candidate Name".into();
            assert!(
                match_records("a", &[query, candidate], 10)
                    .unwrap()
                    .candidates
                    .is_empty(),
                "distinct identifiers must not generate an authoritative-ID candidate"
            );
        }
    }
}
