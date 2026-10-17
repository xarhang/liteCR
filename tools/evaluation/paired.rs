//! Offline, synthetic-only paired evaluation of candidate retrieval and mapped outcomes.
//!
//! This module intentionally does not load either matching implementation. It compares
//! two systems' already-recorded outputs against one mechanics-only fixture and never
//! treats a review suggestion as a link/merge action.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
};

const MISSING: &str = "(missing)";
const BOOTSTRAP_VERSION: &str = "paired-group-percentile-bootstrap-v1";
const SUBGROUP_BOOTSTRAP_VERSION: &str =
    "paired-group-subgroup-candidate-recall-percentile-bootstrap-v1";
const FALSE_ACTION_BOOTSTRAP_VERSION: &str = "paired-group-false-action-percentile-bootstrap-v1";
const BOOTSTRAP_REPLICATES: usize = 10_000;
const BOOTSTRAP_MIN_REPLICATES: usize = 1_000;
const BOOTSTRAP_MAX_GROUP_DRAWS: usize = 50_000_000;

#[derive(Debug, Default)]
struct NullableString {
    present: bool,
    value: Option<String>,
}

impl<'de> Deserialize<'de> for NullableString {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Option::<String>::deserialize(deserializer).map(|value| Self {
            present: true,
            value,
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Artifact {
    schema_version: String,
    benchmark_id: String,
    data_class: String,
    purpose: String,
    split: String,
    source_snapshot_digest: String,
    label_guide_version: String,
    partition_policy: String,
    records: Vec<Record>,
    queries: Vec<Query>,
    pairs: Vec<Pair>,
    systems: Vec<System>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    record_id: String,
    gold_cluster_id: String,
    partition_group_id: String,
    split: String,
    #[serde(default)]
    language_tag: NullableString,
    #[serde(default)]
    script: NullableString,
    #[serde(default)]
    source_bucket: NullableString,
    #[serde(default)]
    completeness_bucket: NullableString,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Query {
    query_id: String,
    anchor_record_id: String,
    split: String,
    independent_group_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pair {
    pair_id: String,
    query_id: String,
    candidate_record_id: String,
    split: String,
    label: String,
    label_status: String,
    #[serde(default)]
    case_tags: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct System {
    system_id: String,
    system_version: String,
    config_digest: String,
    mapping_version: String,
    query_results: Vec<QueryResult>,
    pair_results: Vec<PairResult>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct QueryResult {
    query_id: String,
    status: String,
    candidate_record_ids: Vec<String>,
    elapsed_microseconds: Option<u64>,
    truncated: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PairResult {
    pair_id: String,
    #[serde(default)]
    native_outcome: Option<String>,
    mapped_outcome: String,
    action: String,
}

#[derive(Debug, Serialize)]
struct Report {
    report_version: &'static str,
    benchmark_id: String,
    input_artifact_sha256: String,
    split: String,
    data_class: &'static str,
    purpose: &'static str,
    claim_boundary: &'static str,
    systems: BTreeMap<String, SystemReport>,
    paired_comparison: PairedComparison,
    limitations: Vec<&'static str>,
}

#[derive(Debug, Serialize, Default)]
struct SystemReport {
    queries: usize,
    match_pairs: usize,
    retrieved_match_pairs: usize,
    candidate_recall: Option<f64>,
    labeled_non_match_pairs: usize,
    retrieved_labeled_non_match_pairs: usize,
    sampled_labeled_negative_retrieval_burden: Option<f64>,
    workload: Workload,
    decisions: DecisionMetrics,
    proposed_clusters: ClusterMetrics,
    subgroups: Subgroups,
}

/// Metrics over the full declared record universe. Every explicit `link` or
/// `merge` action is one undirected proposed identity edge; unlinked records
/// remain singleton predicted clusters.
#[derive(Debug, Serialize, Default)]
struct ClusterMetrics {
    record_universe: usize,
    proposed_link_or_merge_edges: usize,
    predicted_clusters: usize,
    gold_clusters: usize,
    pairwise_true_positive_pairs: usize,
    pairwise_false_positive_pairs: usize,
    pairwise_false_negative_pairs: usize,
    pairwise_precision: Option<f64>,
    pairwise_recall: Option<f64>,
    pairwise_f1: Option<f64>,
    bcubed_precision: Option<f64>,
    bcubed_recall: Option<f64>,
    overmerged_predicted_components: usize,
    fragmented_gold_clusters: usize,
}

#[derive(Debug, Serialize, Default)]
struct Workload {
    complete_queries: usize,
    failed_queries: usize,
    unavailable_queries: usize,
    truncated_queries: usize,
    total_candidates: usize,
    mean_candidates_per_query: Option<f64>,
    median_candidates_per_query: Option<f64>,
    p95_candidates_per_query: Option<f64>,
    elapsed_microseconds_total: Option<u64>,
    elapsed_microseconds_missing: usize,
    elapsed_microseconds_sum_overflow: bool,
}

#[derive(Debug, Serialize, Default)]
struct DecisionMetrics {
    labeled_decisions: usize,
    true_same_person_suggestions: usize,
    false_match_suggestions: usize,
    true_different_person_suggestions: usize,
    false_non_match_suggestions: usize,
    review_outcomes: usize,
    not_evaluated_outcomes: usize,
    unavailable_outcomes: usize,
    uncertain_label_outcomes: usize,
    false_link_or_merge_actions_on_non_match: usize,
    actions_on_uncertain_labels: usize,
    actions_on_unavailable_or_unscored: usize,
}

#[derive(Debug, Serialize, Default)]
struct Subgroups {
    by_locale: BTreeMap<String, SubgroupCount>,
    by_script: BTreeMap<String, SubgroupCount>,
    by_source_bucket: BTreeMap<String, SubgroupCount>,
    by_completeness_bucket: BTreeMap<String, SubgroupCount>,
    by_case_tag: BTreeMap<String, SubgroupCount>,
}

#[derive(Debug, Serialize, Default)]
struct SubgroupCount {
    pairs: usize,
    match_pairs: usize,
    retrieved_match_pairs: usize,
    labeled_non_match_pairs: usize,
    retrieved_labeled_non_match_pairs: usize,
    uncertain_pairs: usize,
    review_outcomes: usize,
    false_match_suggestions: usize,
    false_link_or_merge_actions: usize,
}

#[derive(Debug, Serialize, Default)]
struct PairedComparison {
    queries: usize,
    pairs: usize,
    both_query_runs_complete: usize,
    candidate_set_comparisons_skipped_incomplete: usize,
    same_candidate_set_queries: usize,
    differing_candidate_set_queries: usize,
    same_mapped_outcome_pairs: usize,
    differing_mapped_outcome_pairs: usize,
    same_action_pairs: usize,
    differing_action_pairs: usize,
    candidate_recall_difference_bootstrap: CandidateRecallBootstrap,
    candidate_recall_subgroup_bootstrap: CandidateRecallSubgroups,
    false_action_rate_difference_bootstrap: FalseActionRateBootstrap,
}

#[derive(Debug, Serialize, Default)]
struct CandidateRecallSubgroups {
    by_locale: BTreeMap<String, CandidateRecallBootstrap>,
    by_script: BTreeMap<String, CandidateRecallBootstrap>,
}

#[derive(Debug, Serialize, Default)]
struct CandidateRecallBootstrap {
    status: &'static str,
    estimator: &'static str,
    method: &'static str,
    resampling_unit: &'static str,
    interval_method: &'static str,
    seed_derivation: &'static str,
    random_generator: &'static str,
    confidence_level: f64,
    opencr_retrieved_match_pairs: usize,
    opencr_match_pairs: usize,
    opencr_candidate_recall: Option<f64>,
    litecr_retrieved_match_pairs: usize,
    litecr_match_pairs: usize,
    litecr_candidate_recall: Option<f64>,
    point_difference_litecr_minus_opencr: Option<f64>,
    percentile_95_ci_lower: Option<f64>,
    percentile_95_ci_upper: Option<f64>,
    eligible_independent_groups: usize,
    zero_positive_query_groups_excluded: usize,
    positive_pairs: usize,
    replicates_requested: usize,
    replicates_used: usize,
    max_group_draws: usize,
    work_budget_scope: &'static str,
    seed_u64: Option<u64>,
}

#[derive(Debug, Serialize, Default)]
struct FalseActionRateBootstrap {
    status: &'static str,
    estimator: &'static str,
    method: &'static str,
    resampling_unit: &'static str,
    interval_method: &'static str,
    seed_derivation: &'static str,
    random_generator: &'static str,
    confidence_level: f64,
    opencr_false_link_or_merge_actions: usize,
    opencr_labeled_non_match_pairs: usize,
    opencr_false_action_rate: Option<f64>,
    litecr_false_link_or_merge_actions: usize,
    litecr_labeled_non_match_pairs: usize,
    litecr_false_action_rate: Option<f64>,
    point_difference_litecr_minus_opencr: Option<f64>,
    percentile_95_ci_lower: Option<f64>,
    percentile_95_ci_upper: Option<f64>,
    eligible_independent_groups: usize,
    zero_non_match_query_groups_excluded: usize,
    replicates_requested: usize,
    replicates_used: usize,
    max_group_draws: usize,
    seed_u64: Option<u64>,
}

/// Validates and compares one paired evaluation artifact, returning only redacted counts.
pub fn compare_json(bytes: &[u8]) -> Result<String, String> {
    let artifact: Artifact = serde_json::from_slice(bytes)
        .map_err(|_| "invalid paired artifact JSON or unsupported fields".to_owned())?;
    validate(&artifact)?;
    let query_by_id: BTreeMap<&str, &Query> = artifact
        .queries
        .iter()
        .map(|q| (q.query_id.as_str(), q))
        .collect();
    let record_by_id: BTreeMap<&str, &Record> = artifact
        .records
        .iter()
        .map(|r| (r.record_id.as_str(), r))
        .collect();
    let mut reports = BTreeMap::new();
    for system in &artifact.systems {
        reports.insert(
            system.system_id.clone(),
            score_system(system, &artifact, &query_by_id, &record_by_id),
        );
    }
    let paired_comparison = compare_systems(&artifact, bytes);
    let report = Report {
        report_version: "litecr-paired-evaluation-v5",
        benchmark_id: artifact.benchmark_id,
        input_artifact_sha256: format!("sha256:{}", hex_digest(bytes)),
        split: artifact.split,
        data_class: "synthetic_fixture",
        purpose: "mechanics_only",
        claim_boundary: "synthetic mechanics only; no OpenCR or real-world accuracy, superiority, safety, or threshold claim",
        systems: reports,
        paired_comparison,
        limitations: vec![
            "This report validates evaluator mechanics only and is not a production evaluation.",
            "The input artifact SHA-256 identifies the exact JSON bytes; it is an integrity aid, not anonymization.",
            "Labeled-negative retrieval burden is sampled over supplied labeled pairs and does not estimate negative classification performance.",
            "Candidate retrieval, mapped review suggestions, and link/merge actions are separate quantities.",
            "The paired bootstrap resamples positive-bearing identity-connected groups and quantifies mechanics on the supplied synthetic positive pairs only; it does not establish real-world matching accuracy.",
            "Locale/script subgroup intervals resample the same independent identity-connected groups for both systems; sparse synthetic subgroup results do not establish language-specific accuracy or fairness.",
            "The false link/merge action interval resamples labeled non-match identity-connected groups and describes supplied synthetic actions only; it does not establish a real-world error rate or acceptable risk.",
            "No thresholds are selected or recommended by this report.",
        ],
    };
    serde_json::to_string_pretty(&report)
        .map_err(|_| "could not serialize paired report".to_owned())
}

fn hex_digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(64);
    for byte in digest {
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

fn validate(a: &Artifact) -> Result<(), String> {
    if a.schema_version != "1.0"
        || a.data_class != "synthetic_fixture"
        || a.purpose != "mechanics_only"
    {
        return Err("paired evaluator accepts only schema 1.0 synthetic mechanics fixtures".into());
    }
    if !valid_token(&a.split)
        || !synthetic_token(&a.benchmark_id, "synthetic-control-")
        || !synthetic_token(&a.label_guide_version, "synthetic-guide-")
        || a.partition_policy != "identity_connected_v1"
    {
        return Err("invalid artifact metadata token".into());
    }
    if !valid_digest(&a.source_snapshot_digest) {
        return Err("invalid source snapshot digest".into());
    }
    if a.split != "held_out_test" {
        return Err("paired benchmark must use the held_out_test split".into());
    }
    if a.records.is_empty() || a.queries.is_empty() || a.pairs.is_empty() {
        return Err("records, queries, and pairs must be non-empty".into());
    }
    let mut records = BTreeMap::new();
    let mut clusters: BTreeMap<&str, &str> = BTreeMap::new();
    let mut groups: BTreeMap<&str, &str> = BTreeMap::new();
    let mut cluster_groups: BTreeMap<&str, &str> = BTreeMap::new();
    for r in &a.records {
        if !fixture_id(&r.record_id)
            || !fixture_id(&r.gold_cluster_id)
            || !fixture_id(&r.partition_group_id)
            || r.split != a.split
        {
            return Err("invalid record identity or split".into());
        }
        if records.insert(r.record_id.as_str(), r).is_some() {
            return Err("duplicate record id".into());
        }
        check_single_split(
            &mut clusters,
            &r.gold_cluster_id,
            &r.split,
            "gold identity cluster",
        )?;
        check_single_split(
            &mut groups,
            &r.partition_group_id,
            &r.split,
            "partition group",
        )?;
        if let Some(previous_group) =
            cluster_groups.insert(&r.gold_cluster_id, &r.partition_group_id)
        {
            if previous_group != r.partition_group_id {
                return Err(
                    "one gold identity cluster spans multiple independent partition groups".into(),
                );
            }
        }
        validate_optional_category(r.language_tag.value.as_deref(), Category::Locale)?;
        validate_optional_category(r.script.value.as_deref(), Category::Script)?;
        validate_optional_category(r.source_bucket.value.as_deref(), Category::Source)?;
        validate_optional_category(
            r.completeness_bucket.value.as_deref(),
            Category::Completeness,
        )?;
        if !r.language_tag.present
            || !r.script.present
            || !r.source_bucket.present
            || !r.completeness_bucket.present
        {
            return Err(
                "record subgroup properties must be present, using null when missing".into(),
            );
        }
    }
    let mut queries = BTreeMap::new();
    let mut anchors = BTreeSet::new();
    for q in &a.queries {
        if !fixture_id(&q.query_id)
            || !fixture_id(&q.anchor_record_id)
            || !fixture_id(&q.independent_group_id)
            || q.split != a.split
        {
            return Err("invalid query identity or split".into());
        }
        let anchor = records
            .get(q.anchor_record_id.as_str())
            .ok_or("query anchor record is missing")?;
        if anchor.partition_group_id != q.independent_group_id {
            return Err("query independent group does not match anchor record".into());
        }
        if queries.insert(q.query_id.as_str(), q).is_some()
            || !anchors.insert(q.anchor_record_id.as_str())
        {
            return Err("duplicate query id or anchor".into());
        }
    }
    let mut pairs = BTreeMap::new();
    let mut query_candidates: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for p in &a.pairs {
        if !fixture_id(&p.pair_id)
            || !fixture_id(&p.query_id)
            || !fixture_id(&p.candidate_record_id)
            || p.split != a.split
        {
            return Err("invalid pair identity or split".into());
        }
        if !queries.contains_key(p.query_id.as_str()) {
            return Err("pair query reference is missing".into());
        }
        let q = queries[p.query_id.as_str()];
        let anchor = records
            .get(q.anchor_record_id.as_str())
            .ok_or("query anchor record is missing")?;
        let candidate = records
            .get(p.candidate_record_id.as_str())
            .ok_or("pair candidate reference is missing")?;
        if p.candidate_record_id == q.anchor_record_id {
            return Err("pair candidate reference is missing or self-referential".into());
        }
        if !query_candidates
            .entry(p.query_id.as_str())
            .or_default()
            .insert(p.candidate_record_id.as_str())
        {
            return Err("query repeats a candidate in labeled pairs".into());
        }
        if !matches!(p.label.as_str(), "match" | "non_match" | "uncertain") {
            return Err("invalid pair label".into());
        }
        if (p.label == "match" && anchor.gold_cluster_id != candidate.gold_cluster_id)
            || (p.label == "non_match" && anchor.gold_cluster_id == candidate.gold_cluster_id)
        {
            return Err("pair label conflicts with synthetic gold clusters".into());
        }
        if p.label_status != "synthetic_asserted" {
            return Err("synthetic fixtures require synthetic_asserted pair labels".into());
        }
        let case_tag_set: BTreeSet<_> = p.case_tags.iter().collect();
        if case_tag_set.len() != p.case_tags.len()
            || p.case_tags.iter().any(|tag| !allowed_case_tag(tag))
        {
            return Err("pair contains duplicate or non-allowlisted case tags".into());
        }
        if pairs.insert(p.pair_id.as_str(), p).is_some() {
            return Err("duplicate pair id".into());
        }
    }
    if pairs.is_empty() {
        return Err("pairs must be non-empty".into());
    }
    let ids: BTreeSet<_> = a.systems.iter().map(|s| s.system_id.as_str()).collect();
    if a.systems.len() != 2 || ids != BTreeSet::from(["opencr", "litecr"]) {
        return Err("exactly opencr and litecr systems are required".into());
    }
    for s in &a.systems {
        if !s.system_version.starts_with("synthetic-")
            || !s.mapping_version.starts_with("synthetic-map-")
            || !valid_digest(&s.config_digest)
        {
            return Err("invalid system version, mapping version, or config digest".into());
        }
        if s.query_results.len() != queries.len() || s.pair_results.len() != pairs.len() {
            return Err("system result coverage is incomplete or contains duplicates".into());
        }
        let mut seen_q = BTreeSet::new();
        for result in &s.query_results {
            if !queries.contains_key(result.query_id.as_str())
                || !seen_q.insert(result.query_id.as_str())
            {
                return Err("query result coverage has unknown or duplicate ids".into());
            }
            if !matches!(
                result.status.as_str(),
                "complete" | "failed" | "unavailable"
            ) {
                return Err("invalid query result status".into());
            }
            let mut candidate_ids = BTreeSet::new();
            let anchor = queries[result.query_id.as_str()].anchor_record_id.as_str();
            for id in &result.candidate_record_ids {
                if id == anchor
                    || !records.contains_key(id.as_str())
                    || !candidate_ids.insert(id.as_str())
                {
                    return Err("candidate list contains self, unknown, or duplicate ids".into());
                }
            }
            if result.status != "complete" && !result.candidate_record_ids.is_empty() {
                return Err("failed or unavailable query results must not claim candidates".into());
            }
        }
        let mut seen_p = BTreeSet::new();
        for result in &s.pair_results {
            if !pairs.contains_key(result.pair_id.as_str())
                || !seen_p.insert(result.pair_id.as_str())
            {
                return Err("pair result coverage has unknown or duplicate ids".into());
            }
            if !matches!(
                result.mapped_outcome.as_str(),
                "same_person" | "different_person" | "review" | "not_evaluated" | "unavailable"
            ) {
                return Err("invalid mapped outcome".into());
            }
            if !matches!(
                result.action.as_str(),
                "none" | "link" | "merge" | "not_applicable"
            ) {
                return Err("invalid action".into());
            }
            if result
                .native_outcome
                .as_ref()
                .is_some_and(|outcome| outcome.len() > 128)
            {
                return Err("native outcome exceeds the bounded input size".into());
            }
        }
    }
    Ok(())
}

fn score_system(
    s: &System,
    a: &Artifact,
    queries: &BTreeMap<&str, &Query>,
    records: &BTreeMap<&str, &Record>,
) -> SystemReport {
    let qr: BTreeMap<_, _> = s
        .query_results
        .iter()
        .map(|x| (x.query_id.as_str(), x))
        .collect();
    let pr: BTreeMap<_, _> = s
        .pair_results
        .iter()
        .map(|x| (x.pair_id.as_str(), x))
        .collect();
    let mut report = SystemReport {
        queries: queries.len(),
        proposed_clusters: proposed_cluster_metrics(s, a, queries),
        ..Default::default()
    };
    let mut counts = Vec::new();
    let mut elapsed_total = Some(0u64);
    let mut elapsed_sum_overflow = false;
    for q in a.queries.iter() {
        let res = qr[q.query_id.as_str()];
        match res.status.as_str() {
            "complete" => report.workload.complete_queries += 1,
            "failed" => report.workload.failed_queries += 1,
            _ => report.workload.unavailable_queries += 1,
        }
        report.workload.truncated_queries += usize::from(res.truncated);
        report.workload.total_candidates += res.candidate_record_ids.len();
        counts.push(res.candidate_record_ids.len());
        if let Some(us) = res.elapsed_microseconds {
            if let Some(total) = elapsed_total {
                match total.checked_add(us) {
                    Some(sum) => elapsed_total = Some(sum),
                    None => {
                        elapsed_total = None;
                        elapsed_sum_overflow = true;
                    }
                }
            }
        } else {
            report.workload.elapsed_microseconds_missing += 1;
            elapsed_total = None;
        }
    }
    counts.sort_unstable();
    if !counts.is_empty() {
        report.workload.mean_candidates_per_query =
            Some(report.workload.total_candidates as f64 / counts.len() as f64);
        report.workload.median_candidates_per_query = Some(median(&counts));
        report.workload.p95_candidates_per_query = Some(percentile95(&counts));
    }
    report.workload.elapsed_microseconds_total = elapsed_total;
    report.workload.elapsed_microseconds_sum_overflow = elapsed_sum_overflow;
    for p in a.pairs.iter() {
        let q = queries[p.query_id.as_str()];
        let anchor = records[q.anchor_record_id.as_str()];
        let candidate_query_result = qr[p.query_id.as_str()];
        let retrieved = candidate_query_result.status == "complete"
            && candidate_query_result
                .candidate_record_ids
                .iter()
                .any(|id| id == &p.candidate_record_id);
        let mapped = pr[p.pair_id.as_str()];
        let group_keys = subgroup_keys(anchor, p);
        let mut is_false_match = false;
        let mut is_false_action = false;
        match p.label.as_str() {
            "match" => {
                report.match_pairs += 1;
                report.retrieved_match_pairs += usize::from(retrieved);
            }
            "non_match" => {
                report.labeled_non_match_pairs += 1;
                report.retrieved_labeled_non_match_pairs += usize::from(retrieved);
            }
            _ => report.decisions.uncertain_label_outcomes += 1,
        }
        match (p.label.as_str(), mapped.mapped_outcome.as_str()) {
            ("match", "same_person") => {
                report.decisions.labeled_decisions += 1;
                report.decisions.true_same_person_suggestions += 1;
            }
            ("match", "different_person") => {
                report.decisions.labeled_decisions += 1;
                report.decisions.false_non_match_suggestions += 1;
            }
            ("non_match", "same_person") => {
                report.decisions.labeled_decisions += 1;
                report.decisions.false_match_suggestions += 1;
                is_false_match = true;
            }
            ("non_match", "different_person") => {
                report.decisions.labeled_decisions += 1;
                report.decisions.true_different_person_suggestions += 1;
            }
            (_, "review") => report.decisions.review_outcomes += 1,
            (_, "not_evaluated") => report.decisions.not_evaluated_outcomes += 1,
            (_, "unavailable") => report.decisions.unavailable_outcomes += 1,
            _ => {}
        }
        let action_taken = matches!(mapped.action.as_str(), "link" | "merge");
        if p.label == "non_match" && action_taken {
            report.decisions.false_link_or_merge_actions_on_non_match += 1;
            is_false_action = true;
        }
        if p.label == "uncertain" && action_taken {
            report.decisions.actions_on_uncertain_labels += 1;
        }
        if matches!(
            mapped.mapped_outcome.as_str(),
            "not_evaluated" | "unavailable"
        ) && action_taken
        {
            report.decisions.actions_on_unavailable_or_unscored += 1;
        }
        for (dimension, key) in group_keys {
            let bucket = subgroup_map_mut(&mut report.subgroups, dimension)
                .entry(key)
                .or_default();
            bucket.pairs += 1;
            match p.label.as_str() {
                "match" => {
                    bucket.match_pairs += 1;
                    bucket.retrieved_match_pairs += usize::from(retrieved);
                }
                "non_match" => {
                    bucket.labeled_non_match_pairs += 1;
                    bucket.retrieved_labeled_non_match_pairs += usize::from(retrieved);
                }
                _ => bucket.uncertain_pairs += 1,
            }
            bucket.review_outcomes += usize::from(mapped.mapped_outcome == "review");
            bucket.false_match_suggestions += usize::from(is_false_match);
            bucket.false_link_or_merge_actions += usize::from(is_false_action);
        }
    }
    report.candidate_recall = rate(report.retrieved_match_pairs, report.match_pairs);
    report.sampled_labeled_negative_retrieval_burden = rate(
        report.retrieved_labeled_non_match_pairs,
        report.labeled_non_match_pairs,
    );
    report
}

fn proposed_cluster_metrics(
    system: &System,
    artifact: &Artifact,
    queries: &BTreeMap<&str, &Query>,
) -> ClusterMetrics {
    let ids: BTreeMap<_, _> = artifact
        .records
        .iter()
        .enumerate()
        .map(|(index, record)| (record.record_id.as_str(), index))
        .collect();
    let query_results: BTreeMap<_, _> = system
        .pair_results
        .iter()
        .map(|result| (result.pair_id.as_str(), result))
        .collect();
    let mut parent: Vec<usize> = (0..artifact.records.len()).collect();
    let mut rank = vec![0u8; parent.len()];
    let mut edge_count = 0;
    for pair in &artifact.pairs {
        let result = query_results[pair.pair_id.as_str()];
        if matches!(result.action.as_str(), "link" | "merge") {
            let query = queries[pair.query_id.as_str()];
            let left = ids[query.anchor_record_id.as_str()];
            let right = ids[pair.candidate_record_id.as_str()];
            union(&mut parent, &mut rank, left, right);
            edge_count += 1;
        }
    }

    let mut predicted: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    let mut gold: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (index, record) in artifact.records.iter().enumerate() {
        let root = find(&mut parent, index);
        predicted.entry(root).or_default().push(index);
        gold.entry(record.gold_cluster_id.as_str())
            .or_default()
            .push(index);
    }
    let mut intersections: BTreeMap<(usize, &str), usize> = BTreeMap::new();
    for (index, record) in artifact.records.iter().enumerate() {
        *intersections
            .entry((find(&mut parent, index), record.gold_cluster_id.as_str()))
            .or_default() += 1;
    }

    let choose_two = |n: usize| n.saturating_mul(n.saturating_sub(1)) / 2;
    let gold_pairs: usize = gold.values().map(|members| choose_two(members.len())).sum();
    let predicted_pairs: usize = predicted
        .values()
        .map(|members| choose_two(members.len()))
        .sum();
    let true_positive_pairs: usize = intersections.values().map(|count| choose_two(*count)).sum();
    let false_positive_pairs = predicted_pairs.saturating_sub(true_positive_pairs);
    let false_negative_pairs = gold_pairs.saturating_sub(true_positive_pairs);
    let pairwise_precision = rate(true_positive_pairs, predicted_pairs);
    let pairwise_recall = rate(true_positive_pairs, gold_pairs);
    let pairwise_f1 = pairwise_precision.zip(pairwise_recall).map(|(p, r)| {
        if p + r == 0.0 {
            0.0
        } else {
            2.0 * p * r / (p + r)
        }
    });

    let mut bcubed_precision_sum = 0.0;
    let mut bcubed_recall_sum = 0.0;
    for (index, record) in artifact.records.iter().enumerate() {
        let predicted_size = predicted[&find(&mut parent, index)].len();
        let gold_size = gold[record.gold_cluster_id.as_str()].len();
        let overlap = intersections[&(find(&mut parent, index), record.gold_cluster_id.as_str())];
        bcubed_precision_sum += overlap as f64 / predicted_size as f64;
        bcubed_recall_sum += overlap as f64 / gold_size as f64;
    }
    let count = artifact.records.len() as f64;
    let overmerged_predicted_components = predicted
        .values()
        .filter(|members| {
            members
                .iter()
                .map(|index| artifact.records[*index].gold_cluster_id.as_str())
                .collect::<BTreeSet<_>>()
                .len()
                > 1
        })
        .count();
    let fragmented_gold_clusters = gold
        .values()
        .filter(|members| {
            members
                .iter()
                .map(|index| find(&mut parent, *index))
                .collect::<BTreeSet<_>>()
                .len()
                > 1
        })
        .count();

    ClusterMetrics {
        record_universe: artifact.records.len(),
        proposed_link_or_merge_edges: edge_count,
        predicted_clusters: predicted.len(),
        gold_clusters: gold.len(),
        pairwise_true_positive_pairs: true_positive_pairs,
        pairwise_false_positive_pairs: false_positive_pairs,
        pairwise_false_negative_pairs: false_negative_pairs,
        pairwise_precision,
        pairwise_recall,
        pairwise_f1,
        bcubed_precision: Some(bcubed_precision_sum / count),
        bcubed_recall: Some(bcubed_recall_sum / count),
        overmerged_predicted_components,
        fragmented_gold_clusters,
    }
}

fn find(parent: &mut [usize], item: usize) -> usize {
    if parent[item] != item {
        parent[item] = find(parent, parent[item]);
    }
    parent[item]
}

fn union(parent: &mut [usize], rank: &mut [u8], left: usize, right: usize) {
    let mut left_root = find(parent, left);
    let mut right_root = find(parent, right);
    if left_root == right_root {
        return;
    }
    if rank[left_root] < rank[right_root] {
        std::mem::swap(&mut left_root, &mut right_root);
    }
    parent[right_root] = left_root;
    if rank[left_root] == rank[right_root] {
        rank[left_root] = rank[left_root].saturating_add(1);
    }
}

#[derive(Default)]
struct GroupRecallCounts {
    positive_pairs: usize,
    open_retrieved: usize,
    litecr_retrieved: usize,
}

fn candidate_recall_bootstrap(a: &Artifact, artifact_bytes: &[u8]) -> CandidateRecallBootstrap {
    let mut groups: BTreeMap<&str, GroupRecallCounts> = BTreeMap::new();
    let query_by_id: BTreeMap<&str, &Query> = a
        .queries
        .iter()
        .map(|query| (query.query_id.as_str(), query))
        .collect();
    let open = a
        .systems
        .iter()
        .find(|system| system.system_id == "opencr")
        .unwrap();
    let lite = a
        .systems
        .iter()
        .find(|system| system.system_id == "litecr")
        .unwrap();
    let open_by_query: BTreeMap<_, _> = open
        .query_results
        .iter()
        .map(|result| (result.query_id.as_str(), result))
        .collect();
    let lite_by_query: BTreeMap<_, _> = lite
        .query_results
        .iter()
        .map(|result| (result.query_id.as_str(), result))
        .collect();

    for pair in a.pairs.iter().filter(|pair| pair.label == "match") {
        let query = query_by_id[pair.query_id.as_str()];
        let counts = groups
            .entry(query.independent_group_id.as_str())
            .or_default();
        counts.positive_pairs += 1;
        let open_result = open_by_query[pair.query_id.as_str()];
        counts.open_retrieved += usize::from(
            open_result.status == "complete"
                && open_result
                    .candidate_record_ids
                    .iter()
                    .any(|id| id == &pair.candidate_record_id),
        );
        let lite_result = lite_by_query[pair.query_id.as_str()];
        counts.litecr_retrieved += usize::from(
            lite_result.status == "complete"
                && lite_result
                    .candidate_record_ids
                    .iter()
                    .any(|id| id == &pair.candidate_record_id),
        );
    }

    let queried_groups: BTreeSet<_> = a
        .queries
        .iter()
        .map(|query| query.independent_group_id.as_str())
        .collect();
    let zero_positive_query_groups_excluded = queried_groups.len().saturating_sub(groups.len());
    let eligible_independent_groups = groups.len();
    let positive_pairs: usize = groups.values().map(|counts| counts.positive_pairs).sum();
    let open_retrieved: usize = groups.values().map(|counts| counts.open_retrieved).sum();
    let lite_retrieved: usize = groups.values().map(|counts| counts.litecr_retrieved).sum();
    let point_difference_litecr_minus_opencr = (positive_pairs > 0).then(|| {
        (lite_retrieved as f64 / positive_pairs as f64)
            - (open_retrieved as f64 / positive_pairs as f64)
    });
    let seed_u64 = bootstrap_seed(artifact_bytes);

    let mut output = CandidateRecallBootstrap {
        status: "insufficient_positive_bearing_groups",
        estimator: "pooled_candidate_recall_difference_litecr_minus_opencr",
        method: BOOTSTRAP_VERSION,
        resampling_unit: "positive_bearing_independent_identity_connected_group",
        interval_method: "nearest_rank_percentiles_2.5_and_97.5",
        seed_derivation: "sha256(method_version || 0x00 || exact_artifact_bytes), first_64_bits_little_endian",
        random_generator: "xorshift64star_v1_with_rejection_sampling_for_group_index",
        confidence_level: 0.95,
        opencr_retrieved_match_pairs: open_retrieved,
        opencr_match_pairs: positive_pairs,
        opencr_candidate_recall: rate(open_retrieved, positive_pairs),
        litecr_retrieved_match_pairs: lite_retrieved,
        litecr_match_pairs: positive_pairs,
        litecr_candidate_recall: rate(lite_retrieved, positive_pairs),
        point_difference_litecr_minus_opencr,
        percentile_95_ci_lower: None,
        percentile_95_ci_upper: None,
        eligible_independent_groups,
        zero_positive_query_groups_excluded,
        positive_pairs,
        replicates_requested: BOOTSTRAP_REPLICATES,
        replicates_used: 0,
        max_group_draws: BOOTSTRAP_MAX_GROUP_DRAWS,
        work_budget_scope: "this_interval",
        seed_u64: Some(seed_u64),
    };

    if eligible_independent_groups < 2 {
        return output;
    }
    let replicates = BOOTSTRAP_REPLICATES.min(
        BOOTSTRAP_MAX_GROUP_DRAWS
            .checked_div(eligible_independent_groups)
            .unwrap_or(0),
    );
    if replicates < BOOTSTRAP_MIN_REPLICATES {
        output.status = "bootstrap_work_limit_exceeded";
        return output;
    }

    let groups: Vec<_> = groups.into_values().collect();
    let group_count = groups.len();
    let mut rng = DeterministicRng::new(seed_u64);
    let mut differences = Vec::with_capacity(replicates);
    for _ in 0..replicates {
        let mut positive_total = 0.0;
        let mut open_total = 0.0;
        let mut lite_total = 0.0;
        for _ in 0..group_count {
            // The same sampled group is applied to both systems (paired bootstrap).
            let sampled = &groups[rng.index(group_count)];
            positive_total += sampled.positive_pairs as f64;
            open_total += sampled.open_retrieved as f64;
            lite_total += sampled.litecr_retrieved as f64;
        }
        differences.push((lite_total / positive_total) - (open_total / positive_total));
    }
    differences.sort_by(f64::total_cmp);
    output.status = "estimated_synthetic_only";
    output.replicates_used = replicates;
    output.percentile_95_ci_lower = Some(nearest_rank(&differences, 0.025));
    output.percentile_95_ci_upper = Some(nearest_rank(&differences, 0.975));
    output
}

fn candidate_recall_subgroup_bootstrap(
    a: &Artifact,
    artifact_bytes: &[u8],
) -> CandidateRecallSubgroups {
    let query_by_id: BTreeMap<&str, &Query> = a
        .queries
        .iter()
        .map(|query| (query.query_id.as_str(), query))
        .collect();
    let record_by_id: BTreeMap<&str, &Record> = a
        .records
        .iter()
        .map(|record| (record.record_id.as_str(), record))
        .collect();
    let open = a
        .systems
        .iter()
        .find(|system| system.system_id == "opencr")
        .unwrap();
    let lite = a
        .systems
        .iter()
        .find(|system| system.system_id == "litecr")
        .unwrap();
    let open_by_query: BTreeMap<_, _> = open
        .query_results
        .iter()
        .map(|result| (result.query_id.as_str(), result))
        .collect();
    let lite_by_query: BTreeMap<_, _> = lite
        .query_results
        .iter()
        .map(|result| (result.query_id.as_str(), result))
        .collect();

    let mut query_groups_by_locale: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut query_groups_by_script: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for query in &a.queries {
        let anchor = record_by_id[query.anchor_record_id.as_str()];
        let locale = anchor.language_tag.value.as_deref().unwrap_or(MISSING);
        let script = anchor.script.value.as_deref().unwrap_or(MISSING);
        query_groups_by_locale
            .entry(locale.to_owned())
            .or_default()
            .insert(query.independent_group_id.clone());
        query_groups_by_script
            .entry(script.to_owned())
            .or_default()
            .insert(query.independent_group_id.clone());
    }

    let mut groups_by_locale: BTreeMap<String, BTreeMap<String, GroupRecallCounts>> =
        BTreeMap::new();
    let mut groups_by_script: BTreeMap<String, BTreeMap<String, GroupRecallCounts>> =
        BTreeMap::new();
    for pair in a.pairs.iter().filter(|pair| pair.label == "match") {
        let query = query_by_id[pair.query_id.as_str()];
        let anchor = record_by_id[query.anchor_record_id.as_str()];
        let open_result = open_by_query[pair.query_id.as_str()];
        let lite_result = lite_by_query[pair.query_id.as_str()];
        let open_retrieved = usize::from(
            open_result.status == "complete"
                && open_result
                    .candidate_record_ids
                    .iter()
                    .any(|id| id == &pair.candidate_record_id),
        );
        let lite_retrieved = usize::from(
            lite_result.status == "complete"
                && lite_result
                    .candidate_record_ids
                    .iter()
                    .any(|id| id == &pair.candidate_record_id),
        );
        let group_id = query.independent_group_id.clone();
        for (groups, key) in [
            (
                &mut groups_by_locale,
                anchor.language_tag.value.as_deref().unwrap_or(MISSING),
            ),
            (
                &mut groups_by_script,
                anchor.script.value.as_deref().unwrap_or(MISSING),
            ),
        ] {
            let counts = groups
                .entry(key.to_owned())
                .or_default()
                .entry(group_id.clone())
                .or_default();
            counts.positive_pairs += 1;
            counts.open_retrieved += open_retrieved;
            counts.litecr_retrieved += lite_retrieved;
        }
    }

    let total_group_draws_per_replicate = groups_by_locale
        .values()
        .chain(groups_by_script.values())
        .try_fold(0usize, |total, groups| total.checked_add(groups.len()));
    let replicates_per_subgroup = total_group_draws_per_replicate
        .map(subgroup_bootstrap_replicates)
        .unwrap_or(0);
    let by_locale = build_subgroup_bootstraps(
        "locale",
        query_groups_by_locale,
        groups_by_locale,
        replicates_per_subgroup,
        artifact_bytes,
    );
    let by_script = build_subgroup_bootstraps(
        "script",
        query_groups_by_script,
        groups_by_script,
        replicates_per_subgroup,
        artifact_bytes,
    );
    CandidateRecallSubgroups {
        by_locale,
        by_script,
    }
}

fn build_subgroup_bootstraps(
    dimension: &'static str,
    query_groups: BTreeMap<String, BTreeSet<String>>,
    mut positive_groups: BTreeMap<String, BTreeMap<String, GroupRecallCounts>>,
    replicates_per_subgroup: usize,
    artifact_bytes: &[u8],
) -> BTreeMap<String, CandidateRecallBootstrap> {
    query_groups
        .into_iter()
        .map(|(category, queried_group_ids)| {
            let groups = positive_groups.remove(&category).unwrap_or_default();
            let bootstrap = candidate_recall_bootstrap_for_groups(
                groups,
                queried_group_ids.len(),
                replicates_per_subgroup,
                artifact_bytes,
                dimension,
                &category,
            );
            (category, bootstrap)
        })
        .collect()
}

fn candidate_recall_bootstrap_for_groups(
    groups: BTreeMap<String, GroupRecallCounts>,
    queried_group_count: usize,
    replicates: usize,
    artifact_bytes: &[u8],
    dimension: &'static str,
    category: &str,
) -> CandidateRecallBootstrap {
    let eligible_independent_groups = groups.len();
    let positive_pairs: usize = groups.values().map(|counts| counts.positive_pairs).sum();
    let open_retrieved: usize = groups.values().map(|counts| counts.open_retrieved).sum();
    let lite_retrieved: usize = groups.values().map(|counts| counts.litecr_retrieved).sum();
    let point_difference_litecr_minus_opencr = (positive_pairs > 0).then(|| {
        (lite_retrieved as f64 / positive_pairs as f64)
            - (open_retrieved as f64 / positive_pairs as f64)
    });
    let seed_u64 = subgroup_bootstrap_seed(artifact_bytes, dimension, category);
    let mut output = CandidateRecallBootstrap {
        status: "insufficient_positive_bearing_groups",
        estimator: "subgroup_candidate_recall_difference_litecr_minus_opencr",
        method: SUBGROUP_BOOTSTRAP_VERSION,
        resampling_unit: "positive_bearing_independent_identity_connected_group_within_subgroup",
        interval_method: "nearest_rank_percentiles_2.5_and_97.5",
        seed_derivation: "sha256(method_version || 0x00 || exact_artifact_bytes || 0x00 || dimension || 0x00 || category), first_64_bits_little_endian",
        random_generator: "xorshift64star_v1_with_rejection_sampling_for_group_index",
        confidence_level: 0.95,
        opencr_retrieved_match_pairs: open_retrieved,
        opencr_match_pairs: positive_pairs,
        opencr_candidate_recall: rate(open_retrieved, positive_pairs),
        litecr_retrieved_match_pairs: lite_retrieved,
        litecr_match_pairs: positive_pairs,
        litecr_candidate_recall: rate(lite_retrieved, positive_pairs),
        point_difference_litecr_minus_opencr,
        percentile_95_ci_lower: None,
        percentile_95_ci_upper: None,
        eligible_independent_groups,
        zero_positive_query_groups_excluded: queried_group_count
            .saturating_sub(eligible_independent_groups),
        positive_pairs,
        replicates_requested: BOOTSTRAP_REPLICATES,
        replicates_used: 0,
        max_group_draws: BOOTSTRAP_MAX_GROUP_DRAWS,
        work_budget_scope: "all_locale_and_script_subgroup_intervals_combined",
        seed_u64: Some(seed_u64),
    };

    if eligible_independent_groups < 2 {
        return output;
    }
    if replicates < BOOTSTRAP_MIN_REPLICATES {
        output.status = "bootstrap_work_limit_exceeded";
        return output;
    }

    let groups: Vec<_> = groups.into_values().collect();
    let group_count = groups.len();
    let mut rng = DeterministicRng::new(seed_u64);
    let mut differences = Vec::with_capacity(replicates);
    for _ in 0..replicates {
        let mut positive_total = 0.0;
        let mut open_total = 0.0;
        let mut lite_total = 0.0;
        for _ in 0..group_count {
            let sampled = &groups[rng.index(group_count)];
            positive_total += sampled.positive_pairs as f64;
            open_total += sampled.open_retrieved as f64;
            lite_total += sampled.litecr_retrieved as f64;
        }
        differences.push((lite_total / positive_total) - (open_total / positive_total));
    }
    differences.sort_by(f64::total_cmp);
    output.status = "estimated_synthetic_only";
    output.replicates_used = replicates;
    output.percentile_95_ci_lower = Some(nearest_rank(&differences, 0.025));
    output.percentile_95_ci_upper = Some(nearest_rank(&differences, 0.975));
    output
}

fn subgroup_bootstrap_replicates(total_group_draws_per_replicate: usize) -> usize {
    if total_group_draws_per_replicate == 0 {
        return BOOTSTRAP_REPLICATES;
    }
    BOOTSTRAP_REPLICATES.min(
        BOOTSTRAP_MAX_GROUP_DRAWS
            .checked_div(total_group_draws_per_replicate)
            .unwrap_or(0),
    )
}

fn subgroup_bootstrap_seed(artifact_bytes: &[u8], dimension: &str, category: &str) -> u64 {
    let mut hasher = Sha256::new();
    hasher.update(SUBGROUP_BOOTSTRAP_VERSION.as_bytes());
    hasher.update([0]);
    hasher.update(artifact_bytes);
    hasher.update([0]);
    hasher.update(dimension.as_bytes());
    hasher.update([0]);
    hasher.update(category.as_bytes());
    let digest = hasher.finalize();
    let mut seed = u64::from_le_bytes(
        digest[..8]
            .try_into()
            .expect("digest prefix is eight bytes"),
    );
    if seed == 0 {
        seed = 0x9e37_79b9_7f4a_7c15;
    }
    seed
}

fn bootstrap_seed(artifact_bytes: &[u8]) -> u64 {
    bootstrap_seed_for_version(artifact_bytes, BOOTSTRAP_VERSION)
}

fn bootstrap_seed_for_version(artifact_bytes: &[u8], method_version: &str) -> u64 {
    let mut hasher = Sha256::new();
    hasher.update(method_version.as_bytes());
    hasher.update([0]);
    hasher.update(artifact_bytes);
    let digest = hasher.finalize();
    let mut seed = u64::from_le_bytes(
        digest[..8]
            .try_into()
            .expect("digest prefix is eight bytes"),
    );
    if seed == 0 {
        seed = 0x9e37_79b9_7f4a_7c15;
    }
    seed
}

#[derive(Default)]
struct GroupFalseActionCounts {
    non_match_pairs: usize,
    opencr_false_actions: usize,
    litecr_false_actions: usize,
}

fn false_action_rate_bootstrap(a: &Artifact, artifact_bytes: &[u8]) -> FalseActionRateBootstrap {
    let query_by_id: BTreeMap<&str, &Query> = a
        .queries
        .iter()
        .map(|query| (query.query_id.as_str(), query))
        .collect();
    let open = a
        .systems
        .iter()
        .find(|system| system.system_id == "opencr")
        .unwrap();
    let lite = a
        .systems
        .iter()
        .find(|system| system.system_id == "litecr")
        .unwrap();
    let open_by_pair: BTreeMap<_, _> = open
        .pair_results
        .iter()
        .map(|result| (result.pair_id.as_str(), result))
        .collect();
    let lite_by_pair: BTreeMap<_, _> = lite
        .pair_results
        .iter()
        .map(|result| (result.pair_id.as_str(), result))
        .collect();
    let mut groups: BTreeMap<&str, GroupFalseActionCounts> = BTreeMap::new();

    for pair in a.pairs.iter().filter(|pair| pair.label == "non_match") {
        let query = query_by_id[pair.query_id.as_str()];
        let counts = groups
            .entry(query.independent_group_id.as_str())
            .or_default();
        counts.non_match_pairs += 1;
        counts.opencr_false_actions += usize::from(matches!(
            open_by_pair[pair.pair_id.as_str()].action.as_str(),
            "link" | "merge"
        ));
        counts.litecr_false_actions += usize::from(matches!(
            lite_by_pair[pair.pair_id.as_str()].action.as_str(),
            "link" | "merge"
        ));
    }

    let queried_groups: BTreeSet<_> = a
        .queries
        .iter()
        .map(|query| query.independent_group_id.as_str())
        .collect();
    let zero_non_match_query_groups_excluded = queried_groups.len().saturating_sub(groups.len());
    let eligible_independent_groups = groups.len();
    let opencr_false_link_or_merge_actions: usize = groups
        .values()
        .map(|counts| counts.opencr_false_actions)
        .sum();
    let litecr_false_link_or_merge_actions: usize = groups
        .values()
        .map(|counts| counts.litecr_false_actions)
        .sum();
    let opencr_labeled_non_match_pairs: usize =
        groups.values().map(|counts| counts.non_match_pairs).sum();
    let litecr_labeled_non_match_pairs = opencr_labeled_non_match_pairs;
    let opencr_false_action_rate = rate(
        opencr_false_link_or_merge_actions,
        opencr_labeled_non_match_pairs,
    );
    let litecr_false_action_rate = rate(
        litecr_false_link_or_merge_actions,
        litecr_labeled_non_match_pairs,
    );
    let point_difference_litecr_minus_opencr = opencr_false_action_rate
        .zip(litecr_false_action_rate)
        .map(|(open, lite)| lite - open);
    let seed_u64 = bootstrap_seed_for_version(artifact_bytes, FALSE_ACTION_BOOTSTRAP_VERSION);

    let mut output = FalseActionRateBootstrap {
        status: "insufficient_non_match_groups",
        estimator: "false_link_or_merge_actions_over_labeled_non_match_pairs",
        method: FALSE_ACTION_BOOTSTRAP_VERSION,
        resampling_unit: "labeled_non_match_independent_identity_connected_group",
        interval_method: "nearest_rank_percentiles_2.5_and_97.5",
        seed_derivation: "sha256(method_version || 0x00 || exact_artifact_bytes), first_64_bits_little_endian",
        random_generator: "xorshift64star_v1_with_rejection_sampling_for_group_index",
        confidence_level: 0.95,
        opencr_false_link_or_merge_actions,
        opencr_labeled_non_match_pairs,
        opencr_false_action_rate,
        litecr_false_link_or_merge_actions,
        litecr_labeled_non_match_pairs,
        litecr_false_action_rate,
        point_difference_litecr_minus_opencr,
        percentile_95_ci_lower: None,
        percentile_95_ci_upper: None,
        eligible_independent_groups,
        zero_non_match_query_groups_excluded,
        replicates_requested: BOOTSTRAP_REPLICATES,
        replicates_used: 0,
        max_group_draws: BOOTSTRAP_MAX_GROUP_DRAWS,
        seed_u64: Some(seed_u64),
    };

    let replicates = match false_action_bootstrap_replicates(eligible_independent_groups) {
        Ok(replicates) => replicates,
        Err(status) => {
            output.status = status;
            return output;
        }
    };

    let groups: Vec<_> = groups.into_values().collect();
    let group_count = groups.len();
    let mut rng = DeterministicRng::new(seed_u64);
    let mut differences = Vec::with_capacity(replicates);
    for _ in 0..replicates {
        let mut non_match_total = 0.0;
        let mut open_total = 0.0;
        let mut lite_total = 0.0;
        for _ in 0..group_count {
            let sampled = &groups[rng.index(group_count)];
            non_match_total += sampled.non_match_pairs as f64;
            open_total += sampled.opencr_false_actions as f64;
            lite_total += sampled.litecr_false_actions as f64;
        }
        differences.push((lite_total / non_match_total) - (open_total / non_match_total));
    }
    differences.sort_by(f64::total_cmp);
    output.status = "estimated_synthetic_only";
    output.replicates_used = replicates;
    output.percentile_95_ci_lower = Some(nearest_rank(&differences, 0.025));
    output.percentile_95_ci_upper = Some(nearest_rank(&differences, 0.975));
    output
}

fn false_action_bootstrap_replicates(
    eligible_independent_groups: usize,
) -> Result<usize, &'static str> {
    if eligible_independent_groups < 2 {
        return Err("insufficient_non_match_groups");
    }
    let replicates = BOOTSTRAP_REPLICATES.min(
        BOOTSTRAP_MAX_GROUP_DRAWS
            .checked_div(eligible_independent_groups)
            .unwrap_or(0),
    );
    if replicates < BOOTSTRAP_MIN_REPLICATES {
        return Err("bootstrap_work_limit_exceeded");
    }
    Ok(replicates)
}

fn nearest_rank(sorted: &[f64], probability: f64) -> f64 {
    let rank = (probability * sorted.len() as f64).ceil() as usize;
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

struct DeterministicRng(u64);

impl DeterministicRng {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u64(&mut self) -> u64 {
        // xorshift64*: stable, small, and used only for reproducible resampling.
        let mut value = self.0;
        value ^= value >> 12;
        value ^= value << 25;
        value ^= value >> 27;
        self.0 = value;
        value.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn index(&mut self, upper_exclusive: usize) -> usize {
        let upper = upper_exclusive as u64;
        let rejection_threshold = upper.wrapping_neg() % upper;
        loop {
            let value = self.next_u64();
            if value >= rejection_threshold {
                return (value % upper) as usize;
            }
        }
    }
}

fn compare_systems(a: &Artifact, artifact_bytes: &[u8]) -> PairedComparison {
    let open = a.systems.iter().find(|s| s.system_id == "opencr").unwrap();
    let lite = a.systems.iter().find(|s| s.system_id == "litecr").unwrap();
    let oq: BTreeMap<_, _> = open
        .query_results
        .iter()
        .map(|r| (r.query_id.as_str(), r))
        .collect();
    let lq: BTreeMap<_, _> = lite
        .query_results
        .iter()
        .map(|r| (r.query_id.as_str(), r))
        .collect();
    let op: BTreeMap<_, _> = open
        .pair_results
        .iter()
        .map(|r| (r.pair_id.as_str(), r))
        .collect();
    let lp: BTreeMap<_, _> = lite
        .pair_results
        .iter()
        .map(|r| (r.pair_id.as_str(), r))
        .collect();
    let mut out = PairedComparison {
        queries: a.queries.len(),
        pairs: a.pairs.len(),
        candidate_recall_difference_bootstrap: candidate_recall_bootstrap(a, artifact_bytes),
        candidate_recall_subgroup_bootstrap: candidate_recall_subgroup_bootstrap(a, artifact_bytes),
        false_action_rate_difference_bootstrap: false_action_rate_bootstrap(a, artifact_bytes),
        ..Default::default()
    };
    for q in &a.queries {
        let x = oq[q.query_id.as_str()];
        let y = lq[q.query_id.as_str()];
        if x.status == "complete" && y.status == "complete" {
            out.both_query_runs_complete += 1;
            let xs: BTreeSet<_> = x.candidate_record_ids.iter().collect();
            let ys: BTreeSet<_> = y.candidate_record_ids.iter().collect();
            if xs == ys {
                out.same_candidate_set_queries += 1;
            } else {
                out.differing_candidate_set_queries += 1;
            }
        } else {
            out.candidate_set_comparisons_skipped_incomplete += 1;
        }
    }
    for p in &a.pairs {
        let x = op[p.pair_id.as_str()];
        let y = lp[p.pair_id.as_str()];
        if x.mapped_outcome == y.mapped_outcome {
            out.same_mapped_outcome_pairs += 1;
        } else {
            out.differing_mapped_outcome_pairs += 1;
        }
        if x.action == y.action {
            out.same_action_pairs += 1;
        } else {
            out.differing_action_pairs += 1;
        }
    }
    out
}

#[derive(Clone, Copy)]
enum Category {
    Locale,
    Script,
    Source,
    Completeness,
}

fn validate_optional_category(value: Option<&str>, category: Category) -> Result<(), String> {
    if let Some(v) = value {
        let okay = match category {
            Category::Locale => allowed_locale(v),
            Category::Script => matches!(
                v,
                "Laoo"
                    | "Latn"
                    | "Thai"
                    | "Khmr"
                    | "Deva"
                    | "Arab"
                    | "Cyrl"
                    | "Mymr"
                    | "Hans"
                    | "Hant"
                    | "Beng"
                    | "Telu"
                    | "Taml"
                    | "Knda"
                    | "Guru"
                    | "Gujr"
                    | "Orya"
                    | "Sinh"
            ),
            Category::Source | Category::Completeness => fixture_category(v),
        };
        if !okay {
            return Err("artifact contains a non-allowlisted subgroup value".into());
        }
    }
    Ok(())
}

fn allowed_locale(v: &str) -> bool {
    matches!(
        v,
        "lo" | "en"
            | "th"
            | "vi"
            | "hi-IN"
            | "km"
            | "my"
            | "zh"
            | "zh-CN"
            | "zh-TW"
            | "id"
            | "ms"
            | "fr"
            | "es"
            | "ar"
            | "ru"
            | "bn-IN"
            | "ta-IN"
            | "te-IN"
            | "kn-IN"
            | "ml-IN"
            | "gu-IN"
            | "pa-IN"
            | "or-IN"
            | "si"
            | "ne"
            | "ur"
            | "ja"
            | "ko"
            | "pt"
            | "de"
            | "it"
            | "fil"
            | "km-KH"
            | "my-MM"
            | "th-TH"
            | "vi-VN"
            | "lo-LA"
            | "en-GB"
    )
}

fn allowed_case_tag(v: &str) -> bool {
    v.strip_prefix("synthetic-").is_some_and(|tail| {
        !tail.is_empty()
            && tail.len() <= 48
            && tail
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    })
}

fn fixture_category(v: &str) -> bool {
    v.starts_with("fixture-") && valid_token(v) && v.len() <= 64
}

fn subgroup_keys<'a>(record: &'a Record, pair: &'a Pair) -> Vec<(&'static str, String)> {
    let mut keys = vec![
        (
            "locale",
            record
                .language_tag
                .value
                .as_deref()
                .unwrap_or(MISSING)
                .to_owned(),
        ),
        (
            "script",
            record.script.value.as_deref().unwrap_or(MISSING).to_owned(),
        ),
        (
            "source",
            record
                .source_bucket
                .value
                .as_deref()
                .unwrap_or(MISSING)
                .to_owned(),
        ),
        (
            "completeness",
            record
                .completeness_bucket
                .value
                .as_deref()
                .unwrap_or(MISSING)
                .to_owned(),
        ),
    ];
    if pair.case_tags.is_empty() {
        keys.push(("case", MISSING.to_owned()));
    } else {
        keys.extend(pair.case_tags.iter().map(|tag| ("case", tag.clone())));
    }
    keys
}

fn subgroup_map_mut<'a>(
    groups: &'a mut Subgroups,
    dimension: &str,
) -> &'a mut BTreeMap<String, SubgroupCount> {
    match dimension {
        "locale" => &mut groups.by_locale,
        "script" => &mut groups.by_script,
        "source" => &mut groups.by_source_bucket,
        "completeness" => &mut groups.by_completeness_bucket,
        _ => &mut groups.by_case_tag,
    }
}

fn check_single_split<'a>(
    map: &mut BTreeMap<&'a str, &'a str>,
    key: &'a str,
    split: &'a str,
    label: &str,
) -> Result<(), String> {
    if let Some(previous) = map.insert(key, split) {
        if previous != split {
            return Err(format!("{label} crosses split boundaries"));
        }
    }
    Ok(())
}

fn valid_digest(s: &str) -> bool {
    s.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

fn valid_token(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
}

fn fixture_id(s: &str) -> bool {
    s.strip_prefix("fixture-").is_some_and(|tail| {
        !tail.is_empty()
            && tail.len() <= 60
            && tail
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    })
}

fn synthetic_token(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|tail| {
        !tail.is_empty()
            && tail.len() <= 48
            && tail
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
    })
}

fn rate(n: usize, d: usize) -> Option<f64> {
    (d > 0).then_some(n as f64 / d as f64)
}
fn median(sorted: &[usize]) -> f64 {
    let n = sorted.len();
    if n % 2 == 1 {
        sorted[n / 2] as f64
    } else {
        (sorted[n / 2 - 1] as f64 + sorted[n / 2] as f64) / 2.0
    }
}
fn percentile95(sorted: &[usize]) -> f64 {
    sorted[((95 * sorted.len()).div_ceil(100)).saturating_sub(1)] as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn fixture() -> Value {
        let digest = format!("sha256:{}", "a".repeat(64));
        json!({
          "schema_version":"1.0", "benchmark_id":"synthetic-control-paired-mechanics-v1", "data_class":"synthetic_fixture", "purpose":"mechanics_only", "split":"held_out_test",
          "source_snapshot_digest":digest, "label_guide_version":"synthetic-guide-v1", "partition_policy":"identity_connected_v1",
          "records":[
            {"record_id":"fixture-record-control-001","gold_cluster_id":"fixture-cluster-control-001","partition_group_id":"fixture-group-control-001","split":"held_out_test","language_tag":"lo-LA","script":"Laoo","source_bucket":"fixture-source-a","completeness_bucket":"fixture-complete"},
            {"record_id":"fixture-record-control-002","gold_cluster_id":"fixture-cluster-control-001","partition_group_id":"fixture-group-control-001","split":"held_out_test","language_tag":"lo-LA","script":"Laoo","source_bucket":"fixture-source-b","completeness_bucket":"fixture-partial"},
            {"record_id":"fixture-record-control-003","gold_cluster_id":"fixture-cluster-control-002","partition_group_id":"fixture-group-control-002","split":"held_out_test","language_tag":null,"script":null,"source_bucket":null,"completeness_bucket":null},
            {"record_id":"fixture-record-control-004","gold_cluster_id":"fixture-cluster-control-003","partition_group_id":"fixture-group-control-003","split":"held_out_test","language_tag":null,"script":null,"source_bucket":null,"completeness_bucket":null}],
          "queries":[{"query_id":"fixture-query-control-001","anchor_record_id":"fixture-record-control-001","split":"held_out_test","independent_group_id":"fixture-group-control-001"}],
          "pairs":[
            {"pair_id":"fixture-pair-control-001","query_id":"fixture-query-control-001","candidate_record_id":"fixture-record-control-002","split":"held_out_test","label":"match","label_status":"synthetic_asserted","case_tags":["synthetic-identity-positive"]},
            {"pair_id":"fixture-pair-control-002","query_id":"fixture-query-control-001","candidate_record_id":"fixture-record-control-003","split":"held_out_test","label":"non_match","label_status":"synthetic_asserted","case_tags":["synthetic-distinct-control"]},
            {"pair_id":"fixture-pair-control-003","query_id":"fixture-query-control-001","candidate_record_id":"fixture-record-control-004","split":"held_out_test","label":"uncertain","label_status":"synthetic_asserted","case_tags":[]}],
          "systems":[system("opencr", true),system("litecr", false)]
        })
    }
    fn system(id: &str, found: bool) -> Value {
        let cands = if found {
            vec![
                "fixture-record-control-002",
                "fixture-record-control-003",
                "fixture-record-control-004",
            ]
        } else {
            vec!["fixture-record-control-002"]
        };
        let outcome = if found { "same_person" } else { "review" };
        json!({"system_id":id,"system_version":"synthetic-control-0.0.1","config_digest":format!("sha256:{}", "b".repeat(64)),"mapping_version":"synthetic-map-v1",
          "query_results":[{"query_id":"fixture-query-control-001","status":"complete","candidate_record_ids":cands,"elapsed_microseconds":10,"truncated":false}],
          "pair_results":[
            {"pair_id":"fixture-pair-control-001","native_outcome":"native","mapped_outcome":outcome,"action":"none"},
            {"pair_id":"fixture-pair-control-002","native_outcome":null,"mapped_outcome":"different_person","action":"merge"},
            {"pair_id":"fixture-pair-control-003","native_outcome":null,"mapped_outcome":"unavailable","action":"none"}]})
    }
    fn parsed(v: Value) -> Result<String, String> {
        compare_json(v.to_string().as_bytes())
    }

    fn append_match_group(v: &mut Value, number: usize, open_found: bool, lite_found: bool) {
        append_match_group_with_metadata(v, number, open_found, lite_found, None, None);
    }

    fn append_match_group_with_metadata(
        v: &mut Value,
        number: usize,
        open_found: bool,
        lite_found: bool,
        language_tag: Option<&str>,
        script: Option<&str>,
    ) {
        let suffix = format!("{number:03}");
        let group_id = format!("fixture-group-control-{suffix}");
        let cluster_id = format!("fixture-cluster-control-{suffix}");
        let anchor_id = format!("fixture-record-control-{suffix}-a");
        let candidate_id = format!("fixture-record-control-{suffix}-b");
        let query_id = format!("fixture-query-control-{suffix}");
        let pair_id = format!("fixture-pair-control-{suffix}");
        for record_id in [&anchor_id, &candidate_id] {
            v["records"].as_array_mut().unwrap().push(json!({
                "record_id":record_id,
                "gold_cluster_id":cluster_id,
                "partition_group_id":group_id,
                "split":"held_out_test",
                "language_tag":language_tag,
                "script":script,
                "source_bucket":null,
                "completeness_bucket":null
            }));
        }
        v["queries"].as_array_mut().unwrap().push(json!({
            "query_id":query_id,
            "anchor_record_id":anchor_id,
            "split":"held_out_test",
            "independent_group_id":group_id
        }));
        v["pairs"].as_array_mut().unwrap().push(json!({
            "pair_id":pair_id,
            "query_id":query_id,
            "candidate_record_id":candidate_id,
            "split":"held_out_test",
            "label":"match",
            "label_status":"synthetic_asserted",
            "case_tags":["synthetic-identity-positive"]
        }));
        for (system_id, found) in [("opencr", open_found), ("litecr", lite_found)] {
            let system = v["systems"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|system| system["system_id"] == system_id)
                .unwrap();
            let candidates = if found {
                vec![candidate_id.as_str()]
            } else {
                Vec::new()
            };
            system["query_results"].as_array_mut().unwrap().push(json!({
                "query_id":query_id,
                "status":"complete",
                "candidate_record_ids":candidates,
                "elapsed_microseconds":10,
                "truncated":false
            }));
            system["pair_results"].as_array_mut().unwrap().push(json!({
                "pair_id":pair_id,
                "native_outcome":null,
                "mapped_outcome":"review",
                "action":"none"
            }));
        }
    }

    fn append_non_match_group(
        v: &mut Value,
        number: usize,
        pair_count: usize,
        open_false_actions: usize,
        lite_false_actions: usize,
    ) {
        let suffix = format!("{number:03}");
        let group_id = format!("fixture-group-action-{suffix}");
        let anchor_id = format!("fixture-record-action-{suffix}-anchor");
        let query_id = format!("fixture-query-action-{suffix}");
        v["records"].as_array_mut().unwrap().push(json!({
            "record_id":anchor_id,
            "gold_cluster_id":format!("fixture-cluster-action-{suffix}-anchor"),
            "partition_group_id":group_id,
            "split":"held_out_test",
            "language_tag":null,
            "script":null,
            "source_bucket":null,
            "completeness_bucket":null
        }));
        v["queries"].as_array_mut().unwrap().push(json!({
            "query_id":query_id,
            "anchor_record_id":anchor_id,
            "split":"held_out_test",
            "independent_group_id":group_id
        }));
        for pair_number in 0..pair_count {
            let pair_suffix = format!("{suffix}-{pair_number:03}");
            let candidate_id = format!("fixture-record-action-{pair_suffix}-candidate");
            let pair_id = format!("fixture-pair-action-{pair_suffix}");
            v["records"].as_array_mut().unwrap().push(json!({
                "record_id":candidate_id,
                "gold_cluster_id":format!("fixture-cluster-action-{pair_suffix}-candidate"),
                "partition_group_id":format!("fixture-group-action-{pair_suffix}-candidate"),
                "split":"held_out_test",
                "language_tag":null,
                "script":null,
                "source_bucket":null,
                "completeness_bucket":null
            }));
            v["pairs"].as_array_mut().unwrap().push(json!({
                "pair_id":pair_id,
                "query_id":query_id,
                "candidate_record_id":candidate_id,
                "split":"held_out_test",
                "label":"non_match",
                "label_status":"synthetic_asserted",
                "case_tags":["synthetic-distinct-control"]
            }));
            for (system_id, false_action_count) in [
                ("opencr", open_false_actions),
                ("litecr", lite_false_actions),
            ] {
                let system = v["systems"]
                    .as_array_mut()
                    .unwrap()
                    .iter_mut()
                    .find(|system| system["system_id"] == system_id)
                    .unwrap();
                system["pair_results"].as_array_mut().unwrap().push(json!({
                    "pair_id":pair_id,
                    "native_outcome":null,
                    "mapped_outcome":"review",
                    "action":if pair_number < false_action_count { "merge" } else { "none" }
                }));
            }
        }
        for system in v["systems"].as_array_mut().unwrap() {
            system["query_results"].as_array_mut().unwrap().push(json!({
                "query_id":query_id,
                "status":"complete",
                "candidate_record_ids":[],
                "elapsed_microseconds":10,
                "truncated":false
            }));
        }
    }

    #[test]
    fn false_action_bootstrap_reports_paired_reproducible_group_interval_and_counts() {
        let mut v = fixture();
        // Opposite outcomes across independent groups make the paired interval
        // non-degenerate while keeping both systems on the same sampled groups.
        v["systems"][1]["pair_results"][1]["action"] = json!("none");
        append_non_match_group(&mut v, 5, 1, 0, 1);
        append_match_group(&mut v, 6, true, true);
        let bytes = v.to_string();
        let output = compare_json(bytes.as_bytes()).unwrap();
        assert_eq!(output, compare_json(bytes.as_bytes()).unwrap());
        let report: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(report["report_version"], "litecr-paired-evaluation-v5");
        let bootstrap = &report["paired_comparison"]["false_action_rate_difference_bootstrap"];
        assert_eq!(bootstrap["status"], "estimated_synthetic_only");
        assert_eq!(
            bootstrap["estimator"],
            "false_link_or_merge_actions_over_labeled_non_match_pairs"
        );
        assert_eq!(bootstrap["method"], FALSE_ACTION_BOOTSTRAP_VERSION);
        assert_eq!(
            bootstrap["resampling_unit"],
            "labeled_non_match_independent_identity_connected_group"
        );
        assert_eq!(
            bootstrap["interval_method"],
            "nearest_rank_percentiles_2.5_and_97.5"
        );
        assert_eq!(
            bootstrap["seed_derivation"],
            "sha256(method_version || 0x00 || exact_artifact_bytes), first_64_bits_little_endian"
        );
        assert_eq!(
            bootstrap["random_generator"],
            "xorshift64star_v1_with_rejection_sampling_for_group_index"
        );
        assert_eq!(bootstrap["eligible_independent_groups"], 2);
        assert_eq!(bootstrap["zero_non_match_query_groups_excluded"], 1);
        assert_eq!(bootstrap["opencr_false_link_or_merge_actions"], 1);
        assert_eq!(bootstrap["opencr_labeled_non_match_pairs"], 2);
        assert_eq!(bootstrap["litecr_false_link_or_merge_actions"], 1);
        assert_eq!(bootstrap["litecr_labeled_non_match_pairs"], 2);
        assert_eq!(bootstrap["opencr_false_action_rate"], 0.5);
        assert_eq!(bootstrap["litecr_false_action_rate"], 0.5);
        assert_eq!(bootstrap["point_difference_litecr_minus_opencr"], 0.0);
        assert_eq!(bootstrap["percentile_95_ci_lower"], -1.0);
        assert_eq!(bootstrap["percentile_95_ci_upper"], 1.0);
        assert_eq!(bootstrap["replicates_used"], BOOTSTRAP_REPLICATES);
        assert_ne!(bootstrap["seed_u64"], Value::Null);
        assert_eq!(bootstrap["confidence_level"], 0.95);
        assert!(output.contains("synthetic actions only"));
        assert!(!output.contains("fixture-group-action-005"));
    }

    #[test]
    fn false_action_bootstrap_identical_systems_have_zero_paired_difference_interval() {
        let mut v = fixture();
        // Give both systems the same explicit action for every non-match pair,
        // then add a second independent group with the same action pattern.
        // Common group draws must keep the paired difference at zero.
        v["systems"][1]["pair_results"][1]["action"] = json!("merge");
        append_non_match_group(&mut v, 5, 2, 1, 1);
        let report: Value = serde_json::from_str(&parsed(v).unwrap()).unwrap();
        let bootstrap = &report["paired_comparison"]["false_action_rate_difference_bootstrap"];
        assert_eq!(bootstrap["eligible_independent_groups"], 2);
        assert_eq!(bootstrap["point_difference_litecr_minus_opencr"], 0.0);
        assert_eq!(bootstrap["percentile_95_ci_lower"], 0.0);
        assert_eq!(bootstrap["percentile_95_ci_upper"], 0.0);
    }

    #[test]
    fn false_action_bootstrap_resamples_all_negative_pairs_in_a_group_together() {
        let mut v = fixture();
        append_non_match_group(&mut v, 5, 3, 0, 3);
        let report: Value = serde_json::from_str(&parsed(v).unwrap()).unwrap();
        let bootstrap = &report["paired_comparison"]["false_action_rate_difference_bootstrap"];
        assert_eq!(bootstrap["eligible_independent_groups"], 2);
        assert_eq!(bootstrap["opencr_labeled_non_match_pairs"], 4);
        assert_eq!(bootstrap["litecr_labeled_non_match_pairs"], 4);
        assert_eq!(bootstrap["opencr_false_link_or_merge_actions"], 1);
        assert_eq!(bootstrap["litecr_false_link_or_merge_actions"], 4);
        assert_eq!(bootstrap["opencr_false_action_rate"], 0.25);
        assert_eq!(bootstrap["litecr_false_action_rate"], 1.0);
        assert_eq!(bootstrap["point_difference_litecr_minus_opencr"], 0.75);
        assert_eq!(bootstrap["percentile_95_ci_lower"], 0.0);
        assert_eq!(bootstrap["percentile_95_ci_upper"], 1.0);
    }

    #[test]
    fn false_action_counts_only_explicit_link_or_merge_actions_not_suggestions() {
        let mut v = fixture();
        for system in v["systems"].as_array_mut().unwrap() {
            system["pair_results"][1]["action"] = json!("none");
        }
        v["systems"][0]["pair_results"][1]["action"] = json!("not_applicable");
        // Keep a false same-person suggestion on one system and a review on
        // the other; neither is itself a committed identity action.
        v["systems"][0]["pair_results"][1]["mapped_outcome"] = json!("same_person");
        v["systems"][1]["pair_results"][1]["mapped_outcome"] = json!("review");
        append_non_match_group(&mut v, 5, 1, 0, 0);
        let report: Value = serde_json::from_str(&parsed(v).unwrap()).unwrap();
        let bootstrap = &report["paired_comparison"]["false_action_rate_difference_bootstrap"];
        assert_eq!(bootstrap["opencr_false_link_or_merge_actions"], 0);
        assert_eq!(bootstrap["litecr_false_link_or_merge_actions"], 0);
        assert_eq!(bootstrap["opencr_labeled_non_match_pairs"], 2);
        assert_eq!(bootstrap["litecr_labeled_non_match_pairs"], 2);
        assert_eq!(bootstrap["opencr_false_action_rate"], 0.0);
        assert_eq!(bootstrap["litecr_false_action_rate"], 0.0);
        assert_eq!(
            report["systems"]["opencr"]["decisions"]["false_match_suggestions"],
            1
        );
        assert_eq!(bootstrap["eligible_independent_groups"], 2);
    }

    #[test]
    fn false_action_bootstrap_suppresses_interval_without_two_negative_groups() {
        let one_group: Value = serde_json::from_str(&parsed(fixture()).unwrap()).unwrap();
        let one_group = &one_group["paired_comparison"]["false_action_rate_difference_bootstrap"];
        assert_eq!(one_group["eligible_independent_groups"], 1);
        assert_eq!(one_group["status"], "insufficient_non_match_groups");
        assert_eq!(one_group["percentile_95_ci_lower"], Value::Null);
        assert_eq!(one_group["percentile_95_ci_upper"], Value::Null);
        assert_eq!(one_group["replicates_used"], 0);

        let mut no_negative_groups = fixture();
        for pair in no_negative_groups["pairs"].as_array_mut().unwrap() {
            pair["label"] = json!("uncertain");
        }
        let no_groups: Value = serde_json::from_str(&parsed(no_negative_groups).unwrap()).unwrap();
        let no_groups = &no_groups["paired_comparison"]["false_action_rate_difference_bootstrap"];
        assert_eq!(no_groups["eligible_independent_groups"], 0);
        assert_eq!(no_groups["zero_non_match_query_groups_excluded"], 1);
        assert_eq!(no_groups["opencr_labeled_non_match_pairs"], 0);
        assert_eq!(no_groups["opencr_false_action_rate"], Value::Null);
        assert_eq!(no_groups["litecr_false_action_rate"], Value::Null);
        assert_eq!(
            no_groups["point_difference_litecr_minus_opencr"],
            Value::Null
        );
        assert_eq!(no_groups["percentile_95_ci_lower"], Value::Null);
        assert_eq!(no_groups["percentile_95_ci_upper"], Value::Null);
        assert_eq!(no_groups["status"], "insufficient_non_match_groups");
    }

    #[test]
    fn false_action_bootstrap_reports_bounded_work_as_insufficient() {
        assert_eq!(
            false_action_bootstrap_replicates(2),
            Ok(BOOTSTRAP_REPLICATES)
        );
        assert_eq!(false_action_bootstrap_replicates(50_000), Ok(1_000));
        assert_eq!(
            false_action_bootstrap_replicates(50_001),
            Err("bootstrap_work_limit_exceeded")
        );
    }

    #[test]
    fn paired_group_bootstrap_uses_common_draws_and_is_reproducible() {
        let mut v = fixture();
        v["systems"][0]["query_results"][0]["candidate_record_ids"] = json!([]);
        v["systems"][1]["query_results"][0]["candidate_record_ids"] = json!([]);
        // Both systems have identical per-group recalls (0 then 1). A common
        // paired resample therefore has a zero difference every time;
        // independently resampling each system would spuriously widen the CI.
        append_match_group(&mut v, 5, true, true);
        let bytes = v.to_string();
        let output = compare_json(bytes.as_bytes()).unwrap();
        assert_eq!(output, compare_json(bytes.as_bytes()).unwrap());
        let report: Value = serde_json::from_str(&output).unwrap();
        let bootstrap = &report["paired_comparison"]["candidate_recall_difference_bootstrap"];
        assert_eq!(bootstrap["status"], "estimated_synthetic_only");
        assert_eq!(
            bootstrap["estimator"],
            "pooled_candidate_recall_difference_litecr_minus_opencr"
        );
        assert_eq!(bootstrap["method"], BOOTSTRAP_VERSION);
        assert_eq!(
            bootstrap["resampling_unit"],
            "positive_bearing_independent_identity_connected_group"
        );
        assert_eq!(
            bootstrap["interval_method"],
            "nearest_rank_percentiles_2.5_and_97.5"
        );
        assert_eq!(
            bootstrap["random_generator"],
            "xorshift64star_v1_with_rejection_sampling_for_group_index"
        );
        assert_eq!(bootstrap["eligible_independent_groups"], 2);
        assert_eq!(bootstrap["positive_pairs"], 2);
        assert_eq!(bootstrap["replicates_used"], BOOTSTRAP_REPLICATES);
        assert_ne!(bootstrap["seed_u64"], Value::Null);
        assert_eq!(bootstrap["point_difference_litecr_minus_opencr"], 0.0);
        assert_eq!(bootstrap["percentile_95_ci_lower"], 0.0);
        assert_eq!(bootstrap["percentile_95_ci_upper"], 0.0);
        assert!(output.contains("synthetic mechanics only"));
        assert!(!output.contains("fixture-group-control-005"));
    }

    #[test]
    fn locale_and_script_recall_bootstraps_are_paired_deterministic_and_keep_denominators() {
        let mut v = fixture();
        for system in v["systems"].as_array_mut().unwrap() {
            system["query_results"][0]["candidate_record_ids"] = json!([]);
        }
        append_match_group_with_metadata(&mut v, 5, true, true, Some("lo-LA"), Some("Laoo"));

        let bytes = v.to_string();
        let output = compare_json(bytes.as_bytes()).unwrap();
        assert_eq!(output, compare_json(bytes.as_bytes()).unwrap());
        let report: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(report["report_version"], "litecr-paired-evaluation-v5");
        let paired = &report["paired_comparison"]["candidate_recall_subgroup_bootstrap"];
        for subgroup in [&paired["by_locale"]["lo-LA"], &paired["by_script"]["Laoo"]] {
            assert_eq!(subgroup["status"], "estimated_synthetic_only");
            assert_eq!(
                subgroup["estimator"],
                "subgroup_candidate_recall_difference_litecr_minus_opencr"
            );
            assert_eq!(subgroup["method"], SUBGROUP_BOOTSTRAP_VERSION);
            assert_eq!(subgroup["eligible_independent_groups"], 2);
            assert_eq!(subgroup["positive_pairs"], 2);
            assert_eq!(subgroup["opencr_retrieved_match_pairs"], 1);
            assert_eq!(subgroup["opencr_match_pairs"], 2);
            assert_eq!(subgroup["opencr_candidate_recall"], 0.5);
            assert_eq!(subgroup["litecr_retrieved_match_pairs"], 1);
            assert_eq!(subgroup["litecr_match_pairs"], 2);
            assert_eq!(subgroup["litecr_candidate_recall"], 0.5);
            assert_eq!(subgroup["point_difference_litecr_minus_opencr"], 0.0);
            assert_eq!(subgroup["percentile_95_ci_lower"], 0.0);
            assert_eq!(subgroup["percentile_95_ci_upper"], 0.0);
            assert_eq!(subgroup["replicates_used"], BOOTSTRAP_REPLICATES);
            assert_ne!(subgroup["seed_u64"], Value::Null);
        }
    }

    #[test]
    fn locale_and_script_bootstraps_keep_missing_buckets_and_suppress_sparse_intervals() {
        let mut v = fixture();
        append_match_group_with_metadata(&mut v, 5, true, false, None, Some("Laoo"));
        append_match_group_with_metadata(&mut v, 6, true, false, Some("lo-LA"), None);

        let report: Value = serde_json::from_str(&parsed(v).unwrap()).unwrap();
        let paired = &report["paired_comparison"]["candidate_recall_subgroup_bootstrap"];
        for subgroup in [&paired["by_locale"][MISSING], &paired["by_script"][MISSING]] {
            assert_eq!(subgroup["status"], "insufficient_positive_bearing_groups");
            assert_eq!(subgroup["eligible_independent_groups"], 1);
            assert_eq!(subgroup["positive_pairs"], 1);
            assert_eq!(subgroup["opencr_match_pairs"], 1);
            assert_eq!(subgroup["litecr_match_pairs"], 1);
            assert_eq!(subgroup["percentile_95_ci_lower"], Value::Null);
            assert_eq!(subgroup["percentile_95_ci_upper"], Value::Null);
            assert_eq!(subgroup["replicates_used"], 0);
        }
    }

    #[test]
    fn subgroup_bootstrap_work_is_bounded_across_the_report() {
        assert_eq!(subgroup_bootstrap_replicates(0), BOOTSTRAP_REPLICATES);
        assert_eq!(subgroup_bootstrap_replicates(2), BOOTSTRAP_REPLICATES);
        assert_eq!(subgroup_bootstrap_replicates(5_001), 9_998);
        assert_eq!(subgroup_bootstrap_replicates(50_001), 999);
    }

    #[test]
    fn identical_system_outputs_have_zero_paired_difference_interval() {
        let mut v = fixture();
        append_match_group(&mut v, 5, true, true);
        v["systems"][1]["query_results"] = v["systems"][0]["query_results"].clone();
        v["systems"][1]["pair_results"] = v["systems"][0]["pair_results"].clone();
        let report: Value = serde_json::from_str(&parsed(v).unwrap()).unwrap();
        let bootstrap = &report["paired_comparison"]["candidate_recall_difference_bootstrap"];
        assert_eq!(bootstrap["point_difference_litecr_minus_opencr"], 0.0);
        assert_eq!(bootstrap["percentile_95_ci_lower"], 0.0);
        assert_eq!(bootstrap["percentile_95_ci_upper"], 0.0);
    }

    #[test]
    fn zero_or_one_positive_bearing_group_suppresses_interval() {
        let one_group: Value = serde_json::from_str(&parsed(fixture()).unwrap()).unwrap();
        let one_group = &one_group["paired_comparison"]["candidate_recall_difference_bootstrap"];
        assert_eq!(one_group["eligible_independent_groups"], 1);
        assert_eq!(one_group["status"], "insufficient_positive_bearing_groups");
        assert_eq!(one_group["percentile_95_ci_lower"], Value::Null);
        assert_eq!(one_group["percentile_95_ci_upper"], Value::Null);
        assert_eq!(one_group["replicates_used"], 0);

        let mut no_positive = fixture();
        for pair in no_positive["pairs"].as_array_mut().unwrap() {
            pair["label"] = json!("uncertain");
        }
        let report: Value = serde_json::from_str(&parsed(no_positive).unwrap()).unwrap();
        let bootstrap = &report["paired_comparison"]["candidate_recall_difference_bootstrap"];
        assert_eq!(bootstrap["eligible_independent_groups"], 0);
        assert_eq!(bootstrap["zero_positive_query_groups_excluded"], 1);
        assert_eq!(bootstrap["positive_pairs"], 0);
        assert_eq!(
            bootstrap["point_difference_litecr_minus_opencr"],
            Value::Null
        );
        assert_eq!(bootstrap["percentile_95_ci_lower"], Value::Null);
        assert_eq!(bootstrap["percentile_95_ci_upper"], Value::Null);
        assert_eq!(bootstrap["status"], "insufficient_positive_bearing_groups");
    }

    #[test]
    fn failed_and_unavailable_queries_are_bootstrapped_as_retrieval_misses() {
        let mut v = fixture();
        v["systems"][1]["query_results"][0]["candidate_record_ids"] = json!([]);
        append_match_group(&mut v, 5, false, true);
        v["systems"][0]["query_results"][0]["status"] = json!("failed");
        v["systems"][0]["query_results"][0]["candidate_record_ids"] = json!([]);
        v["systems"][1]["query_results"][1]["status"] = json!("unavailable");
        v["systems"][1]["query_results"][1]["candidate_record_ids"] = json!([]);
        let report: Value = serde_json::from_str(&parsed(v).unwrap()).unwrap();
        let bootstrap = &report["paired_comparison"]["candidate_recall_difference_bootstrap"];
        assert_eq!(bootstrap["point_difference_litecr_minus_opencr"], 0.0);
        assert_eq!(bootstrap["percentile_95_ci_lower"], 0.0);
        assert_eq!(bootstrap["percentile_95_ci_upper"], 0.0);
    }

    #[test]
    fn denominator_edges_and_missing_groups_are_visible() {
        let mut v = fixture();
        v["pairs"][0]["label"] = json!("uncertain");
        v["pairs"][1]["label"] = json!("uncertain");
        let report: Value = serde_json::from_str(&parsed(v).unwrap()).unwrap();
        let sys = &report["systems"]["opencr"];
        assert_eq!(sys["candidate_recall"], Value::Null);
        assert_eq!(
            sys["sampled_labeled_negative_retrieval_burden"],
            Value::Null
        );
        assert_eq!(sys["subgroups"]["by_locale"]["lo-LA"]["pairs"], 3);
        assert_eq!(sys["decisions"]["uncertain_label_outcomes"], 3);
    }

    #[test]
    fn workload_time_exposes_missing_measurements_and_does_not_saturate_overflow() {
        let mut missing = fixture();
        for system in missing["systems"].as_array_mut().unwrap() {
            system["query_results"][0]["elapsed_microseconds"] = Value::Null;
        }
        let report: Value = serde_json::from_str(&parsed(missing).unwrap()).unwrap();
        let workload = &report["systems"]["opencr"]["workload"];
        assert_eq!(workload["elapsed_microseconds_total"], Value::Null);
        assert_eq!(workload["elapsed_microseconds_missing"], 1);
        assert_eq!(workload["elapsed_microseconds_sum_overflow"], false);

        let mut overflow = fixture();
        overflow["records"].as_array_mut().unwrap().push(json!({
            "record_id":"fixture-record-control-005",
            "gold_cluster_id":"fixture-cluster-control-004",
            "partition_group_id":"fixture-group-control-004",
            "split":"held_out_test",
            "language_tag":null,
            "script":null,
            "source_bucket":null,
            "completeness_bucket":null
        }));
        overflow["queries"].as_array_mut().unwrap().push(json!({
            "query_id":"fixture-query-control-002",
            "anchor_record_id":"fixture-record-control-005",
            "split":"held_out_test",
            "independent_group_id":"fixture-group-control-004"
        }));
        for system in overflow["systems"].as_array_mut().unwrap() {
            system["query_results"].as_array_mut().unwrap().push(json!({
                "query_id":"fixture-query-control-002",
                "status":"complete",
                "candidate_record_ids":[],
                "elapsed_microseconds":u64::MAX,
                "truncated":false
            }));
            system["query_results"][0]["elapsed_microseconds"] = json!(u64::MAX);
        }
        let report: Value = serde_json::from_str(&parsed(overflow).unwrap()).unwrap();
        let workload = &report["systems"]["opencr"]["workload"];
        assert_eq!(workload["elapsed_microseconds_total"], Value::Null);
        assert_eq!(workload["elapsed_microseconds_missing"], 0);
        assert_eq!(workload["elapsed_microseconds_sum_overflow"], true);
    }

    #[test]
    fn truncation_and_missing_anchor_metadata_keep_explicit_denominators() {
        let mut v = fixture();
        let extra_query = json!({
            "query_id":"fixture-query-control-002",
            "anchor_record_id":"fixture-record-control-003",
            "split":"held_out_test",
            "independent_group_id":"fixture-group-control-002"
        });
        let extra_pair = json!({
            "pair_id":"fixture-pair-control-004",
            "query_id":"fixture-query-control-002",
            "candidate_record_id":"fixture-record-control-004",
            "split":"held_out_test",
            "label":"non_match",
            "label_status":"synthetic_asserted",
            "case_tags":[]
        });
        v["queries"].as_array_mut().unwrap().push(extra_query);
        v["pairs"].as_array_mut().unwrap().push(extra_pair);
        for system in v["systems"].as_array_mut().unwrap() {
            system["query_results"].as_array_mut().unwrap().push(json!({
                "query_id":"fixture-query-control-002",
                "status":"complete",
                "candidate_record_ids":[],
                "elapsed_microseconds":5,
                "truncated":false
            }));
            system["pair_results"].as_array_mut().unwrap().push(json!({
                "pair_id":"fixture-pair-control-004",
                "native_outcome":null,
                "mapped_outcome":"different_person",
                "action":"none"
            }));
        }

        // A truncated but complete response can still return a labeled positive;
        // recall reflects the returned candidate set while truncation stays visible.
        v["systems"][0]["query_results"][0]["truncated"] = json!(true);
        let report: Value = serde_json::from_str(&parsed(v.clone()).unwrap()).unwrap();
        let open = &report["systems"]["opencr"];
        assert_eq!(open["candidate_recall"], 1.0);
        assert_eq!(open["workload"]["truncated_queries"], 1);
        assert_eq!(open["subgroups"]["by_locale"][MISSING]["pairs"], 1);
        assert_eq!(open["subgroups"]["by_script"][MISSING]["pairs"], 1);
        assert_eq!(open["subgroups"]["by_locale"]["lo-LA"]["pairs"], 3);

        // If the capped result omits the positive, the returned-set recall falls;
        // no candidate-set comparison or aggregate rewrites this denominator.
        v["systems"][0]["query_results"][0]["candidate_record_ids"] =
            json!(["fixture-record-control-003", "fixture-record-control-004"]);
        let report: Value = serde_json::from_str(&parsed(v).unwrap()).unwrap();
        let open = &report["systems"]["opencr"];
        assert_eq!(open["candidate_recall"], 0.0);
        assert_eq!(open["match_pairs"], 1);
        assert_eq!(open["retrieved_match_pairs"], 0);
        assert_eq!(open["labeled_non_match_pairs"], 2);
        assert_eq!(open["subgroups"]["by_locale"][MISSING]["pairs"], 1);
    }

    #[test]
    fn duplicate_query_ids_or_anchors_are_rejected() {
        let mut v = fixture();
        let original_query = v["queries"][0].clone();
        v["queries"].as_array_mut().unwrap().push(original_query);
        assert!(
            parsed(v)
                .unwrap_err()
                .contains("duplicate query id or anchor")
        );
    }

    #[test]
    fn failed_and_unavailable_queries_count_as_not_retrieved() {
        let mut v = fixture();
        for s in v["systems"].as_array_mut().unwrap() {
            s["query_results"][0]["status"] = json!("failed");
            s["query_results"][0]["candidate_record_ids"] = json!([]);
        }
        let report: Value = serde_json::from_str(&parsed(v.clone()).unwrap()).unwrap();
        assert_eq!(report["systems"]["litecr"]["candidate_recall"], 0.0);
        assert_eq!(report["systems"]["litecr"]["workload"]["failed_queries"], 1);
        assert_eq!(report["paired_comparison"]["same_candidate_set_queries"], 0);
        assert_eq!(
            report["paired_comparison"]["differing_candidate_set_queries"],
            0
        );
        assert_eq!(
            report["paired_comparison"]["candidate_set_comparisons_skipped_incomplete"],
            1
        );
        for s in v["systems"].as_array_mut().unwrap() {
            s["query_results"][0]["status"] = json!("unavailable");
        }
        let report: Value = serde_json::from_str(&parsed(v).unwrap()).unwrap();
        assert_eq!(
            report["systems"]["opencr"]["workload"]["unavailable_queries"],
            1
        );
        assert_eq!(report["paired_comparison"]["same_candidate_set_queries"], 0);
        assert_eq!(
            report["paired_comparison"]["differing_candidate_set_queries"],
            0
        );
        assert_eq!(
            report["paired_comparison"]["candidate_set_comparisons_skipped_incomplete"],
            1
        );
    }

    #[test]
    fn coverage_and_duplicate_candidate_are_rejected() {
        let mut v = fixture();
        v["systems"][0]["pair_results"]
            .as_array_mut()
            .unwrap()
            .pop();
        assert!(parsed(v).unwrap_err().contains("coverage"));
        let mut v = fixture();
        v["systems"][0]["query_results"][0]["candidate_record_ids"] =
            json!(["fixture-record-control-002", "fixture-record-control-002"]);
        assert!(parsed(v).unwrap_err().contains("duplicate ids"));
    }

    #[test]
    fn split_identity_leakage_is_rejected() {
        let mut v = fixture();
        v["records"][1]["split"] = json!("train");
        assert!(parsed(v).unwrap_err().contains("split"));
    }

    #[test]
    fn split_identity_guard_rejects_gold_cluster_or_partition_group_reuse() {
        let mut gold = BTreeMap::new();
        check_single_split(
            &mut gold,
            "fixture-cluster-control-001",
            "train",
            "gold identity cluster",
        )
        .unwrap();
        assert!(
            check_single_split(
                &mut gold,
                "fixture-cluster-control-001",
                "held_out_test",
                "gold identity cluster"
            )
            .unwrap_err()
            .contains("crosses split")
        );
        let mut groups = BTreeMap::new();
        check_single_split(
            &mut groups,
            "fixture-group-control-001",
            "train",
            "partition group",
        )
        .unwrap();
        assert!(
            check_single_split(
                &mut groups,
                "fixture-group-control-001",
                "held_out_test",
                "partition group"
            )
            .unwrap_err()
            .contains("crosses split")
        );
    }

    #[test]
    fn one_gold_identity_cannot_be_counted_as_multiple_independent_groups() {
        let mut v = fixture();
        // The anchor and positive candidate are the same gold identity. Splitting
        // them into separate partition groups would make the evaluator accept
        // correlated queries as independent observations.
        v["records"][1]["partition_group_id"] = json!("fixture-group-control-other");
        assert!(
            parsed(v)
                .unwrap_err()
                .contains("multiple independent partition groups")
        );
    }

    #[test]
    fn report_is_redacted_and_retrieval_decision_action_are_distinct() {
        let mut v = fixture();
        v["records"][0]["record_id"] = json!("fixture-record-secret-888");
        v["queries"][0]["anchor_record_id"] = json!("fixture-record-secret-888");
        v["systems"][0]["pair_results"][1]["mapped_outcome"] = json!("review");
        let output = parsed(v).unwrap();
        assert!(!output.contains("fixture-record-secret-888"));
        let report: Value = serde_json::from_str(&output).unwrap();
        let sys = &report["systems"]["opencr"];
        assert_eq!(sys["retrieved_labeled_non_match_pairs"], 1);
        assert_eq!(sys["decisions"]["review_outcomes"], 1);
        assert_eq!(sys["decisions"]["false_match_suggestions"], 0);
        assert_eq!(
            sys["decisions"]["false_link_or_merge_actions_on_non_match"],
            1
        );
        assert!(output.contains("synthetic mechanics only"));
        assert!(output.contains("sampled_labeled_negative_retrieval_burden"));
        assert!(!output.to_lowercase().contains("specificity"));
    }

    #[test]
    fn proposed_cluster_metrics_use_only_explicit_actions_and_keep_singletons() {
        let mut input = fixture();
        let initial: Value = serde_json::from_str(&parsed(input.clone()).unwrap()).unwrap();
        let clusters = &initial["systems"]["opencr"]["proposed_clusters"];
        assert_eq!(clusters["record_universe"], 4);
        assert_eq!(clusters["proposed_link_or_merge_edges"], 1);
        assert_eq!(clusters["pairwise_true_positive_pairs"], 0);
        assert_eq!(clusters["pairwise_false_positive_pairs"], 1);
        assert_eq!(clusters["pairwise_false_negative_pairs"], 1);
        assert_eq!(clusters["overmerged_predicted_components"], 1);
        assert_eq!(clusters["fragmented_gold_clusters"], 1);
        assert_eq!(clusters["bcubed_precision"], 0.75);
        assert_eq!(clusters["bcubed_recall"], 0.75);

        // A review/same-person suggestion is not an edge. Only an explicit
        // action may join the true pair; its transitive false edge is visible.
        input["systems"][0]["pair_results"][0]["action"] = json!("link");
        let linked: Value = serde_json::from_str(&parsed(input).unwrap()).unwrap();
        let clusters = &linked["systems"]["opencr"]["proposed_clusters"];
        assert_eq!(clusters["proposed_link_or_merge_edges"], 2);
        assert_eq!(clusters["pairwise_true_positive_pairs"], 1);
        assert_eq!(clusters["pairwise_false_positive_pairs"], 2);
        assert_eq!(clusters["pairwise_false_negative_pairs"], 0);
        assert_eq!(clusters["overmerged_predicted_components"], 1);
        assert_eq!(clusters["fragmented_gold_clusters"], 0);
        assert_eq!(clusters["pairwise_precision"], 1.0 / 3.0);
        assert_eq!(clusters["pairwise_recall"], 1.0);
    }

    #[test]
    fn report_binds_results_to_exact_input_bytes_without_exposing_row_ids() {
        let artifact = fixture().to_string();
        let output = compare_json(artifact.as_bytes()).unwrap();
        let report: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(
            report["input_artifact_sha256"],
            format!("sha256:{}", hex_digest(artifact.as_bytes()))
        );
        assert!(output.contains("integrity aid, not anonymization"));
        assert!(!output.contains("fixture-record-control-001"));

        let whitespace_variant = format!(" {artifact}");
        let second: Value =
            serde_json::from_str(&compare_json(whitespace_variant.as_bytes()).unwrap()).unwrap();
        assert_ne!(
            report["input_artifact_sha256"], second["input_artifact_sha256"],
            "digest must distinguish the exact submitted bytes"
        );
        assert_ne!(
            report["paired_comparison"]["candidate_recall_difference_bootstrap"]["seed_u64"],
            second["paired_comparison"]["candidate_recall_difference_bootstrap"]["seed_u64"],
            "bootstrap seed must also bind to exact artifact bytes"
        );
    }

    #[test]
    fn unknown_subgroup_strings_and_non_synthetic_artifacts_are_rejected() {
        let mut v = fixture();
        v["records"][0]["source_bucket"] = json!("raw source: clinic North 17");
        assert!(parsed(v).unwrap_err().contains("allowlisted"));
        let mut v = fixture();
        v["data_class"] = json!("authorized_controlled");
        assert!(parsed(v).unwrap_err().contains("synthetic"));
    }

    #[test]
    fn nullable_subgroup_keys_are_required_and_synthetic_labels_cannot_claim_adjudication() {
        let mut v = fixture();
        v["records"][0].as_object_mut().unwrap().remove("script");
        assert!(
            parsed(v)
                .unwrap_err()
                .contains("properties must be present")
        );
        let mut v = fixture();
        v["pairs"][0]["label_status"] = json!("adjudicated");
        assert!(parsed(v).unwrap_err().contains("synthetic_asserted"));
    }
}
