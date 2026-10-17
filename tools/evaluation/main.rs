use litecr_matching::{
    AdvisoryOutcome, CandidateEvidence, ContactToken, MatchAssertion, MatchIdentifier, MatchRecord,
    PartialDate, match_records,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    io::Read,
    process::ExitCode,
    time::Instant,
};
use unicode_segmentation::UnicodeSegmentation;

#[path = "label_blind_export.rs"]
mod label_blind_export;
#[path = "paired.rs"]
mod paired;

const APPROXIMATE_CANDIDATE_VERSION: &str = "unicode-scalar-edit-distance-same-script-v1";
const EXPERIMENT_MAX_EDIT_DISTANCE: usize = 1;
const EXPERIMENT_MAX_NAME_SCALARS: usize = 256;
const EXPERIMENT_CANDIDATE_LIMIT: usize = 200;
const EXPERIMENT_MAX_COMPARISONS: usize = 100_000;

fn read_bounded(path: &str, limit: usize, kind: &str) -> Result<Vec<u8>, String> {
    let file = fs::File::open(path).map_err(|_| format!("could not read {kind}"))?;
    read_limited(file, limit, kind)
}

fn read_limited<R: Read>(reader: R, limit: usize, kind: &str) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::with_capacity(limit.min(64 * 1024));
    reader
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| format!("could not read {kind}"))?;
    if bytes.len() > limit {
        return Err(format!("{kind} exceeds size limit"));
    }
    Ok(bytes)
}

#[derive(Deserialize)]
struct Dataset {
    schema_version: String,
    dataset_id: String,
    dataset_status: String,
    purpose: String,
    provenance: serde_json::Value,
    records: Vec<Record>,
    pair_labels: Vec<Pair>,
    queries: Vec<Query>,
}
#[derive(Clone, Deserialize)]
struct Record {
    record_id: String,
    gold_cluster_id: String,
    split: String,
    name_assertions: Vec<RawName>,
    #[serde(default)]
    date_of_birth: Option<PartialDate>,
    #[serde(default)]
    identifiers: Vec<RawIdentifier>,
    #[serde(default)]
    contacts: Vec<ContactToken>,
    #[serde(default)]
    address_token: Option<String>,
}
#[derive(Clone, Deserialize)]
struct RawName {
    original_value: String,
    #[serde(default)]
    script: Option<String>,
    #[serde(default)]
    language: Option<String>,
    representation: Option<String>,
    normalization_version: Option<String>,
}
#[derive(Clone, Deserialize)]
struct RawIdentifier {
    authority: String,
    #[serde(rename = "type")]
    identifier_type: String,
    original_value: String,
    #[serde(default)]
    status: Option<String>,
}
#[derive(Clone, Deserialize, Serialize)]
struct Pair {
    pair_id: String,
    left_record_id: String,
    right_record_id: String,
    split: String,
    label: String,
    #[serde(default)]
    case_tags: Vec<String>,
}
#[derive(Deserialize, Serialize)]
struct Query {
    query_id: String,
    query_record_id: String,
    split: String,
    eligible_match_record_ids: Vec<String>,
    hard_negative_record_ids: Vec<String>,
    #[serde(default)]
    case_tags: Vec<String>,
}
#[derive(Serialize)]
struct Report {
    report_version: &'static str,
    dataset_id: String,
    dataset_status: String,
    purpose: String,
    provenance: serde_json::Value,
    split: String,
    split_digest: String,
    matcher_input_digest: String,
    label_query_manifest_digest: String,
    matcher_versions: litecr_matching::MatcherVersions,
    queries: usize,
    queries_with_positive: usize,
    positive_candidates: usize,
    positive_candidates_retrieved: usize,
    candidate_recall: Option<f64>,
    candidate_recall_aggregation: &'static str,
    hard_negatives: usize,
    hard_negatives_retrieved: usize,
    queries_truncated: usize,
    returned_candidate_count: usize,
    candidate_count_distribution: CandidateCountDistribution,
    union_duplicate_count: usize,
    candidate_pass_metrics: BTreeMap<String, PassSummary>,
    subgroup_metrics: SubgroupMetrics,
    candidate_generation_experiment: CandidateGenerationExperiment,
    #[serde(skip_serializing_if = "Option::is_none")]
    algorithm_variant_experiment: Option<AlgorithmVariantExperiment>,
    review_outcome_count: usize,
    insufficient_evidence_count: usize,
    conflict_review_count: usize,
    pair_rows: usize,
    pair_labels: BTreeMap<String, PairSummary>,
    claims: String,
    limitations: Vec<String>,
}

#[derive(Serialize)]
struct AlgorithmVariantExperiment {
    status: &'static str,
    cutoff: usize,
    script_policy: &'static str,
    grapheme_unit: &'static str,
    workload_bounds: VariantWorkloadBounds,
    levenshtein: VariantMetrics,
    damerau_levenshtein: VariantMetrics,
    grapheme_levenshtein: VariantMetrics,
    grapheme_damerau_levenshtein: VariantMetrics,
    pair_cases: BTreeMap<String, PairVariantMetrics>,
    limitations: Vec<String>,
}

#[derive(Serialize)]
struct VariantWorkloadBounds {
    max_records: usize,
    max_queries: usize,
    max_comparisons: usize,
    max_distance_cells: usize,
    candidate_limit: usize,
    measured_records: usize,
    measured_queries: usize,
    measured_comparisons: usize,
    measured_retrieval_distance_cells: usize,
    measured_labeled_pair_distance_cells: usize,
    measured_distance_cells: usize,
}

#[derive(Serialize, Default)]
struct VariantMetrics {
    queries: usize,
    positive_candidates: usize,
    positive_candidates_retrieved: usize,
    hard_negatives: usize,
    hard_negatives_retrieved: usize,
    candidates_returned: usize,
    max_candidates_per_query: usize,
    truncated_queries: usize,
    all_labeled_pair_distance_count: usize,
    all_labeled_pair_distances_within_cutoff: usize,
    uncertain_pairs_excluded: usize,
    by_script: BTreeMap<String, PairVariantMetrics>,
}

#[derive(Serialize, Default)]
struct PairVariantMetrics {
    positive_pairs: usize,
    levenshtein_positive_within_cutoff: usize,
    damerau_positive_within_cutoff: usize,
    grapheme_levenshtein_positive_within_cutoff: usize,
    grapheme_damerau_positive_within_cutoff: usize,
    hard_negative_pairs: usize,
    levenshtein_hard_negative_within_cutoff: usize,
    damerau_hard_negative_within_cutoff: usize,
    grapheme_levenshtein_hard_negative_within_cutoff: usize,
    grapheme_damerau_hard_negative_within_cutoff: usize,
    uncertain_pairs_excluded: usize,
}
#[derive(Serialize)]
struct CandidateGenerationExperiment {
    status: &'static str,
    baseline_algorithm: String,
    baseline_candidate_version: String,
    approximate_algorithm: &'static str,
    normalization_version: &'static str,
    max_edit_distance: usize,
    script_policy: &'static str,
    workload_bounds: WorkloadBounds,
    baseline: ExperimentMetrics,
    approximate_union: ExperimentMetrics,
    runtime_note: &'static str,
}
#[derive(Serialize)]
struct WorkloadBounds {
    max_records_per_split: usize,
    max_queries_per_run: usize,
    max_query_record_comparisons_per_run: usize,
    max_name_scalars_per_assertion: usize,
    max_candidates_per_query: usize,
    measured_split_records: usize,
    measured_split_queries: usize,
    measured_query_record_comparisons: usize,
}
#[derive(Serialize)]
struct ExperimentMetrics {
    queries: usize,
    positive_queries: usize,
    positive_queries_with_retrieved_match: usize,
    positive_query_recall: Option<f64>,
    positives: usize,
    positives_retrieved: usize,
    candidate_recall: Option<f64>,
    hard_negatives: usize,
    hard_negatives_retrieved: usize,
    candidate_count: usize,
    max_candidates_per_query: usize,
    truncated_queries: usize,
    elapsed_microseconds: u128,
    by_script: BTreeMap<String, ExperimentSubgroupSummary>,
    by_language: BTreeMap<String, ExperimentSubgroupSummary>,
    by_case_tag: BTreeMap<String, ExperimentSubgroupSummary>,
    by_script_case: BTreeMap<String, ExperimentSubgroupSummary>,
    by_language_case: BTreeMap<String, ExperimentSubgroupSummary>,
    missing_attribution: MissingSubgroupAttribution,
}
#[derive(Default, Serialize)]
struct PairSummary {
    pairs: usize,
    scored_pairs: usize,
    outcome_counts: BTreeMap<String, usize>,
    mean_raw_points: Option<f64>,
    raw_point_values: Vec<i16>,
    score_type: String,
}
#[derive(Default, Serialize)]
struct PassSummary {
    unique_candidates: usize,
    gold_positives_retrieved: usize,
    queries_with_candidates: usize,
}
#[derive(Serialize)]
struct CandidateCountDistribution {
    queries: usize,
    min: usize,
    mean: f64,
    median: f64,
    p95_nearest_rank: usize,
    max: usize,
}
#[derive(Serialize)]
struct SubgroupMetrics {
    /// Subgroups are assigned from the query record's source_original name
    /// assertion. Missing values are kept in an explicit "(missing)" bucket.
    by_script: BTreeMap<String, SubgroupSummary>,
    by_language: BTreeMap<String, SubgroupSummary>,
    missing_attribution: MissingSubgroupAttribution,
}
#[derive(Default, Serialize)]
struct SubgroupSummary {
    queries: usize,
    queries_with_positive: usize,
    positive_candidates: usize,
    positive_candidates_retrieved: usize,
    candidate_recall: Option<f64>,
    hard_negatives: usize,
    hard_negatives_retrieved: usize,
    hard_negative_retrieval_rate: Option<f64>,
}
#[derive(Serialize)]
struct ExperimentSubgroupSummary {
    queries: usize,
    positive_queries: usize,
    positive_queries_with_retrieved_match: usize,
    positive_query_recall: Option<f64>,
    positive_candidates: usize,
    positive_candidates_retrieved: usize,
    candidate_recall: Option<f64>,
    hard_negatives: usize,
    hard_negatives_retrieved: usize,
    hard_negative_retrieval_rate: Option<f64>,
    candidate_count: usize,
    max_candidates_per_query: usize,
    truncated_queries: usize,
}
#[derive(Default)]
struct ExperimentSubgroupCounts {
    counts: SubgroupCounts,
    candidate_count: usize,
    max_candidates_per_query: usize,
    truncated_queries: usize,
}
#[derive(Default)]
struct SubgroupCounts {
    queries: usize,
    queries_with_positive: usize,
    positive_queries_with_retrieved_match: usize,
    positive_candidates: usize,
    positive_candidates_retrieved: usize,
    hard_negatives: usize,
    hard_negatives_retrieved: usize,
}
#[derive(Serialize, Default)]
struct MissingSubgroupAttribution {
    queries_missing_script: usize,
    queries_missing_language: usize,
    positive_candidates_on_queries_missing_script: usize,
    positive_candidates_on_queries_missing_language: usize,
    hard_negatives_on_queries_missing_script: usize,
    hard_negatives_on_queries_missing_language: usize,
}
fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("evaluation failed: {e}");
            ExitCode::FAILURE
        }
    }
}
fn run() -> Result<(), String> {
    let args: Vec<_> = env::args().collect();
    if args.get(1).map(String::as_str) == Some("paired") {
        if args.len() < 3 || args.len() > 4 {
            return Err("usage: litecr-eval paired <benchmark.json> [report.json]".into());
        }
        let bytes = fs::read(&args[2]).map_err(|_| "could not read paired benchmark".to_owned())?;
        let report = paired::compare_json(&bytes)?;
        if let Some(path) = args.get(3) {
            fs::write(path, report).map_err(|_| "could not write paired report".to_owned())?;
        } else {
            println!("{report}");
        }
        return Ok(());
    }
    if args.get(1).map(String::as_str) == Some("label-blind-validate") {
        if args.len() < 4 || args.len() > 5 {
            return Err("usage: litecr-eval label-blind-validate <export.json> <opaque-query-manifest.json> [report.json]".into());
        }
        let export = read_bounded(
            &args[2],
            label_blind_export::MAX_EXPORT_BYTES,
            "label-blind export",
        )?;
        let manifest = read_bounded(
            &args[3],
            label_blind_export::MAX_QUERY_MANIFEST_BYTES,
            "opaque query manifest",
        )?;
        let query_ids: Vec<String> = serde_json::from_slice(&manifest)
            .map_err(|_| "query manifest must be a JSON array of opaque query IDs".to_owned())?;
        let report = label_blind_export::validate_json(&export, &query_ids)?;
        if let Some(path) = args.get(4) {
            fs::write(path, report).map_err(|_| "could not write label-blind report".to_owned())?;
        } else {
            println!("{report}");
        }
        return Ok(());
    }
    if args.get(1).map(String::as_str) == Some("label-blind-build") {
        if args.len() != 5 {
            return Err("usage: litecr-eval label-blind-build <body.json> <opaque-query-manifest.json> <export.json>".into());
        }
        let body = read_bounded(
            &args[2],
            label_blind_export::MAX_EXPORT_BYTES,
            "label-blind export body",
        )?;
        let manifest = read_bounded(
            &args[3],
            label_blind_export::MAX_QUERY_MANIFEST_BYTES,
            "opaque query manifest",
        )?;
        let query_ids: Vec<String> = serde_json::from_slice(&manifest)
            .map_err(|_| "query manifest must be a JSON array of opaque query IDs".to_owned())?;
        let export = label_blind_export::build_json(&body, &query_ids)?;
        fs::write(&args[4], export).map_err(|_| "could not write label-blind export".to_owned())?;
        return Ok(());
    }
    if args.len() < 3 || args.len() > 4 {
        return Err("usage: litecr-eval <dataset.json> <split> [report.json] | paired <benchmark.json> [report.json] | label-blind-build <body.json> <opaque-query-manifest.json> <export.json> | label-blind-validate <export.json> <opaque-query-manifest.json> [report.json]".into());
    }
    let bytes = fs::read(&args[1]).map_err(|e| format!("could not read dataset: {e}"))?;
    let d: Dataset =
        serde_json::from_slice(&bytes).map_err(|e| format!("invalid dataset JSON: {e}"))?;
    if d.schema_version != "1.0" {
        return Err("unsupported dataset schema_version".into());
    }
    validate_dataset_metadata(&d)?;
    let split = &args[2];
    validate_dataset_relations(&d.records, &d.pair_labels, &d.queries, split)?;
    let rows: Vec<_> = d.records.iter().filter(|r| &r.split == split).collect();
    if rows.is_empty() {
        return Err("requested split has no records".into());
    }
    let matcher_input: Vec<MatchRecord> = rows.iter().map(|x| to_match_record(x)).collect();
    let mut record_split_digest = rows.iter().map(|x| x.record_id.clone()).collect::<Vec<_>>();
    record_split_digest.sort();
    let mut split_hash = Sha256::new();
    for id in &record_split_digest {
        split_hash.update((id.len() as u64).to_be_bytes());
        split_hash.update(id.as_bytes());
    }
    let split_digest = format!("sha256:{}", hex(&split_hash.finalize()));
    let matcher_input_digest = digest_matcher_inputs(&rows)?;
    let label_query_manifest_digest = digest_label_query_manifest(&d, split)?;
    let by_query: BTreeMap<_, _> = d
        .queries
        .iter()
        .filter(|q| q.split == *split)
        .map(|q| (q.query_record_id.as_str(), q))
        .collect();
    validate_experiment_bounds(rows.len(), by_query.len(), &rows)?;
    let mut positives = 0usize;
    let mut positives_found = 0usize;
    let mut negatives = 0usize;
    let mut negatives_found = 0usize;
    let mut truncated = 0;
    let mut returned = 0;
    let mut candidate_counts = Vec::with_capacity(by_query.len());
    let mut union_duplicates = 0;
    let mut pass_metrics: BTreeMap<String, PassSummary> = BTreeMap::new();
    let mut by_script: BTreeMap<String, SubgroupCounts> = BTreeMap::new();
    let mut by_language: BTreeMap<String, SubgroupCounts> = BTreeMap::new();
    let mut missing_attribution = MissingSubgroupAttribution {
        queries_missing_script: 0,
        queries_missing_language: 0,
        positive_candidates_on_queries_missing_script: 0,
        positive_candidates_on_queries_missing_language: 0,
        hard_negatives_on_queries_missing_script: 0,
        hard_negatives_on_queries_missing_language: 0,
    };
    let mut review = 0;
    let mut insufficient = 0;
    let mut conflict = 0;
    let mut version = None;
    for q in by_query.values() {
        let result = match_records(&q.query_record_id, &matcher_input, 200)
            .map_err(|e| format!("matcher failed for {}: {e:?}", q.query_id))?;
        version = Some(result.versions);
        let ids: BTreeSet<_> = result
            .candidates
            .iter()
            .map(|c| c.candidate_record_id.as_str())
            .collect();
        positives += q.eligible_match_record_ids.len();
        let query_positives_found = q
            .eligible_match_record_ids
            .iter()
            .filter(|id| ids.contains(id.as_str()))
            .count();
        positives_found += query_positives_found;
        negatives += q.hard_negative_record_ids.len();
        let query_negatives_found = q
            .hard_negative_record_ids
            .iter()
            .filter(|id| ids.contains(id.as_str()))
            .count();
        negatives_found += query_negatives_found;
        let query_record = rows
            .iter()
            .find(|record| record.record_id == q.query_record_id)
            .ok_or_else(|| format!("query {} record is absent from split", q.query_id))?;
        let (script, language) = query_subgroups(query_record);
        record_subgroup(
            &mut by_script,
            subgroup_key(script),
            q.eligible_match_record_ids.len(),
            query_positives_found,
            q.hard_negative_record_ids.len(),
            query_negatives_found,
        );
        record_subgroup(
            &mut by_language,
            subgroup_key(language),
            q.eligible_match_record_ids.len(),
            query_positives_found,
            q.hard_negative_record_ids.len(),
            query_negatives_found,
        );
        if script.is_none() {
            missing_attribution.queries_missing_script += 1;
            missing_attribution.positive_candidates_on_queries_missing_script +=
                q.eligible_match_record_ids.len();
            missing_attribution.hard_negatives_on_queries_missing_script +=
                q.hard_negative_record_ids.len();
        }
        if language.is_none() {
            missing_attribution.queries_missing_language += 1;
            missing_attribution.positive_candidates_on_queries_missing_language +=
                q.eligible_match_record_ids.len();
            missing_attribution.hard_negatives_on_queries_missing_language +=
                q.hard_negative_record_ids.len();
        }
        truncated += usize::from(result.retrieval.truncated);
        returned += result.candidates.len();
        candidate_counts.push(result.candidates.len());
        let gold: BTreeSet<_> = q
            .eligible_match_record_ids
            .iter()
            .map(String::as_str)
            .collect();
        for c in &result.candidates {
            union_duplicates += c.passes.len().saturating_sub(1);
            for pass in &c.passes {
                let stats = pass_metrics.entry(pass_name(*pass).into()).or_default();
                stats.unique_candidates += 1;
                stats.gold_positives_retrieved +=
                    usize::from(gold.contains(c.candidate_record_id.as_str()));
            }
        }
        for stats in pass_metrics.values_mut() {
            /* query contribution is deduplicated below from current candidates */
            let _ = stats;
        }
        let current_passes: BTreeSet<_> = result
            .candidates
            .iter()
            .flat_map(|c| c.passes.iter().map(|p| pass_name(*p)))
            .collect();
        for pass in current_passes {
            pass_metrics
                .entry(pass.into())
                .or_default()
                .queries_with_candidates += 1;
        }
        for c in result.candidates {
            match c.outcome {
                AdvisoryOutcome::Review => review += 1,
                AdvisoryOutcome::InsufficientEvidence => insufficient += 1,
                AdvisoryOutcome::ConflictReview => conflict += 1,
            }
        }
    }
    let mut pair_buckets: BTreeMap<String, PairSummary> = BTreeMap::new();
    for p in d.pair_labels.iter().filter(|p| p.split == *split) {
        let pair_input = rows
            .iter()
            .filter(|r| r.record_id == p.left_record_id || r.record_id == p.right_record_id)
            .map(|r| to_match_record(r))
            .collect::<Vec<_>>();
        if pair_input.len() != 2 {
            continue;
        }
        let m = match_records(&p.left_record_id, &pair_input, 200)
            .map_err(|e| format!("pair {} evaluation failed: {e:?}", p.pair_id))?;
        // Only score the requested pair. A candidate omitted by retrieval is not a
        // zero-valued score and must remain visible as `not_retrieved`.
        let candidate = m
            .candidates
            .iter()
            .find(|candidate| candidate.candidate_record_id == p.right_record_id);
        record_pair_observation(&mut pair_buckets, &p.label, candidate);
    }
    let pair_labels = finish_pair_summaries(pair_buckets);
    let candidate_generation_experiment = run_candidate_experiment(&rows, &by_query)?;
    let algorithm_variant_experiment =
        run_algorithm_variant_experiment(&d, &rows, &by_query, split)?;
    let report = Report {
        report_version: "litecr-evaluation-v6",
        dataset_id: d.dataset_id,
        dataset_status: d.dataset_status,
        purpose: d.purpose,
        provenance: d.provenance,
        split: split.clone(),
        split_digest,
        matcher_input_digest,
        label_query_manifest_digest,
        matcher_versions: version.ok_or("no matching queries in split")?,
        queries: by_query.len(),
        queries_with_positive: d
            .queries
            .iter()
            .filter(|q| q.split == *split && !q.eligible_match_record_ids.is_empty())
            .count(),
        positive_candidates: positives,
        positive_candidates_retrieved: positives_found,
        candidate_recall: (positives > 0).then_some(positives_found as f64 / positives as f64),
        candidate_recall_aggregation: "micro over eligible gold-match pairs (retrieved positives / eligible positives)",
        hard_negatives: negatives,
        hard_negatives_retrieved: negatives_found,
        queries_truncated: truncated,
        returned_candidate_count: returned,
        candidate_count_distribution: candidate_count_distribution(candidate_counts)
            .ok_or("no matching queries in split")?,
        union_duplicate_count: union_duplicates,
        candidate_pass_metrics: pass_metrics,
        subgroup_metrics: SubgroupMetrics {
            by_script: finish_subgroups(by_script),
            by_language: finish_subgroups(by_language),
            missing_attribution,
        },
        candidate_generation_experiment,
        algorithm_variant_experiment,
        review_outcome_count: review,
        insufficient_evidence_count: insufficient,
        conflict_review_count: conflict,
        pair_rows: pair_buckets_count(&d.pair_labels, split),
        pair_labels,
        claims: "synthetic mechanics only; no real-world accuracy, representativeness, calibration, OpenCR comparison, or production threshold claim".into(),
        limitations: vec![
            "No authorized representative adjudicated dataset is supplied.".into(),
            "The held-out split is the repository synthetic fixture and is not an independent accuracy study.".into(),
            "Candidate recall denominator is exhaustive only within the fixture's eligible query universe.".into(),
            "Subgroups use only the query record's source_original name assertion; missing metadata is reported, not inferred.".into(),
            "No heuristic score is a probability; no threshold was selected on this report.".into(),
        ],
    };
    let out = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
    if let Some(path) = args.get(3) {
        fs::write(path, &out).map_err(|e| format!("could not write report: {e}"))?
    } else {
        println!("{out}")
    }
    Ok(())
}

fn validate_dataset_metadata(dataset: &Dataset) -> Result<(), String> {
    if dataset.dataset_status != "synthetic_fixture" {
        return Err(
            "litecr-eval currently accepts synthetic mechanics fixtures only; controlled-cohort evaluation is not available".into(),
        );
    }
    if dataset.purpose != "mechanics_only" {
        return Err(
            "synthetic evaluator input must use purpose=mechanics_only; accuracy evaluation is not available".into(),
        );
    }
    if dataset
        .provenance
        .get("contains_real_person_data")
        .and_then(|value| value.as_bool())
        != Some(false)
    {
        return Err(
            "synthetic dataset provenance must assert contains_real_person_data=false".into(),
        );
    }
    Ok(())
}

#[derive(Serialize)]
struct LabelQueryManifest<'a> {
    dataset_id: &'a str,
    dataset_status: &'a str,
    purpose: &'a str,
    provenance: &'a serde_json::Value,
    split: &'a str,
    pair_labels: Vec<&'a Pair>,
    queries: Vec<&'a Query>,
}

fn digest_matcher_inputs(records: &[&Record]) -> Result<String, String> {
    let mut inputs: Vec<_> = records
        .iter()
        .map(|record| to_match_record(record))
        .collect();
    inputs.sort_by(|left, right| left.record_id.cmp(&right.record_id));
    digest_serializable(&inputs)
}

fn digest_label_query_manifest(dataset: &Dataset, split: &str) -> Result<String, String> {
    let mut pair_labels: Vec<_> = dataset
        .pair_labels
        .iter()
        .filter(|pair| pair.split == split)
        .collect();
    pair_labels.sort_by(|left, right| left.pair_id.cmp(&right.pair_id));
    let mut queries: Vec<_> = dataset
        .queries
        .iter()
        .filter(|query| query.split == split)
        .collect();
    queries.sort_by(|left, right| left.query_id.cmp(&right.query_id));

    digest_serializable(&LabelQueryManifest {
        dataset_id: &dataset.dataset_id,
        dataset_status: &dataset.dataset_status,
        purpose: &dataset.purpose,
        provenance: &dataset.provenance,
        split,
        pair_labels,
        queries,
    })
}

fn digest_serializable(value: &impl Serialize) -> Result<String, String> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| format!("could not encode digest input: {error}"))?;
    let digest = Sha256::digest(bytes);
    Ok(format!("sha256:{}", hex(&digest)))
}

/// Validate every relationship consumed by the evaluator before calculating
/// metrics. In particular, never silently drop a labeled pair because one of
/// its records belongs to another split or is absent from the dataset.
fn validate_dataset_relations(
    records: &[Record],
    pairs: &[Pair],
    queries: &[Query],
    requested_split: &str,
) -> Result<(), String> {
    if !matches!(requested_split, "train" | "development" | "held_out_test") {
        return Err(format!("unsupported split: {requested_split}"));
    }

    let mut record_by_id = BTreeMap::new();
    let mut cluster_splits: BTreeMap<&str, &str> = BTreeMap::new();
    for record in records {
        if !matches!(
            record.split.as_str(),
            "train" | "development" | "held_out_test"
        ) {
            return Err(format!(
                "record {} has an unsupported split",
                record.record_id
            ));
        }
        if record_by_id
            .insert(record.record_id.as_str(), record)
            .is_some()
        {
            return Err(format!("duplicate record_id: {}", record.record_id));
        }
        if let Some(prior_split) = cluster_splits.get(record.gold_cluster_id.as_str()) {
            if *prior_split != record.split.as_str() {
                return Err("a gold identity cluster appears in multiple splits".into());
            }
        } else {
            cluster_splits.insert(record.gold_cluster_id.as_str(), record.split.as_str());
        }
    }
    if !records.iter().any(|record| record.split == requested_split) {
        return Err(format!("requested split has no records: {requested_split}"));
    }

    let mut pair_ids = BTreeSet::new();
    for pair in pairs {
        if !pair_ids.insert(pair.pair_id.as_str()) {
            return Err(format!("duplicate pair_id: {}", pair.pair_id));
        }
        if !matches!(pair.label.as_str(), "match" | "non_match" | "uncertain") {
            return Err(format!("pair {} has an unsupported label", pair.pair_id));
        }
        if pair.left_record_id == pair.right_record_id {
            return Err(format!(
                "pair {} compares a record with itself",
                pair.pair_id
            ));
        }
        let left = record_by_id
            .get(pair.left_record_id.as_str())
            .ok_or_else(|| {
                format!(
                    "pair {} references missing record {}",
                    pair.pair_id, pair.left_record_id
                )
            })?;
        let right = record_by_id
            .get(pair.right_record_id.as_str())
            .ok_or_else(|| {
                format!(
                    "pair {} references missing record {}",
                    pair.pair_id, pair.right_record_id
                )
            })?;
        if left.split != right.split || pair.split != left.split {
            return Err(format!("pair {} crosses split boundaries", pair.pair_id));
        }
        match pair.label.as_str() {
            "match" if left.gold_cluster_id != right.gold_cluster_id => {
                return Err(format!(
                    "pair {} match label conflicts with gold clusters",
                    pair.pair_id
                ));
            }
            "non_match" if left.gold_cluster_id == right.gold_cluster_id => {
                return Err(format!(
                    "pair {} non_match label conflicts with gold clusters",
                    pair.pair_id
                ));
            }
            _ => {}
        }
    }

    let mut query_ids = BTreeSet::new();
    let mut query_records = BTreeSet::new();
    for query in queries {
        if !query_ids.insert(query.query_id.as_str()) {
            return Err(format!("duplicate query_id: {}", query.query_id));
        }
        if !query_records.insert((query.split.as_str(), query.query_record_id.as_str())) {
            return Err(format!("duplicate query anchor: {}", query.query_record_id));
        }
        let anchor = record_by_id
            .get(query.query_record_id.as_str())
            .ok_or_else(|| {
                format!(
                    "query {} references missing anchor {}",
                    query.query_id, query.query_record_id
                )
            })?;
        if !matches!(
            query.split.as_str(),
            "train" | "development" | "held_out_test"
        ) || query.split != anchor.split
        {
            return Err(format!(
                "query {} split does not match its anchor",
                query.query_id
            ));
        }

        let mut positive_ids = BTreeSet::new();
        for candidate_id in &query.eligible_match_record_ids {
            if !positive_ids.insert(candidate_id.as_str()) {
                return Err(format!(
                    "query {} repeats a positive candidate",
                    query.query_id
                ));
            }
            let candidate = record_by_id.get(candidate_id.as_str()).ok_or_else(|| {
                format!(
                    "query {} references missing positive candidate {candidate_id}",
                    query.query_id
                )
            })?;
            if candidate.split != anchor.split || candidate.record_id == anchor.record_id {
                return Err(format!(
                    "query {} has an out-of-split or self positive",
                    query.query_id
                ));
            }
            if candidate.gold_cluster_id != anchor.gold_cluster_id {
                return Err(format!(
                    "query {} positive conflicts with gold clusters",
                    query.query_id
                ));
            }
        }
        let mut negative_ids = BTreeSet::new();
        for candidate_id in &query.hard_negative_record_ids {
            if !negative_ids.insert(candidate_id.as_str()) {
                return Err(format!("query {} repeats a hard negative", query.query_id));
            }
            let candidate = record_by_id.get(candidate_id.as_str()).ok_or_else(|| {
                format!(
                    "query {} references missing hard negative {candidate_id}",
                    query.query_id
                )
            })?;
            if candidate.split != anchor.split || candidate.record_id == anchor.record_id {
                return Err(format!(
                    "query {} has an out-of-split or self negative",
                    query.query_id
                ));
            }
            if candidate.gold_cluster_id == anchor.gold_cluster_id {
                return Err(format!(
                    "query {} hard negative conflicts with gold clusters",
                    query.query_id
                ));
            }
            if positive_ids.contains(candidate_id.as_str()) {
                return Err(format!(
                    "query {} marks a candidate both positive and negative",
                    query.query_id
                ));
            }
        }
        let expected_positive_ids: BTreeSet<_> = records
            .iter()
            .filter(|record| {
                record.split == anchor.split
                    && record.record_id != anchor.record_id
                    && record.gold_cluster_id == anchor.gold_cluster_id
            })
            .map(|record| record.record_id.as_str())
            .collect();
        if positive_ids != expected_positive_ids {
            return Err(format!(
                "query {} positive truth is not exhaustive for its split",
                query.query_id
            ));
        }
    }

    Ok(())
}

fn query_subgroups(record: &Record) -> (Option<&str>, Option<&str>) {
    let source_original = record
        .name_assertions
        .iter()
        .find(|name| name.representation.as_deref() == Some("source_original"));
    source_original.map_or((None, None), |name| {
        (
            name.script
                .as_deref()
                .filter(|value| !value.trim().is_empty()),
            name.language
                .as_deref()
                .filter(|value| !value.trim().is_empty()),
        )
    })
}
fn subgroup_key(value: Option<&str>) -> String {
    value.unwrap_or("(missing)").to_owned()
}
fn candidate_count_distribution(mut counts: Vec<usize>) -> Option<CandidateCountDistribution> {
    if counts.is_empty() {
        return None;
    }
    counts.sort_unstable();
    let queries = counts.len();
    let total_candidates: usize = counts.iter().sum();
    let midpoint = queries / 2;
    let median = if queries % 2 == 0 {
        (counts[midpoint - 1] as f64 + counts[midpoint] as f64) / 2.0
    } else {
        counts[midpoint] as f64
    };
    // Nearest-rank percentile: rank = ceil(0.95 * n), using one-based ranks.
    let p95_index = (95 * queries).div_ceil(100) - 1;
    Some(CandidateCountDistribution {
        queries,
        min: counts[0],
        mean: total_candidates as f64 / queries as f64,
        median,
        p95_nearest_rank: counts[p95_index],
        max: counts[queries - 1],
    })
}
fn record_subgroup(
    groups: &mut BTreeMap<String, SubgroupCounts>,
    key: String,
    positives: usize,
    positives_retrieved: usize,
    hard_negatives: usize,
    hard_negatives_retrieved: usize,
) {
    let group = groups.entry(key).or_default();
    group.queries += 1;
    group.queries_with_positive += usize::from(positives > 0);
    group.positive_candidates += positives;
    group.positive_candidates_retrieved += positives_retrieved;
    group.hard_negatives += hard_negatives;
    group.hard_negatives_retrieved += hard_negatives_retrieved;
}
fn finish_subgroups(groups: BTreeMap<String, SubgroupCounts>) -> BTreeMap<String, SubgroupSummary> {
    groups
        .into_iter()
        .map(|(key, counts)| {
            let candidate_recall = (counts.positive_candidates > 0).then_some(
                counts.positive_candidates_retrieved as f64 / counts.positive_candidates as f64,
            );
            let hard_negative_retrieval_rate = (counts.hard_negatives > 0)
                .then_some(counts.hard_negatives_retrieved as f64 / counts.hard_negatives as f64);
            (
                key,
                SubgroupSummary {
                    queries: counts.queries,
                    queries_with_positive: counts.queries_with_positive,
                    positive_candidates: counts.positive_candidates,
                    positive_candidates_retrieved: counts.positive_candidates_retrieved,
                    candidate_recall,
                    hard_negatives: counts.hard_negatives,
                    hard_negatives_retrieved: counts.hard_negatives_retrieved,
                    hard_negative_retrieval_rate,
                },
            )
        })
        .collect()
}

#[derive(Clone, Copy)]
struct ExperimentSubgroupObservation {
    positives: usize,
    positives_retrieved: usize,
    hard_negatives: usize,
    hard_negatives_retrieved: usize,
    candidates: usize,
    truncated: bool,
}

fn record_experiment_subgroup(
    groups: &mut BTreeMap<String, ExperimentSubgroupCounts>,
    key: String,
    observation: ExperimentSubgroupObservation,
) {
    let group = groups.entry(key).or_default();
    group.counts.queries += 1;
    group.counts.queries_with_positive += usize::from(observation.positives > 0);
    group.counts.positive_queries_with_retrieved_match +=
        usize::from(observation.positives > 0 && observation.positives_retrieved > 0);
    group.counts.positive_candidates += observation.positives;
    group.counts.positive_candidates_retrieved += observation.positives_retrieved;
    group.counts.hard_negatives += observation.hard_negatives;
    group.counts.hard_negatives_retrieved += observation.hard_negatives_retrieved;
    group.candidate_count += observation.candidates;
    group.max_candidates_per_query = group.max_candidates_per_query.max(observation.candidates);
    group.truncated_queries += usize::from(observation.truncated);
}

fn finish_experiment_subgroups(
    groups: BTreeMap<String, ExperimentSubgroupCounts>,
) -> BTreeMap<String, ExperimentSubgroupSummary> {
    groups
        .into_iter()
        .map(|(key, counts)| {
            let candidate_recall = (counts.counts.positive_candidates > 0).then_some(
                counts.counts.positive_candidates_retrieved as f64
                    / counts.counts.positive_candidates as f64,
            );
            let positive_query_recall = (counts.counts.queries_with_positive > 0).then_some(
                counts.counts.positive_queries_with_retrieved_match as f64
                    / counts.counts.queries_with_positive as f64,
            );
            let hard_negative_retrieval_rate = (counts.counts.hard_negatives > 0).then_some(
                counts.counts.hard_negatives_retrieved as f64 / counts.counts.hard_negatives as f64,
            );
            (
                key,
                ExperimentSubgroupSummary {
                    queries: counts.counts.queries,
                    positive_queries: counts.counts.queries_with_positive,
                    positive_queries_with_retrieved_match: counts
                        .counts
                        .positive_queries_with_retrieved_match,
                    positive_query_recall,
                    positive_candidates: counts.counts.positive_candidates,
                    positive_candidates_retrieved: counts.counts.positive_candidates_retrieved,
                    candidate_recall,
                    hard_negatives: counts.counts.hard_negatives,
                    hard_negatives_retrieved: counts.counts.hard_negatives_retrieved,
                    hard_negative_retrieval_rate,
                    candidate_count: counts.candidate_count,
                    max_candidates_per_query: counts.max_candidates_per_query,
                    truncated_queries: counts.truncated_queries,
                },
            )
        })
        .collect()
}

fn validate_experiment_bounds(
    record_count: usize,
    query_count: usize,
    rows: &[&Record],
) -> Result<(), String> {
    const MAX_RECORDS: usize = 10_000;
    const MAX_QUERIES: usize = 1_000;
    if record_count > MAX_RECORDS || query_count > MAX_QUERIES {
        return Err(format!(
            "experiment workload exceeds bounds (records <= {MAX_RECORDS}, queries <= {MAX_QUERIES})"
        ));
    }
    let comparisons = record_count.saturating_mul(query_count);
    if comparisons > EXPERIMENT_MAX_COMPARISONS {
        return Err(format!(
            "experiment workload exceeds {EXPERIMENT_MAX_COMPARISONS} query-record comparisons"
        ));
    }
    for record in rows {
        for name in &record.name_assertions {
            if name.original_value.chars().count() > EXPERIMENT_MAX_NAME_SCALARS {
                return Err(format!(
                    "experiment name exceeds {} Unicode scalars on record {}",
                    EXPERIMENT_MAX_NAME_SCALARS, record.record_id
                ));
            }
        }
    }
    Ok(())
}

fn run_candidate_experiment(
    rows: &[&Record],
    queries: &BTreeMap<&str, &Query>,
) -> Result<CandidateGenerationExperiment, String> {
    let baseline_started = Instant::now();
    let mut baseline = ExperimentCounts::default();
    let match_records: Vec<_> = rows.iter().map(|record| to_match_record(record)).collect();
    for query in queries.values() {
        let result = match_records_for_query(
            &query.query_record_id,
            &match_records,
            EXPERIMENT_CANDIDATE_LIMIT,
        )?;
        let ids: BTreeSet<_> = result
            .candidates
            .iter()
            .map(|candidate| candidate.candidate_record_id.as_str())
            .collect();
        baseline.record(query, rows, &ids, result.retrieval.truncated);
    }
    let baseline_elapsed = baseline_started.elapsed().as_micros();

    let approximate_started = Instant::now();
    let mut approximate = ExperimentCounts::default();
    let match_records: Vec<_> = rows.iter().map(|record| to_match_record(record)).collect();
    for query in queries.values() {
        let baseline_result = match_records_for_query(
            &query.query_record_id,
            &match_records,
            EXPERIMENT_CANDIDATE_LIMIT,
        )?;
        let baseline_ids: BTreeSet<_> = baseline_result
            .candidates
            .iter()
            .map(|candidate| candidate.candidate_record_id.clone())
            .collect();
        let added = approximate_name_candidates(query, rows)?;
        let extras: Vec<_> = added
            .iter()
            .filter(|(candidate_id, _)| !baseline_ids.contains(candidate_id))
            .collect();
        let remaining = EXPERIMENT_CANDIDATE_LIMIT.saturating_sub(baseline_ids.len());
        let mut ids = baseline_ids;
        for (candidate_id, _) in extras.iter().take(remaining) {
            ids.insert((*candidate_id).clone());
        }
        let truncated = baseline_result.retrieval.truncated || extras.len() > remaining;
        let ids_ref: BTreeSet<_> = ids.iter().map(String::as_str).collect();
        approximate.record(query, rows, &ids_ref, truncated);
    }
    let approximate_elapsed = approximate_started.elapsed().as_micros();

    Ok(CandidateGenerationExperiment {
        status: "experiment_only_not_production_enabled",
        baseline_algorithm: "current production candidate generator plus current advisory matcher"
            .into(),
        baseline_candidate_version: litecr_matching::CANDIDATE_VERSION.into(),
        approximate_algorithm: APPROXIMATE_CANDIDATE_VERSION,
        normalization_version: litecr_matching::NORMALIZATION_VERSION,
        max_edit_distance: EXPERIMENT_MAX_EDIT_DISTANCE,
        script_policy: "compare source_original names only when both script tags are present and exactly equal; no transliteration, script conversion, or language inference",
        workload_bounds: WorkloadBounds {
            max_records_per_split: 10_000,
            max_queries_per_run: 1_000,
            max_query_record_comparisons_per_run: EXPERIMENT_MAX_COMPARISONS,
            max_name_scalars_per_assertion: EXPERIMENT_MAX_NAME_SCALARS,
            max_candidates_per_query: EXPERIMENT_CANDIDATE_LIMIT,
            measured_split_records: rows.len(),
            measured_split_queries: queries.len(),
            measured_query_record_comparisons: rows.len().saturating_mul(queries.len()),
        },
        baseline: baseline.finish(queries.len(), baseline_elapsed),
        approximate_union: approximate.finish(queries.len(), approximate_elapsed),
        runtime_note: "single local wall-clock sample includes repeated baseline retrieval; indicative only, not a benchmark or production latency claim",
    })
}

fn run_algorithm_variant_experiment(
    dataset: &Dataset,
    rows: &[&Record],
    queries: &BTreeMap<&str, &Query>,
    split: &str,
) -> Result<Option<AlgorithmVariantExperiment>, String> {
    let variant_pairs: Vec<_> = dataset
        .pair_labels
        .iter()
        .filter(|pair| {
            pair.split == split && pair.case_tags.iter().any(|tag| tag.starts_with("variant-"))
        })
        .collect();
    if variant_pairs.is_empty() {
        return Ok(None);
    }

    const MAX_VARIANT_COMPARISONS: usize = 100_000;
    const MAX_VARIANT_DISTANCE_CELLS: usize = 50_000_000;
    const VARIANT_CANDIDATE_LIMIT: usize = 200;
    const MAX_VARIANT_SCALARS: usize = 256;
    let comparisons = rows
        .len()
        .checked_mul(queries.len())
        .ok_or_else(|| "algorithm variant comparison count overflow".to_owned())?;
    if rows.len() > 10_000 || queries.len() > 1_000 || comparisons > MAX_VARIANT_COMPARISONS {
        return Err("algorithm variant experiment exceeds its workload bounds".into());
    }
    if rows
        .iter()
        .flat_map(|record| &record.name_assertions)
        .any(|name| name.original_value.chars().count() > MAX_VARIANT_SCALARS)
    {
        return Err(format!(
            "algorithm variant name exceeds {MAX_VARIANT_SCALARS} Unicode scalars"
        ));
    }
    let records_by_id: BTreeMap<_, _> = rows
        .iter()
        .map(|record| (record.record_id.as_str(), *record))
        .collect();
    let retrieval_distance_cells = checked_four_comparator_cells(
        variant_distance_cell_work(rows, queries)?,
        "query retrieval",
    )?;
    let labeled_pair_distance_cells = checked_four_comparator_cells(
        variant_pair_distance_cell_work(&variant_pairs, &records_by_id)?,
        "labeled pairs",
    )?;
    // Bound query retrieval and labeled-pair analysis together. Each base
    // cell is multiplied by four for scalar/grapheme Levenshtein and
    // Damerau–Levenshtein; uncertain pairs are included in pair work too.
    let distance_cells = retrieval_distance_cells
        .checked_add(labeled_pair_distance_cells)
        .ok_or_else(|| "algorithm variant aggregate distance-cell count overflow".to_owned())?;
    if distance_cells > MAX_VARIANT_DISTANCE_CELLS {
        return Err(format!(
            "algorithm variant experiment exceeds {MAX_VARIANT_DISTANCE_CELLS} aggregate distance cells"
        ));
    }

    let mut levenshtein = VariantMetrics::default();
    let mut damerau = VariantMetrics::default();
    let mut grapheme_levenshtein = VariantMetrics::default();
    let mut grapheme_damerau = VariantMetrics::default();
    let uncertain_candidates_by_query = uncertain_candidates_by_query(&variant_pairs, queries);
    for query in queries.values() {
        let query_record = rows
            .iter()
            .find(|record| record.record_id == query.query_record_id)
            .ok_or_else(|| format!("variant query record {} is missing", query.query_record_id))?;
        let query_name = original_name_and_script(query_record);
        let lev_result = variant_candidates(
            &query.query_record_id,
            query_name,
            rows,
            VariantDistance::Levenshtein,
            VARIANT_CANDIDATE_LIMIT,
        );
        let dam_result = variant_candidates(
            &query.query_record_id,
            query_name,
            rows,
            VariantDistance::DamerauLevenshtein,
            VARIANT_CANDIDATE_LIMIT,
        );
        let grapheme_lev_result = variant_candidates(
            &query.query_record_id,
            query_name,
            rows,
            VariantDistance::GraphemeLevenshtein,
            VARIANT_CANDIDATE_LIMIT,
        );
        let grapheme_dam_result = variant_candidates(
            &query.query_record_id,
            query_name,
            rows,
            VariantDistance::GraphemeDamerauLevenshtein,
            VARIANT_CANDIDATE_LIMIT,
        );
        let excluded = uncertain_candidates_by_query.get(query.query_record_id.as_str());
        record_variant_retrieval(
            &mut levenshtein,
            query,
            &lev_result.0,
            lev_result.1,
            excluded,
        );
        record_variant_retrieval(&mut damerau, query, &dam_result.0, dam_result.1, excluded);
        record_variant_retrieval(
            &mut grapheme_levenshtein,
            query,
            &grapheme_lev_result.0,
            grapheme_lev_result.1,
            excluded,
        );
        record_variant_retrieval(
            &mut grapheme_damerau,
            query,
            &grapheme_dam_result.0,
            grapheme_dam_result.1,
            excluded,
        );
    }

    let mut pair_cases = BTreeMap::<String, PairVariantMetrics>::new();
    for pair in variant_pairs {
        let tag = pair
            .case_tags
            .iter()
            .find(|tag| tag.starts_with("variant-"))
            .expect("filtered above");
        let left = records_by_id
            .get(pair.left_record_id.as_str())
            .ok_or_else(|| format!("variant pair {} left record missing", pair.pair_id))?;
        let right = records_by_id
            .get(pair.right_record_id.as_str())
            .ok_or_else(|| format!("variant pair {} right record missing", pair.pair_id))?;
        let left_assertion = original_name_and_script(left);
        let right_assertion = original_name_and_script(right);
        let distances = matching_script_name_pair(left_assertion, right_assertion).map(|(a, b)| {
            let a = litecr_matching::normalized_assertion_key(a);
            let b = litecr_matching::normalized_assertion_key(b);
            (
                levenshtein_distance(&a, &b),
                damerau_levenshtein_distance(&a, &b),
                grapheme_levenshtein_distance(&a, &b),
                grapheme_damerau_levenshtein_distance(&a, &b),
            )
        });
        let Some((lev_distance, dam_distance, grapheme_lev_distance, grapheme_dam_distance)) =
            distances
        else {
            return Err(format!(
                "variant pair {} lacks explicit same-script source_original names",
                pair.pair_id
            ));
        };
        let counts = pair_cases.entry(tag.clone()).or_default();
        match pair.label.as_str() {
            "match" => {
                counts.positive_pairs += 1;
                counts.levenshtein_positive_within_cutoff += usize::from(lev_distance <= 1);
                counts.damerau_positive_within_cutoff += usize::from(dam_distance <= 1);
                counts.grapheme_levenshtein_positive_within_cutoff +=
                    usize::from(grapheme_lev_distance <= 1);
                counts.grapheme_damerau_positive_within_cutoff +=
                    usize::from(grapheme_dam_distance <= 1);
            }
            "non_match" => {
                counts.hard_negative_pairs += 1;
                counts.levenshtein_hard_negative_within_cutoff += usize::from(lev_distance <= 1);
                counts.damerau_hard_negative_within_cutoff += usize::from(dam_distance <= 1);
                counts.grapheme_levenshtein_hard_negative_within_cutoff +=
                    usize::from(grapheme_lev_distance <= 1);
                counts.grapheme_damerau_hard_negative_within_cutoff +=
                    usize::from(grapheme_dam_distance <= 1);
            }
            "uncertain" => {
                counts.uncertain_pairs_excluded += 1;
                levenshtein.uncertain_pairs_excluded += 1;
                damerau.uncertain_pairs_excluded += 1;
                grapheme_levenshtein.uncertain_pairs_excluded += 1;
                grapheme_damerau.uncertain_pairs_excluded += 1;
            }
            _ => {
                return Err(format!(
                    "variant pair {} has an invalid label",
                    pair.pair_id
                ));
            }
        }
        levenshtein.all_labeled_pair_distance_count += 1;
        damerau.all_labeled_pair_distance_count += 1;
        levenshtein.all_labeled_pair_distances_within_cutoff += usize::from(lev_distance <= 1);
        damerau.all_labeled_pair_distances_within_cutoff += usize::from(dam_distance <= 1);
        grapheme_levenshtein.all_labeled_pair_distance_count += 1;
        grapheme_damerau.all_labeled_pair_distance_count += 1;
        grapheme_levenshtein.all_labeled_pair_distances_within_cutoff +=
            usize::from(grapheme_lev_distance <= 1);
        grapheme_damerau.all_labeled_pair_distances_within_cutoff +=
            usize::from(grapheme_dam_distance <= 1);
        let script = left_assertion
            .map(|(_, script)| script)
            .unwrap_or("(missing)")
            .to_owned();
        let script_counts = levenshtein.by_script.entry(script.clone()).or_default();
        let dam_script_counts = damerau.by_script.entry(script.clone()).or_default();
        let grapheme_lev_script_counts = grapheme_levenshtein
            .by_script
            .entry(script.clone())
            .or_default();
        let grapheme_dam_script_counts = grapheme_damerau.by_script.entry(script).or_default();
        match pair.label.as_str() {
            "match" => {
                script_counts.positive_pairs += 1;
                dam_script_counts.positive_pairs += 1;
                grapheme_lev_script_counts.positive_pairs += 1;
                grapheme_dam_script_counts.positive_pairs += 1;
                script_counts.levenshtein_positive_within_cutoff += usize::from(lev_distance <= 1);
                dam_script_counts.levenshtein_positive_within_cutoff +=
                    usize::from(lev_distance <= 1);
                script_counts.damerau_positive_within_cutoff += usize::from(dam_distance <= 1);
                dam_script_counts.damerau_positive_within_cutoff += usize::from(dam_distance <= 1);
                grapheme_lev_script_counts.grapheme_levenshtein_positive_within_cutoff +=
                    usize::from(grapheme_lev_distance <= 1);
                grapheme_dam_script_counts.grapheme_levenshtein_positive_within_cutoff +=
                    usize::from(grapheme_lev_distance <= 1);
                grapheme_lev_script_counts.grapheme_damerau_positive_within_cutoff +=
                    usize::from(grapheme_dam_distance <= 1);
                grapheme_dam_script_counts.grapheme_damerau_positive_within_cutoff +=
                    usize::from(grapheme_dam_distance <= 1);
            }
            "non_match" => {
                script_counts.hard_negative_pairs += 1;
                dam_script_counts.hard_negative_pairs += 1;
                grapheme_lev_script_counts.hard_negative_pairs += 1;
                grapheme_dam_script_counts.hard_negative_pairs += 1;
                script_counts.levenshtein_hard_negative_within_cutoff +=
                    usize::from(lev_distance <= 1);
                dam_script_counts.levenshtein_hard_negative_within_cutoff +=
                    usize::from(lev_distance <= 1);
                script_counts.damerau_hard_negative_within_cutoff += usize::from(dam_distance <= 1);
                dam_script_counts.damerau_hard_negative_within_cutoff +=
                    usize::from(dam_distance <= 1);
                grapheme_lev_script_counts.grapheme_levenshtein_hard_negative_within_cutoff +=
                    usize::from(grapheme_lev_distance <= 1);
                grapheme_dam_script_counts.grapheme_levenshtein_hard_negative_within_cutoff +=
                    usize::from(grapheme_lev_distance <= 1);
                grapheme_lev_script_counts.grapheme_damerau_hard_negative_within_cutoff +=
                    usize::from(grapheme_dam_distance <= 1);
                grapheme_dam_script_counts.grapheme_damerau_hard_negative_within_cutoff +=
                    usize::from(grapheme_dam_distance <= 1);
            }
            "uncertain" => {
                script_counts.uncertain_pairs_excluded += 1;
                dam_script_counts.uncertain_pairs_excluded += 1;
                grapheme_lev_script_counts.uncertain_pairs_excluded += 1;
                grapheme_dam_script_counts.uncertain_pairs_excluded += 1;
            }
            _ => {
                return Err(format!(
                    "variant pair {} has an invalid label",
                    pair.pair_id
                ));
            }
        }
    }

    Ok(Some(AlgorithmVariantExperiment {
        status: "offline_synthetic_mechanics_only_not_production_enabled",
        cutoff: 1,
        script_policy: "compare only source_original names with explicit identical script tags; no transliteration or language inference",
        grapheme_unit: "Unicode extended grapheme clusters (UAX #29) after existing normalization; comparator mechanics only, not phonemes or language-specific tokens",
        workload_bounds: VariantWorkloadBounds {
            max_records: 10_000,
            max_queries: 1_000,
            max_comparisons: MAX_VARIANT_COMPARISONS,
            max_distance_cells: MAX_VARIANT_DISTANCE_CELLS,
            candidate_limit: VARIANT_CANDIDATE_LIMIT,
            measured_records: rows.len(),
            measured_queries: queries.len(),
            measured_comparisons: comparisons,
            measured_retrieval_distance_cells: retrieval_distance_cells,
            measured_labeled_pair_distance_cells: labeled_pair_distance_cells,
            measured_distance_cells: distance_cells,
        },
        levenshtein,
        damerau_levenshtein: damerau,
        grapheme_levenshtein,
        grapheme_damerau_levenshtein: grapheme_damerau,
        pair_cases,
        limitations: vec![
            "Constructed synthetic edit cases verify distance mechanics only, not identity accuracy or algorithm superiority.".into(),
            "Hard-negative retrieval is a retrieval-burden count, not a false-positive rate or merge decision.".into(),
            "The fixture carries explicit script tags; no language, transliteration, or cross-script equivalence is inferred.".into(),
            "Public OpenCR algorithm documentation does not identify the deployed OpenCR configuration or thresholds.".into(),
        ],
    }))
}

fn checked_four_comparator_cells(base_cells: usize, stage: &str) -> Result<usize, String> {
    base_cells
        .checked_mul(4)
        .ok_or_else(|| format!("algorithm variant {stage} distance-cell count overflow"))
}

fn variant_distance_cell_work(
    rows: &[&Record],
    queries: &BTreeMap<&str, &Query>,
) -> Result<usize, String> {
    let mut cells = 0usize;
    for query in queries.values() {
        let Some(query_record) = rows
            .iter()
            .find(|record| record.record_id == query.query_record_id)
        else {
            continue;
        };
        let Some((query_name, query_script)) = original_name_and_script(query_record) else {
            continue;
        };
        let query_len = litecr_matching::normalized_assertion_key(query_name)
            .chars()
            .count();
        for candidate in rows {
            let Some((candidate_name, candidate_script)) = original_name_and_script(candidate)
            else {
                continue;
            };
            if candidate.record_id == query_record.record_id || candidate_script != query_script {
                continue;
            }
            let candidate_len = litecr_matching::normalized_assertion_key(candidate_name)
                .chars()
                .count();
            let pair_cells = query_len
                .checked_add(2)
                .and_then(|left| {
                    candidate_len
                        .checked_add(2)
                        .and_then(|right| left.checked_mul(right))
                })
                .ok_or_else(|| {
                    "algorithm variant retrieval distance-cell count overflow".to_owned()
                })?;
            cells = cells.checked_add(pair_cells).ok_or_else(|| {
                "algorithm variant retrieval distance-cell count overflow".to_owned()
            })?;
        }
    }
    Ok(cells)
}

fn variant_pair_distance_cell_work(
    pairs: &[&Pair],
    records_by_id: &BTreeMap<&str, &Record>,
) -> Result<usize, String> {
    let mut cells = 0usize;
    for pair in pairs {
        let left = records_by_id
            .get(pair.left_record_id.as_str())
            .ok_or_else(|| format!("variant pair {} left record missing", pair.pair_id))?;
        let right = records_by_id
            .get(pair.right_record_id.as_str())
            .ok_or_else(|| format!("variant pair {} right record missing", pair.pair_id))?;
        let (left_name, right_name) = matching_script_name_pair(
            original_name_and_script(left),
            original_name_and_script(right),
        )
        .ok_or_else(|| {
            format!(
                "variant pair {} lacks explicit same-script source_original names",
                pair.pair_id
            )
        })?;
        // Scalar lengths upper-bound grapheme lengths, so this conservatively
        // accounts for each comparator's DP work without segmenting twice.
        let left_len = litecr_matching::normalized_assertion_key(left_name)
            .chars()
            .count();
        let right_len = litecr_matching::normalized_assertion_key(right_name)
            .chars()
            .count();
        let pair_cells = left_len
            .checked_add(2)
            .and_then(|left| {
                right_len
                    .checked_add(2)
                    .and_then(|right| left.checked_mul(right))
            })
            .ok_or_else(|| {
                "algorithm variant labeled-pair distance-cell count overflow".to_owned()
            })?;
        cells = cells.checked_add(pair_cells).ok_or_else(|| {
            "algorithm variant labeled-pair distance-cell count overflow".to_owned()
        })?;
    }
    Ok(cells)
}

#[derive(Clone, Copy)]
enum VariantDistance {
    Levenshtein,
    DamerauLevenshtein,
    GraphemeLevenshtein,
    GraphemeDamerauLevenshtein,
}

fn variant_candidates(
    query_record_id: &str,
    query: Option<(&str, &str)>,
    rows: &[&Record],
    algorithm: VariantDistance,
    limit: usize,
) -> (Vec<String>, bool) {
    let Some((query_name, query_script)) = query else {
        return (Vec::new(), false);
    };
    let query_key = litecr_matching::normalized_assertion_key(query_name);
    let mut candidates = Vec::<(usize, String)>::new();
    for record in rows {
        let Some((name, script)) = original_name_and_script(record) else {
            continue;
        };
        if script != query_script || record.record_id == query_record_id {
            continue;
        }
        let key = litecr_matching::normalized_assertion_key(name);
        let distance = match algorithm {
            VariantDistance::Levenshtein => levenshtein_distance(&query_key, &key),
            VariantDistance::DamerauLevenshtein => damerau_levenshtein_distance(&query_key, &key),
            VariantDistance::GraphemeLevenshtein => grapheme_levenshtein_distance(&query_key, &key),
            VariantDistance::GraphemeDamerauLevenshtein => {
                grapheme_damerau_levenshtein_distance(&query_key, &key)
            }
        };
        if distance <= 1 {
            candidates.push((distance, record.record_id.clone()));
        }
    }
    candidates.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    let truncated = candidates.len() > limit;
    (
        candidates
            .into_iter()
            .take(limit)
            .map(|(_, id)| id)
            .collect(),
        truncated,
    )
}

fn record_variant_retrieval(
    metrics: &mut VariantMetrics,
    query: &Query,
    candidate_ids: &[String],
    truncated: bool,
    uncertain_candidates: Option<&BTreeSet<String>>,
) {
    let ids: BTreeSet<_> = candidate_ids.iter().map(String::as_str).collect();
    metrics.queries += 1;
    metrics.positive_candidates += query
        .eligible_match_record_ids
        .iter()
        .filter(|id| !uncertain_candidates.is_some_and(|uncertain| uncertain.contains(id.as_str())))
        .count();
    metrics.positive_candidates_retrieved += query
        .eligible_match_record_ids
        .iter()
        .filter(|id| {
            !uncertain_candidates.is_some_and(|uncertain| uncertain.contains(id.as_str()))
                && ids.contains(id.as_str())
        })
        .count();
    metrics.hard_negatives += query
        .hard_negative_record_ids
        .iter()
        .filter(|id| !uncertain_candidates.is_some_and(|uncertain| uncertain.contains(id.as_str())))
        .count();
    metrics.hard_negatives_retrieved += query
        .hard_negative_record_ids
        .iter()
        .filter(|id| {
            !uncertain_candidates.is_some_and(|uncertain| uncertain.contains(id.as_str()))
                && ids.contains(id.as_str())
        })
        .count();
    metrics.candidates_returned += candidate_ids.len();
    metrics.max_candidates_per_query = metrics.max_candidates_per_query.max(candidate_ids.len());
    metrics.truncated_queries += usize::from(truncated);
}

fn uncertain_candidates_by_query(
    pairs: &[&Pair],
    queries: &BTreeMap<&str, &Query>,
) -> BTreeMap<String, BTreeSet<String>> {
    let mut excluded = BTreeMap::<String, BTreeSet<String>>::new();
    for pair in pairs.iter().filter(|pair| pair.label == "uncertain") {
        if queries.contains_key(pair.left_record_id.as_str()) {
            excluded
                .entry(pair.left_record_id.clone())
                .or_default()
                .insert(pair.right_record_id.clone());
        }
        if queries.contains_key(pair.right_record_id.as_str()) {
            excluded
                .entry(pair.right_record_id.clone())
                .or_default()
                .insert(pair.left_record_id.clone());
        }
    }
    excluded
}

fn matching_script_name_pair<'a>(
    left: Option<(&'a str, &'a str)>,
    right: Option<(&'a str, &'a str)>,
) -> Option<(&'a str, &'a str)> {
    let (left_name, left_script) = left?;
    let (right_name, right_script) = right?;
    (left_script == right_script).then_some((left_name, right_name))
}

fn levenshtein_distance(left: &str, right: &str) -> usize {
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    levenshtein_distance_units(&left, &right)
}

fn grapheme_levenshtein_distance(left: &str, right: &str) -> usize {
    let left_normalized = litecr_matching::normalized_assertion_key(left);
    let right_normalized = litecr_matching::normalized_assertion_key(right);
    let left: Vec<&str> = UnicodeSegmentation::graphemes(left_normalized.as_str(), true).collect();
    let right: Vec<&str> =
        UnicodeSegmentation::graphemes(right_normalized.as_str(), true).collect();
    levenshtein_distance_units(&left, &right)
}

fn levenshtein_distance_units<T: Eq>(left: &[T], right: &[T]) -> usize {
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    let mut current = vec![0; right.len() + 1];
    for (i, a) in left.iter().enumerate() {
        current[0] = i + 1;
        for (j, b) in right.iter().enumerate() {
            current[j + 1] = (previous[j + 1] + 1)
                .min(current[j] + 1)
                .min(previous[j] + usize::from(a != b));
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}

fn damerau_levenshtein_distance(left: &str, right: &str) -> usize {
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    damerau_levenshtein_distance_units(&left, &right)
}

fn grapheme_damerau_levenshtein_distance(left: &str, right: &str) -> usize {
    let left_normalized = litecr_matching::normalized_assertion_key(left);
    let right_normalized = litecr_matching::normalized_assertion_key(right);
    let left: Vec<&str> = UnicodeSegmentation::graphemes(left_normalized.as_str(), true).collect();
    let right: Vec<&str> =
        UnicodeSegmentation::graphemes(right_normalized.as_str(), true).collect();
    damerau_levenshtein_distance_units(&left, &right)
}

fn damerau_levenshtein_distance_units<T: Copy + Eq + std::hash::Hash>(
    left: &[T],
    right: &[T],
) -> usize {
    use std::collections::HashMap;
    let (m, n) = (left.len(), right.len());
    let inf = m + n;
    let mut matrix = vec![vec![0usize; n + 2]; m + 2];
    matrix[0][0] = inf;
    for i in 0..=m {
        matrix[i + 1][0] = inf;
        matrix[i + 1][1] = i;
    }
    for j in 0..=n {
        matrix[0][j + 1] = inf;
        matrix[1][j + 1] = j;
    }
    let mut last_seen = HashMap::<T, usize>::new();
    for i in 1..=m {
        let mut last_match_column = 0;
        for j in 1..=n {
            let prior_row = *last_seen.get(&right[j - 1]).unwrap_or(&0);
            let prior_column = last_match_column;
            let substitution_cost = usize::from(left[i - 1] != right[j - 1]);
            if substitution_cost == 0 {
                last_match_column = j;
            }
            matrix[i + 1][j + 1] = (matrix[i][j] + substitution_cost)
                .min(matrix[i + 1][j] + 1)
                .min(matrix[i][j + 1] + 1)
                .min(
                    matrix[prior_row][prior_column]
                        + (i - prior_row - 1)
                        + 1
                        + (j - prior_column - 1),
                );
        }
        last_seen.insert(left[i - 1], i);
    }
    matrix[m + 1][n + 1]
}

#[derive(Default)]
struct ExperimentCounts {
    positive_queries: usize,
    positive_queries_with_retrieved_match: usize,
    positives: usize,
    positives_retrieved: usize,
    hard_negatives: usize,
    hard_negatives_retrieved: usize,
    candidates: usize,
    max_candidates_per_query: usize,
    truncated_queries: usize,
    by_script: BTreeMap<String, ExperimentSubgroupCounts>,
    by_language: BTreeMap<String, ExperimentSubgroupCounts>,
    by_case_tag: BTreeMap<String, ExperimentSubgroupCounts>,
    by_script_case: BTreeMap<String, ExperimentSubgroupCounts>,
    by_language_case: BTreeMap<String, ExperimentSubgroupCounts>,
    missing_attribution: MissingSubgroupAttribution,
}
impl ExperimentCounts {
    fn record(
        &mut self,
        query: &Query,
        rows: &[&Record],
        candidates: &BTreeSet<&str>,
        truncated: bool,
    ) {
        let positives_retrieved = query
            .eligible_match_record_ids
            .iter()
            .filter(|id| candidates.contains(id.as_str()))
            .count();
        let negatives_retrieved = query
            .hard_negative_record_ids
            .iter()
            .filter(|id| candidates.contains(id.as_str()))
            .count();
        let query_record = rows
            .iter()
            .find(|record| record.record_id == query.query_record_id)
            .expect("query record was validated against the selected split");
        let (script, language) = query_subgroups(query_record);
        let observation = ExperimentSubgroupObservation {
            positives: query.eligible_match_record_ids.len(),
            positives_retrieved,
            hard_negatives: query.hard_negative_record_ids.len(),
            hard_negatives_retrieved: negatives_retrieved,
            candidates: candidates.len(),
            truncated,
        };
        record_experiment_subgroup(&mut self.by_script, subgroup_key(script), observation);
        if script.is_none() {
            self.missing_attribution.queries_missing_script += 1;
            self.missing_attribution
                .positive_candidates_on_queries_missing_script +=
                query.eligible_match_record_ids.len();
            self.missing_attribution
                .hard_negatives_on_queries_missing_script += query.hard_negative_record_ids.len();
        }
        if language.is_none() {
            self.missing_attribution.queries_missing_language += 1;
            self.missing_attribution
                .positive_candidates_on_queries_missing_language +=
                query.eligible_match_record_ids.len();
            self.missing_attribution
                .hard_negatives_on_queries_missing_language += query.hard_negative_record_ids.len();
        }
        record_experiment_subgroup(&mut self.by_language, subgroup_key(language), observation);
        for tag in &query.case_tags {
            record_experiment_subgroup(&mut self.by_case_tag, tag.clone(), observation);
            record_experiment_subgroup(
                &mut self.by_script_case,
                format!("{}/{}", subgroup_key(script), tag),
                observation,
            );
            record_experiment_subgroup(
                &mut self.by_language_case,
                format!("{}/{}", subgroup_key(language), tag),
                observation,
            );
        }
        let positive_query = !query.eligible_match_record_ids.is_empty();
        let positive_query_hit = positive_query && positives_retrieved > 0;
        self.positive_queries += usize::from(positive_query);
        self.positive_queries_with_retrieved_match += usize::from(positive_query_hit);
        self.positives += query.eligible_match_record_ids.len();
        self.positives_retrieved += positives_retrieved;
        self.hard_negatives += query.hard_negative_record_ids.len();
        self.hard_negatives_retrieved += negatives_retrieved;
        self.candidates += candidates.len();
        self.max_candidates_per_query = self.max_candidates_per_query.max(candidates.len());
        self.truncated_queries += usize::from(truncated);
    }

    fn finish(self, queries: usize, elapsed_microseconds: u128) -> ExperimentMetrics {
        ExperimentMetrics {
            queries,
            positive_queries: self.positive_queries,
            positive_queries_with_retrieved_match: self.positive_queries_with_retrieved_match,
            positive_query_recall: (self.positive_queries > 0).then_some(
                self.positive_queries_with_retrieved_match as f64 / self.positive_queries as f64,
            ),
            positives: self.positives,
            positives_retrieved: self.positives_retrieved,
            candidate_recall: (self.positives > 0)
                .then_some(self.positives_retrieved as f64 / self.positives as f64),
            hard_negatives: self.hard_negatives,
            hard_negatives_retrieved: self.hard_negatives_retrieved,
            candidate_count: self.candidates,
            max_candidates_per_query: self.max_candidates_per_query,
            truncated_queries: self.truncated_queries,
            elapsed_microseconds,
            by_script: finish_experiment_subgroups(self.by_script),
            by_language: finish_experiment_subgroups(self.by_language),
            by_case_tag: finish_experiment_subgroups(self.by_case_tag),
            by_script_case: finish_experiment_subgroups(self.by_script_case),
            by_language_case: finish_experiment_subgroups(self.by_language_case),
            missing_attribution: self.missing_attribution,
        }
    }
}

fn match_records_for_query(
    query_id: &str,
    rows: &[MatchRecord],
    limit: usize,
) -> Result<litecr_matching::MatchResult, String> {
    litecr_matching::match_records(query_id, rows, limit)
        .map_err(|error| format!("matcher failed for {query_id}: {error:?}"))
}

fn approximate_name_candidates(
    query: &Query,
    rows: &[&Record],
) -> Result<Vec<(String, usize)>, String> {
    let query_record = rows
        .iter()
        .find(|record| record.record_id == query.query_record_id)
        .ok_or_else(|| format!("query record {} is missing", query.query_record_id))?;
    let Some((query_name, query_script)) = original_name_and_script(query_record) else {
        return Ok(Vec::new());
    };
    let query_key = litecr_matching::normalized_assertion_key(query_name);
    if query_key.is_empty() {
        return Ok(Vec::new());
    }
    let mut candidates = Vec::new();
    for record in rows {
        if record.record_id == query_record.record_id {
            continue;
        }
        let Some((name, script)) = original_name_and_script(record) else {
            continue;
        };
        if script != query_script {
            continue;
        }
        let key = litecr_matching::normalized_assertion_key(name);
        if key.chars().count() < 2 {
            continue;
        }
        let distance = unicode_scalar_distance_at_most_one(&query_key, &key);
        if let Some(distance) = distance {
            candidates.push((record.record_id.clone(), distance));
        }
    }
    candidates.sort_by(|left, right| left.1.cmp(&right.1).then(left.0.cmp(&right.0)));
    Ok(candidates)
}

fn original_name_and_script(record: &Record) -> Option<(&str, &str)> {
    let assertion = record
        .name_assertions
        .iter()
        .find(|name| name.representation.as_deref() == Some("source_original"))?;
    let script = assertion.script.as_deref()?.trim();
    if script.is_empty() {
        return None;
    }
    Some((assertion.original_value.as_str(), script))
}

fn unicode_scalar_distance_at_most_one(left: &str, right: &str) -> Option<usize> {
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    if left.len().abs_diff(right.len()) > EXPERIMENT_MAX_EDIT_DISTANCE {
        return None;
    }
    let mut i = 0;
    let mut j = 0;
    let mut edits = 0;
    while i < left.len() && j < right.len() {
        if left[i] == right[j] {
            i += 1;
            j += 1;
        } else {
            edits += 1;
            if edits > EXPERIMENT_MAX_EDIT_DISTANCE {
                return None;
            }
            match left.len().cmp(&right.len()) {
                std::cmp::Ordering::Greater => i += 1,
                std::cmp::Ordering::Less => j += 1,
                std::cmp::Ordering::Equal => {
                    i += 1;
                    j += 1;
                }
            }
        }
    }
    if i < left.len() || j < right.len() {
        edits += 1;
    }
    (edits <= EXPERIMENT_MAX_EDIT_DISTANCE).then_some(edits)
}

fn pair_buckets_count(p: &[Pair], split: &str) -> usize {
    p.iter().filter(|x| x.split == split).count()
}

fn record_pair_observation(
    summaries: &mut BTreeMap<String, PairSummary>,
    label: &str,
    candidate: Option<&CandidateEvidence>,
) {
    let summary = summaries.entry(label.to_owned()).or_default();
    summary.pairs += 1;
    *summary
        .outcome_counts
        .entry(pair_outcome_name(candidate).to_owned())
        .or_default() += 1;
    if let Some(candidate) = candidate {
        summary.scored_pairs += 1;
        summary.raw_point_values.push(candidate.score_points);
    }
}

fn pair_outcome_name(candidate: Option<&CandidateEvidence>) -> &'static str {
    match candidate.map(|candidate| candidate.outcome) {
        None => "not_retrieved",
        Some(AdvisoryOutcome::Review) => "review",
        Some(AdvisoryOutcome::InsufficientEvidence) => "insufficient_evidence",
        Some(AdvisoryOutcome::ConflictReview) => "conflict_review",
    }
}

fn finish_pair_summaries(
    summaries: BTreeMap<String, PairSummary>,
) -> BTreeMap<String, PairSummary> {
    summaries
        .into_iter()
        .map(|(label, mut summary)| {
            let score_sum: i64 = summary
                .raw_point_values
                .iter()
                .map(|score| i64::from(*score))
                .sum();
            summary.mean_raw_points = (summary.scored_pairs > 0)
                .then_some(score_sum as f64 / summary.scored_pairs as f64);
            summary.score_type = "raw_heuristic_points_not_probability".into();
            (label, summary)
        })
        .collect()
}
fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 15) as usize] as char);
    }
    out
}
fn pass_name(pass: litecr_matching::Pass) -> &'static str {
    match pass {
        litecr_matching::Pass::ExactName => "exact_name",
        litecr_matching::Pass::PartialBirthYear => "partial_birth_year",
        litecr_matching::Pass::AuthoritativeIdentifier => "authoritative_identifier",
        litecr_matching::Pass::NameVariant => "name_variant",
        litecr_matching::Pass::IncompleteName => "incomplete_name",
    }
}
fn to_match_record(r: &Record) -> MatchRecord {
    let names: Vec<MatchAssertion> = r
        .name_assertions
        .iter()
        .map(|n| MatchAssertion {
            field_path: "name.original".into(),
            original_value: n.original_value.clone(),
            language_tag: n.language.clone(),
            script: n.script.clone(),
            representation: n.representation.clone(),
            normalization_version: n.normalization_version.clone(),
        })
        .collect();
    let identifiers = r
        .identifiers
        .iter()
        .map(|i| MatchIdentifier {
            authority: i.authority.clone(),
            identifier_type: i.identifier_type.clone(),
            value: i.original_value.clone(),
            authoritative: true,
            status: i.status.clone(),
        })
        .collect();
    MatchRecord {
        record_id: r.record_id.clone(),
        registry_id: "evaluation-synthetic-scope".into(),
        organization_id: r.split.clone(),
        source_version: None,
        receive_version: None,
        name_assertions: names,
        assertions: vec![],
        date_of_birth: r.date_of_birth.clone(),
        identifiers,
        contacts: r.contacts.clone(),
        address_token: r.address_token.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, rc::Rc};

    struct CountingReader {
        bytes: Vec<u8>,
        consumed: Rc<Cell<usize>>,
    }

    impl Read for CountingReader {
        fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
            let count = output.len().min(self.bytes.len());
            output[..count].copy_from_slice(&self.bytes[..count]);
            self.bytes.drain(..count);
            self.consumed.set(self.consumed.get() + count);
            Ok(count)
        }
    }

    #[test]
    fn bounded_read_stops_after_limit_plus_one_without_consuming_whole_input() {
        let consumed = Rc::new(Cell::new(0));
        let reader = CountingReader {
            bytes: vec![b'x'; 10_000],
            consumed: consumed.clone(),
        };
        let error = read_limited(reader, 64, "test artifact").unwrap_err();
        assert!(error.contains("size limit"));
        assert_eq!(consumed.get(), 65);
    }

    #[test]
    fn candidate_count_distribution_includes_empty_queries_and_uses_nearest_rank_p95() {
        let distribution = candidate_count_distribution(vec![3, 0, 2, 1]).unwrap();
        assert_eq!(distribution.queries, 4);
        assert_eq!(distribution.min, 0);
        assert_eq!(distribution.mean, 1.5);
        assert_eq!(distribution.median, 1.5);
        assert_eq!(distribution.p95_nearest_rank, 3);
        assert_eq!(distribution.max, 3);

        let all_empty = candidate_count_distribution(vec![0, 0, 0]).unwrap();
        assert_eq!(all_empty.mean, 0.0);
        assert_eq!(all_empty.median, 0.0);
        assert_eq!(all_empty.p95_nearest_rank, 0);
        assert!(candidate_count_distribution(Vec::new()).is_none());
    }

    fn candidate(outcome: AdvisoryOutcome, score_points: i16) -> CandidateEvidence {
        CandidateEvidence {
            candidate_record_id: "candidate-1".into(),
            candidate_source_version: None,
            candidate_receive_version: None,
            passes: Vec::new(),
            score_points,
            score_type: "raw_heuristic_points".into(),
            outcome,
            fields: Vec::new(),
            hard_conflicts: Vec::new(),
        }
    }

    #[test]
    fn pair_outcomes_keep_uncertain_and_not_retrieved_separate_from_scores() {
        let mut summaries = BTreeMap::new();
        let review = candidate(AdvisoryOutcome::Review, 12);
        let conflict = candidate(AdvisoryOutcome::ConflictReview, -4);
        let insufficient = candidate(AdvisoryOutcome::InsufficientEvidence, 0);

        record_pair_observation(&mut summaries, "match", Some(&review));
        record_pair_observation(&mut summaries, "match", None);
        record_pair_observation(&mut summaries, "non_match", Some(&conflict));
        record_pair_observation(&mut summaries, "uncertain", Some(&insufficient));

        let summaries = finish_pair_summaries(summaries);
        let matches = &summaries["match"];
        assert_eq!(matches.pairs, 2);
        assert_eq!(matches.scored_pairs, 1);
        assert_eq!(matches.outcome_counts["review"], 1);
        assert_eq!(matches.outcome_counts["not_retrieved"], 1);
        assert_eq!(matches.mean_raw_points, Some(12.0));
        assert_eq!(matches.raw_point_values, vec![12]);

        let non_matches = &summaries["non_match"];
        assert_eq!(non_matches.pairs, 1);
        assert_eq!(non_matches.outcome_counts["conflict_review"], 1);
        assert_eq!(non_matches.mean_raw_points, Some(-4.0));

        let uncertain = &summaries["uncertain"];
        assert_eq!(uncertain.pairs, 1);
        assert_eq!(uncertain.outcome_counts["insufficient_evidence"], 1);
        assert_eq!(uncertain.scored_pairs, 1);
    }

    fn validation_record(record_id: &str, cluster: &str, split: &str) -> Record {
        Record {
            record_id: record_id.into(),
            gold_cluster_id: cluster.into(),
            split: split.into(),
            name_assertions: Vec::new(),
            date_of_birth: None,
            identifiers: Vec::new(),
            contacts: Vec::new(),
            address_token: None,
        }
    }

    fn synthetic_dataset_metadata() -> Dataset {
        Dataset {
            schema_version: "1.0".into(),
            dataset_id: "synthetic-metadata-test".into(),
            dataset_status: "synthetic_fixture".into(),
            purpose: "mechanics_only".into(),
            provenance: serde_json::json!({"contains_real_person_data": false}),
            records: Vec::new(),
            pair_labels: Vec::new(),
            queries: Vec::new(),
        }
    }

    #[test]
    fn evaluator_accepts_only_explicit_synthetic_mechanics_metadata() {
        let mut dataset = synthetic_dataset_metadata();
        assert!(validate_dataset_metadata(&dataset).is_ok());

        dataset.dataset_status = "authorized_controlled".into();
        assert!(
            validate_dataset_metadata(&dataset)
                .unwrap_err()
                .contains("controlled-cohort evaluation is not available")
        );

        dataset.dataset_status = "synthetic_fixture".into();
        dataset.purpose = "held_out_evaluation".into();
        assert!(
            validate_dataset_metadata(&dataset)
                .unwrap_err()
                .contains("accuracy evaluation is not available")
        );

        dataset.purpose = "mechanics_only".into();
        dataset.provenance = serde_json::json!({"contains_real_person_data": true});
        assert!(
            validate_dataset_metadata(&dataset)
                .unwrap_err()
                .contains("contains_real_person_data=false")
        );
    }

    #[test]
    fn evaluator_rejects_cross_split_and_gold_inconsistent_pairs() {
        let records = vec![
            validation_record("a", "person-a", "held_out_test"),
            validation_record("b", "person-b", "held_out_test"),
            validation_record("c", "person-c", "development"),
        ];
        let cross_split = Pair {
            pair_id: "pair-a-c".into(),
            left_record_id: "a".into(),
            right_record_id: "c".into(),
            split: "held_out_test".into(),
            label: "non_match".into(),
            case_tags: Vec::new(),
        };
        assert!(
            validate_dataset_relations(&records, &[cross_split], &[], "held_out_test")
                .unwrap_err()
                .contains("crosses split boundaries")
        );

        let mislabeled = Pair {
            pair_id: "pair-a-b".into(),
            left_record_id: "a".into(),
            right_record_id: "b".into(),
            split: "held_out_test".into(),
            label: "match".into(),
            case_tags: Vec::new(),
        };
        assert!(
            validate_dataset_relations(&records, &[mislabeled], &[], "held_out_test")
                .unwrap_err()
                .contains("conflicts with gold clusters")
        );
    }

    #[test]
    fn evaluator_rejects_gold_identity_reuse_across_splits() {
        let records = vec![
            validation_record("train-record", "same-person", "train"),
            validation_record("held-out-record", "same-person", "held_out_test"),
        ];

        let error = validate_dataset_relations(&records, &[], &[], "held_out_test")
            .expect_err("one identity must not cross the evaluation split boundary");
        assert_eq!(error, "a gold identity cluster appears in multiple splits");
    }

    #[test]
    fn evaluator_rejects_query_positive_that_conflicts_with_gold_cluster() {
        let records = vec![
            validation_record("query", "person-a", "held_out_test"),
            validation_record("candidate", "person-b", "held_out_test"),
        ];
        let query = Query {
            query_id: "query-1".into(),
            query_record_id: "query".into(),
            split: "held_out_test".into(),
            eligible_match_record_ids: vec!["candidate".into()],
            hard_negative_record_ids: Vec::new(),
            case_tags: Vec::new(),
        };
        assert!(
            validate_dataset_relations(&records, &[], &[query], "held_out_test")
                .unwrap_err()
                .contains("positive conflicts with gold clusters")
        );
    }

    #[test]
    fn evaluator_requires_exhaustive_positive_universe_per_query() {
        let records = vec![
            validation_record("query", "same-person", "held_out_test"),
            validation_record("missed", "same-person", "held_out_test"),
        ];
        let query = Query {
            query_id: "query-1".into(),
            query_record_id: "query".into(),
            split: "held_out_test".into(),
            eligible_match_record_ids: Vec::new(),
            hard_negative_record_ids: Vec::new(),
            case_tags: Vec::new(),
        };
        assert!(
            validate_dataset_relations(&records, &[], &[query], "held_out_test")
                .unwrap_err()
                .contains("positive truth is not exhaustive")
        );
    }

    #[test]
    fn content_digests_change_with_inputs_and_ignore_record_order() {
        let mut first = validation_record("record-a", "person-a", "held_out_test");
        first.name_assertions.push(RawName {
            original_value: "ຊື່ທົດສອບ".into(),
            script: Some("Laoo".into()),
            language: Some("lo".into()),
            representation: Some("source_original".into()),
            normalization_version: Some("fixture-raw-v1".into()),
        });
        let second = validation_record("record-b", "person-b", "held_out_test");

        assert_eq!(
            digest_matcher_inputs(&[&first, &second]).unwrap(),
            digest_matcher_inputs(&[&second, &first]).unwrap(),
            "record order must not change the matcher-input digest"
        );

        let mut changed = first.clone();
        changed.name_assertions[0].original_value.push('!');
        assert_ne!(
            digest_matcher_inputs(&[&changed, &second]).unwrap(),
            digest_matcher_inputs(&[&first, &second]).unwrap(),
            "a matcher-visible source-value change must change the digest"
        );
    }

    #[test]
    fn label_query_manifest_digest_tracks_pair_labels() {
        let pair = Pair {
            pair_id: "pair-1".into(),
            left_record_id: "record-a".into(),
            right_record_id: "record-b".into(),
            split: "held_out_test".into(),
            label: "match".into(),
            case_tags: vec!["synthetic-control".into()],
        };
        let dataset = Dataset {
            schema_version: "1.0".into(),
            dataset_id: "synthetic-digest-test".into(),
            dataset_status: "synthetic_fixture".into(),
            purpose: "mechanics_only".into(),
            provenance: serde_json::json!({"contains_real_person_data": false}),
            records: Vec::new(),
            pair_labels: vec![pair],
            queries: Vec::new(),
        };
        let original = digest_label_query_manifest(&dataset, "held_out_test").unwrap();
        let mut reordered = dataset;
        reordered.pair_labels.reverse();
        assert_eq!(
            original,
            digest_label_query_manifest(&reordered, "held_out_test").unwrap(),
            "manifest array order must not alter the digest"
        );
        reordered.pair_labels[0].label = "uncertain".into();
        assert_ne!(
            original,
            digest_label_query_manifest(&reordered, "held_out_test").unwrap(),
            "a label change must change the manifest digest"
        );
    }

    #[test]
    fn subgroup_metrics_report_per_script_rates_and_keep_missing_metadata_visible() {
        let mut groups = BTreeMap::new();
        record_subgroup(&mut groups, "Laoo".into(), 2, 1, 3, 2);
        record_subgroup(&mut groups, "(missing)".into(), 1, 0, 2, 0);
        let report = finish_subgroups(groups);

        assert_eq!(report["Laoo"].queries, 1);
        assert_eq!(report["Laoo"].positive_candidates, 2);
        assert_eq!(report["Laoo"].positive_candidates_retrieved, 1);
        assert_eq!(report["Laoo"].candidate_recall, Some(0.5));
        assert_eq!(report["Laoo"].hard_negatives, 3);
        assert_eq!(report["Laoo"].hard_negatives_retrieved, 2);
        assert_eq!(report["Laoo"].hard_negative_retrieval_rate, Some(2.0 / 3.0));

        assert_eq!(report["(missing)"].queries, 1);
        assert_eq!(report["(missing)"].candidate_recall, Some(0.0));
        assert_eq!(report["(missing)"].hard_negative_retrieval_rate, Some(0.0));
    }

    #[test]
    fn subgroup_anchor_prefers_original_name_and_counts_missing_dimension_independently() {
        let record = Record {
            record_id: "q".into(),
            gold_cluster_id: "cluster-q".into(),
            split: "held_out_test".into(),
            name_assertions: vec![
                RawName {
                    original_value: "Latin variant".into(),
                    script: Some("Latn".into()),
                    language: Some("en".into()),
                    representation: Some("transliteration".into()),
                    normalization_version: Some("fixture-translit-v1".into()),
                },
                RawName {
                    original_value: "ຊື່ຕົວຢ່າງ".into(),
                    script: Some("Laoo".into()),
                    language: None,
                    representation: Some("source_original".into()),
                    normalization_version: Some("fixture-raw-v1".into()),
                },
            ],
            date_of_birth: None,
            identifiers: Vec::new(),
            contacts: Vec::new(),
            address_token: None,
        };

        assert_eq!(query_subgroups(&record), (Some("Laoo"), None));
        assert_eq!(
            query_subgroups(&Record {
                name_assertions: Vec::new(),
                ..record
            }),
            (None, None)
        );
    }

    #[test]
    fn subgroup_attribution_does_not_infer_locale_or_script_from_derived_names() {
        let record = Record {
            record_id: "derived-only".into(),
            gold_cluster_id: "cluster-derived-only".into(),
            split: "held_out_test".into(),
            name_assertions: vec![RawName {
                original_value: "derived Latin form".into(),
                script: Some("Latn".into()),
                language: Some("en".into()),
                representation: Some("transliteration".into()),
                normalization_version: Some("fixture-translit-v1".into()),
            }],
            date_of_birth: None,
            identifiers: Vec::new(),
            contacts: Vec::new(),
            address_token: None,
        };

        assert_eq!(
            query_subgroups(&record),
            (None, None),
            "derived name metadata must not be reported as source-original subgroup attribution"
        );
    }

    #[test]
    fn approximate_edit_distance_counts_unicode_scalars_not_utf8_bytes() {
        assert_eq!(unicode_scalar_distance_at_most_one("ກຸ", "ກູ"), Some(1));
        assert_eq!(unicode_scalar_distance_at_most_one("क़लम", "कलम"), Some(1));
        assert_eq!(unicode_scalar_distance_at_most_one("abc", "abxc"), Some(1));
        assert_eq!(unicode_scalar_distance_at_most_one("abc", "axyz"), None);
        assert_eq!(unicode_scalar_distance_at_most_one("", "क"), Some(1));
    }

    #[test]
    fn edit_variants_distinguish_adjacent_transposition_and_scalar_edits() {
        for (left, right) in [("ABCD", "ABDC"), ("ກຂຄງ", "ກຄຂງ"), ("ابتث", "ابثت")]
        {
            assert_eq!(levenshtein_distance(left, right), 2);
            assert_eq!(damerau_levenshtein_distance(left, right), 1);
        }
        for (left, right) in [("ABCD", "ABXD"), ("ABCD", "ABXCD"), ("ABCD", "ABD")] {
            assert_eq!(levenshtein_distance(left, right), 1);
            assert_eq!(damerau_levenshtein_distance(left, right), 1);
        }
        assert_eq!(damerau_levenshtein_distance("CA", "ABC"), 2);
    }

    #[test]
    fn grapheme_distances_normalize_combining_sequences_and_handle_indic_clusters() {
        let composed = "é";
        let decomposed = "e\u{0301}";
        assert_eq!(
            litecr_matching::normalized_assertion_key(composed),
            litecr_matching::normalized_assertion_key(decomposed)
        );
        assert_eq!(grapheme_levenshtein_distance(composed, decomposed), 0);
        assert_eq!(
            grapheme_damerau_levenshtein_distance(composed, decomposed),
            0
        );

        let stacked_combining_sequence = "a\u{0301}\u{0323}";
        assert_eq!(
            UnicodeSegmentation::graphemes(stacked_combining_sequence, true).count(),
            1
        );
        assert_eq!(levenshtein_distance(stacked_combining_sequence, "b"), 3);
        assert_eq!(
            grapheme_levenshtein_distance(stacked_combining_sequence, "b"),
            1
        );

        // Devanagari KA + VIRAMA + SSA forms one extended grapheme cluster under UAX #29.
        let conjunct = "क्ष";
        assert_eq!(UnicodeSegmentation::graphemes(conjunct, true).count(), 1);
        assert_eq!(levenshtein_distance(conjunct, "क"), 2);
        assert_eq!(grapheme_levenshtein_distance(conjunct, "क"), 1);
        assert_eq!(grapheme_damerau_levenshtein_distance(conjunct, "क"), 1);
    }

    #[test]
    fn grapheme_mechanics_cover_additional_script_mark_sequences() {
        // These isolated codepoint controls test Unicode segmentation and edit
        // units only. They are not names, language-specific matching rules, or
        // evidence that any edit is valid in a language or identity context.
        let controls = [
            ("Lao tone mark", "ກ່", "ຂ"),
            ("Thai tone mark", "ก่", "ข"),
            ("Khmer dependent vowel", "កា", "គ"),
            ("Arabic combining fatha", "بَ", "ت"),
        ];

        for (label, left, right) in controls {
            assert_eq!(
                UnicodeSegmentation::graphemes(left, true).count(),
                1,
                "{label} left control should be one extended grapheme"
            );
            assert_eq!(
                UnicodeSegmentation::graphemes(right, true).count(),
                1,
                "{label} right control should be one extended grapheme"
            );
            assert_eq!(
                levenshtein_distance(left, right),
                2,
                "{label} scalar control should require two edits"
            );
            assert_eq!(
                grapheme_levenshtein_distance(left, right),
                1,
                "{label} grapheme control should require one edit"
            );
            assert_eq!(
                grapheme_damerau_levenshtein_distance(left, right),
                1,
                "{label} grapheme Damerau control should require one edit"
            );
        }
    }

    #[test]
    fn variant_candidate_cap_reports_truncation_and_requires_explicit_script() {
        let make_record = |id: &str, name: &str, script: Option<&str>| Record {
            record_id: id.into(),
            gold_cluster_id: id.into(),
            split: "held_out_test".into(),
            name_assertions: vec![RawName {
                original_value: name.into(),
                script: script.map(str::to_owned),
                language: None,
                representation: Some("source_original".into()),
                normalization_version: Some("fixture-raw-v1".into()),
            }],
            date_of_birth: None,
            identifiers: Vec::new(),
            contacts: Vec::new(),
            address_token: None,
        };
        let anchor = make_record("q", "ABCD", Some("Latn"));
        let near_one = make_record("a", "ABXD", Some("Latn"));
        let near_two = make_record("b", "ABYD", Some("Latn"));
        let cross_script = make_record("c", "ABCD", Some("Cyrl"));
        let rows = [&anchor, &near_one, &near_two, &cross_script];

        let (candidates, truncated) = variant_candidates(
            "q",
            original_name_and_script(&anchor),
            &rows,
            VariantDistance::Levenshtein,
            1,
        );
        assert_eq!(candidates.len(), 1);
        assert!(truncated);
        assert!(!candidates.contains(&"c".to_owned()));
        assert!(
            variant_candidates("q", None, &rows, VariantDistance::DamerauLevenshtein, 10)
                .0
                .is_empty()
        );

        let conjunct_anchor = make_record("q-cluster", "क्ष", Some("Deva"));
        let conjunct_near = make_record("near-cluster", "क", Some("Deva"));
        let cluster_rows = [&conjunct_anchor, &conjunct_near];
        assert!(
            variant_candidates(
                "q-cluster",
                original_name_and_script(&conjunct_anchor),
                &cluster_rows,
                VariantDistance::Levenshtein,
                10,
            )
            .0
            .is_empty()
        );
        assert_eq!(
            variant_candidates(
                "q-cluster",
                original_name_and_script(&conjunct_anchor),
                &cluster_rows,
                VariantDistance::GraphemeLevenshtein,
                10,
            )
            .0,
            vec!["near-cluster"]
        );
    }

    #[test]
    fn algorithm_variant_fixture_reports_mechanics_without_thresholding_production() {
        let dataset: Dataset = serde_json::from_str(include_str!(
            "../../fixtures/matching/algorithm-variants.json"
        ))
        .expect("algorithm variant fixture parses");
        let rows: Vec<_> = dataset
            .records
            .iter()
            .filter(|record| record.split == "held_out_test")
            .collect();
        let queries: BTreeMap<_, _> = dataset
            .queries
            .iter()
            .filter(|query| query.split == "held_out_test")
            .map(|query| (query.query_record_id.as_str(), query))
            .collect();
        let report = run_algorithm_variant_experiment(&dataset, &rows, &queries, "held_out_test")
            .expect("bounded synthetic experiment")
            .expect("variant cases exist");

        assert_eq!(report.levenshtein.positive_candidates_retrieved, 21);
        assert_eq!(report.damerau_levenshtein.positive_candidates_retrieved, 28);
        assert_eq!(
            report.grapheme_levenshtein.positive_candidates_retrieved,
            21
        );
        assert_eq!(
            report
                .grapheme_damerau_levenshtein
                .positive_candidates_retrieved,
            28
        );
        assert_eq!(report.levenshtein.hard_negatives_retrieved, 7);
        assert_eq!(report.damerau_levenshtein.hard_negatives_retrieved, 7);
        assert_eq!(report.levenshtein.truncated_queries, 0);
        assert_eq!(report.damerau_levenshtein.truncated_queries, 0);
        assert_eq!(report.grapheme_levenshtein.truncated_queries, 0);
        assert_eq!(report.grapheme_damerau_levenshtein.truncated_queries, 0);
        assert_eq!(
            report.pair_cases["variant-transposition"].levenshtein_positive_within_cutoff,
            0
        );
        assert_eq!(
            report.pair_cases["variant-transposition"].damerau_positive_within_cutoff,
            7
        );
        assert_eq!(
            report.status,
            "offline_synthetic_mechanics_only_not_production_enabled"
        );
    }

    #[test]
    fn uncertain_variant_pairs_are_reported_but_excluded_from_hard_negative_counts() {
        let mut dataset: Dataset = serde_json::from_str(include_str!(
            "../../fixtures/matching/algorithm-variants.json"
        ))
        .expect("algorithm variant fixture parses");
        let by_record: BTreeMap<_, _> = dataset
            .records
            .iter()
            .map(|record| (record.record_id.as_str(), record))
            .collect();
        let (query, candidate_id, script) = dataset
            .queries
            .iter()
            .find_map(|query| {
                query
                    .hard_negative_record_ids
                    .iter()
                    .find_map(|candidate_id| {
                        let anchor = by_record.get(query.query_record_id.as_str())?;
                        let candidate = by_record.get(candidate_id.as_str())?;
                        let anchor_script = original_name_and_script(anchor)?.1;
                        let candidate_script = original_name_and_script(candidate)?.1;
                        (anchor_script == candidate_script).then_some((
                            query,
                            candidate_id,
                            anchor_script,
                        ))
                    })
            })
            .expect("same-script query hard-negative control exists");
        let uncertain = Pair {
            pair_id: "uncertain-hard-negative-control".into(),
            left_record_id: query.query_record_id.clone(),
            right_record_id: candidate_id.clone(),
            split: query.split.clone(),
            label: "uncertain".into(),
            case_tags: vec!["variant-hard-negative".into()],
        };
        dataset.pair_labels.push(uncertain);

        let rows: Vec<_> = dataset
            .records
            .iter()
            .filter(|record| record.split == "held_out_test")
            .collect();
        let queries: BTreeMap<_, _> = dataset
            .queries
            .iter()
            .filter(|query| query.split == "held_out_test")
            .map(|query| (query.query_record_id.as_str(), query))
            .collect();
        let report = run_algorithm_variant_experiment(&dataset, &rows, &queries, "held_out_test")
            .expect("bounded synthetic experiment")
            .expect("variant cases exist");

        for metrics in [
            &report.levenshtein,
            &report.damerau_levenshtein,
            &report.grapheme_levenshtein,
            &report.grapheme_damerau_levenshtein,
        ] {
            assert_eq!(metrics.uncertain_pairs_excluded, 1);
            assert_eq!(metrics.hard_negatives, 6);
            assert_eq!(metrics.hard_negatives_retrieved, 6);
        }
        let hard_negative_case = &report.pair_cases["variant-hard-negative"];
        assert_eq!(hard_negative_case.hard_negative_pairs, 7);
        assert_eq!(hard_negative_case.uncertain_pairs_excluded, 1);
        assert_eq!(
            hard_negative_case.levenshtein_hard_negative_within_cutoff,
            7
        );
        assert_eq!(hard_negative_case.damerau_hard_negative_within_cutoff, 7);
        assert_eq!(
            hard_negative_case.grapheme_levenshtein_hard_negative_within_cutoff,
            7
        );
        assert_eq!(
            hard_negative_case.grapheme_damerau_hard_negative_within_cutoff,
            7
        );
        let script_case = &report.grapheme_levenshtein.by_script[script];
        assert_eq!(script_case.hard_negative_pairs, 1);
        assert_eq!(script_case.uncertain_pairs_excluded, 1);
        assert_eq!(
            script_case.grapheme_levenshtein_hard_negative_within_cutoff,
            1
        );
        assert_eq!(
            report.grapheme_levenshtein.all_labeled_pair_distance_count,
            36
        );
    }

    #[test]
    fn repeated_labeled_pairs_are_included_in_and_rejected_by_work_budget() {
        let mut dataset: Dataset = serde_json::from_str(include_str!(
            "../../fixtures/matching/algorithm-variants.json"
        ))
        .expect("algorithm variant fixture parses");
        for record in &mut dataset.records {
            record.name_assertions[0].original_value = "A".repeat(256);
        }
        let repeated = dataset
            .pair_labels
            .iter()
            .find(|pair| pair.case_tags.iter().any(|tag| tag.starts_with("variant-")))
            .expect("variant pair exists")
            .clone();
        dataset.pair_labels.extend((0..200).map(|index| {
            let mut pair = repeated.clone();
            pair.pair_id = format!("repeated-oversized-pair-{index}");
            pair
        }));

        let rows: Vec<_> = dataset
            .records
            .iter()
            .filter(|record| record.split == "held_out_test")
            .collect();
        let queries: BTreeMap<_, _> = dataset
            .queries
            .iter()
            .filter(|query| query.split == "held_out_test")
            .map(|query| (query.query_record_id.as_str(), query))
            .collect();
        let error =
            match run_algorithm_variant_experiment(&dataset, &rows, &queries, "held_out_test") {
                Err(error) => error,
                Ok(_) => {
                    panic!("repeated labeled pairs must exceed aggregate distance-work budget")
                }
            };
        assert!(error.contains("aggregate distance cells"), "{error}");
    }

    #[test]
    fn data_entry_fixture_covers_each_stratum_and_reports_subgroup_workload() {
        let dataset: Dataset = serde_json::from_str(include_str!(
            "../../fixtures/matching/data-entry-scenarios.json"
        ))
        .expect("data-entry fixture parses");
        let rows: Vec<_> = dataset
            .records
            .iter()
            .filter(|record| record.split == "held_out_test")
            .collect();
        let queries: BTreeMap<_, _> = dataset
            .queries
            .iter()
            .filter(|query| query.split == "held_out_test")
            .map(|query| (query.query_record_id.as_str(), query))
            .collect();
        let scenarios: BTreeSet<_> = dataset
            .queries
            .iter()
            .flat_map(|query| query.case_tags.iter().cloned())
            .collect();
        assert_eq!(dataset.queries.len(), 64);
        assert_eq!(
            scenarios,
            [
                "variant-deletion",
                "variant-insertion",
                "variant-mark-loss",
                "variant-missing-date",
                "variant-spacing-punctuation",
                "variant-substitution",
                "variant-token-order",
                "variant-transposition",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect()
        );
        let report = run_candidate_experiment(&rows, &queries).expect("bounded scenario run");
        assert_eq!(report.baseline.positives, 64);
        assert_eq!(report.baseline.hard_negatives, 64);
        assert_eq!(report.baseline.truncated_queries, 0);
        assert_eq!(report.approximate_union.positives, 64);
        assert_eq!(report.approximate_union.hard_negatives, 64);
        assert_eq!(report.approximate_union.truncated_queries, 0);
        for metrics in [&report.baseline, &report.approximate_union] {
            assert!(metrics.positive_queries > 0);
            assert!(metrics.positive_queries_with_retrieved_match <= metrics.positive_queries);
            assert_eq!(
                metrics.positive_query_recall,
                Some(
                    metrics.positive_queries_with_retrieved_match as f64
                        / metrics.positive_queries as f64
                )
            );
        }
        assert_eq!(report.approximate_union.by_script.len(), 7);
        assert_eq!(report.approximate_union.by_language.len(), 8);
        for script in ["Laoo", "Thai", "Khmr", "Deva", "Arab", "Cyrl"] {
            let subgroup = &report.approximate_union.by_script[script];
            assert_eq!(subgroup.queries, 8);
            assert!(subgroup.positive_queries > 0);
            assert!(subgroup.positive_queries_with_retrieved_match <= subgroup.positive_queries);
            assert_eq!(
                subgroup.positive_query_recall,
                Some(
                    subgroup.positive_queries_with_retrieved_match as f64
                        / subgroup.positive_queries as f64
                )
            );
            assert_eq!(subgroup.positive_candidates_retrieved, 7);
            assert_eq!(subgroup.hard_negatives_retrieved, 8);
            assert_eq!(subgroup.candidate_count, 21);
            assert_eq!(subgroup.max_candidates_per_query, 4);
            assert_eq!(subgroup.truncated_queries, 0);
        }
        let latin = &report.approximate_union.by_script["Latn"];
        assert_eq!(latin.queries, 16);
        assert_eq!(latin.positive_candidates_retrieved, 14);
        assert_eq!(latin.hard_negatives_retrieved, 16);
        assert_eq!(latin.candidate_count, 44);
        for language in ["lo", "th", "km", "hi-IN", "ar", "ru"] {
            assert_eq!(
                report.approximate_union.by_language[language].candidate_count,
                21
            );
        }
        assert_eq!(
            report.approximate_union.by_language["en"].candidate_count,
            22
        );
        assert_eq!(
            report.approximate_union.by_language["vi"].candidate_count,
            22
        );
        assert_eq!(
            report.approximate_union.by_script_case["Laoo/variant-token-order"]
                .positive_candidates_retrieved,
            0
        );
        assert_eq!(
            report.approximate_union.by_script_case["Laoo/variant-token-order"].candidate_count,
            1
        );
        assert_eq!(
            report.approximate_union.by_language_case["vi/variant-mark-loss"]
                .positive_candidates_retrieved,
            1
        );
        assert_eq!(
            report.approximate_union.by_language_case["vi/variant-mark-loss"]
                .hard_negatives_retrieved,
            1
        );
        assert_eq!(
            report.approximate_union.by_language_case["vi/variant-mark-loss"]
                .max_candidates_per_query,
            3
        );
    }

    #[test]
    fn candidate_experiment_rejects_comparison_work_above_its_budget() {
        let error = validate_experiment_bounds(101, 1_000, &[]).unwrap_err();
        assert!(error.contains("100000 query-record comparisons"));
    }

    #[test]
    fn experiment_positive_query_recall_is_null_without_positive_queries() {
        let report = ExperimentCounts::default().finish(0, 0);
        assert_eq!(report.positive_queries, 0);
        assert_eq!(report.positive_queries_with_retrieved_match, 0);
        assert_eq!(report.positive_query_recall, None);
    }

    #[test]
    fn positive_query_coverage_is_distinct_from_pair_recall() {
        let dataset: Dataset =
            serde_json::from_str(include_str!("../../fixtures/matching/dataset.json"))
                .expect("synthetic matching fixture parses");
        let rows: Vec<_> = dataset
            .records
            .iter()
            .filter(|record| record.split == "held_out_test")
            .collect();
        let queries: BTreeMap<_, _> = dataset
            .queries
            .iter()
            .filter(|query| query.split == "held_out_test")
            .map(|query| (query.query_record_id.as_str(), query))
            .collect();
        let report = run_candidate_experiment(&rows, &queries).expect("bounded fixture run");

        // One gold-positive pair shares only a source_variant name, so it is not exact evidence.
        assert_eq!(report.baseline.positive_queries, 10);
        assert_eq!(report.baseline.positive_queries_with_retrieved_match, 9);
        assert_eq!(report.baseline.positive_query_recall, Some(0.9));
        assert_eq!(report.approximate_union.positive_queries, 10);
        assert_eq!(
            report
                .approximate_union
                .positive_queries_with_retrieved_match,
            9
        );
        assert_eq!(report.approximate_union.positive_query_recall, Some(0.9));
        assert_eq!(report.baseline.positives_retrieved, 16);
        assert_eq!(report.approximate_union.positives_retrieved, 16);
        assert_eq!(report.baseline.candidate_recall, Some(16.0 / 17.0));
        assert_eq!(report.approximate_union.candidate_recall, Some(16.0 / 17.0));
    }

    #[test]
    fn synthetic_fixture_has_all_target_scripts_and_cross_script_control_is_not_transliterated() {
        let dataset: Dataset =
            serde_json::from_str(include_str!("../../fixtures/matching/dataset.json"))
                .expect("synthetic matching fixture parses");
        let scripts: BTreeSet<_> = dataset
            .records
            .iter()
            .flat_map(|record| record.name_assertions.iter())
            .filter(|name| name.representation.as_deref() == Some("source_original"))
            .filter_map(|name| name.script.as_deref())
            .collect();
        for script in ["Laoo", "Thai", "Khmr", "Latn", "Deva", "Arab", "Cyrl"] {
            assert!(scripts.contains(script), "missing script control: {script}");
        }

        let query = dataset
            .queries
            .iter()
            .find(|query| query.query_id == "syn-query-016")
            .expect("no-transliteration query exists");
        let query_record = dataset
            .records
            .iter()
            .find(|record| record.record_id == query.query_record_id)
            .expect("query record exists");
        let cross_script_id = query.hard_negative_record_ids.first().unwrap();
        let cross_script_record = dataset
            .records
            .iter()
            .find(|record| &record.record_id == cross_script_id)
            .expect("cross-script control exists");
        let query_script = original_name_and_script(query_record).unwrap().1;
        let candidate_script = original_name_and_script(cross_script_record).unwrap().1;
        assert_ne!(query_script, candidate_script);
        assert!(
            !query_record
                .name_assertions
                .iter()
                .any(|name| { name.representation.as_deref() == Some("transliteration") })
        );
        assert!(
            !cross_script_record
                .name_assertions
                .iter()
                .any(|name| { name.representation.as_deref() == Some("transliteration") })
        );
        let query = Query {
            query_id: query.query_id.clone(),
            query_record_id: query.query_record_id.clone(),
            split: query.split.clone(),
            eligible_match_record_ids: query.eligible_match_record_ids.clone(),
            hard_negative_record_ids: query.hard_negative_record_ids.clone(),
            case_tags: query.case_tags.clone(),
        };
        let rows: Vec<_> = dataset
            .records
            .iter()
            .filter(|record| record.split == query.split)
            .collect();
        let approximate = approximate_name_candidates(&query, &rows).unwrap();
        assert!(!approximate.iter().any(|(id, _)| id == cross_script_id));
    }
}
