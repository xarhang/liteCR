import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const here = path.dirname(fileURLToPath(import.meta.url));
const dataset = JSON.parse(await readFile(path.join(here, 'dataset.json'), 'utf8'));
const schema = JSON.parse(await readFile(path.join(here, 'schema', 'dataset.schema.json'), 'utf8'));
const errors = [];
const assert = (condition, message) => { if (!condition) errors.push(message); };
const unique = (values) => new Set(values).size === values.length;
const byId = (items, key) => new Map(items.map((item) => [item[key], item]));

assert(schema.$schema === 'https://json-schema.org/draft/2020-12/schema', 'schema must declare JSON Schema 2020-12');
assert(dataset.schema_version === '1.0', 'unsupported dataset schema version');
assert(dataset.dataset_status === 'synthetic_fixture', 'checked-in fixture must be labeled synthetic_fixture');
assert(dataset.purpose === 'mechanics_only', 'checked-in fixture must be mechanics_only');
assert(dataset.provenance?.contains_real_person_data === false, 'synthetic provenance must explicitly deny real-person data');
assert(dataset.splits.includes('train') && dataset.splits.includes('development') && dataset.splits.includes('held_out_test'), 'all declared split classes must be present');

const records = byId(dataset.records, 'record_id');
const clusters = byId(dataset.clusters, 'gold_cluster_id');
assert(records.size === dataset.records.length, 'record_id values must be unique');
assert(clusters.size === dataset.clusters.length, 'gold_cluster_id values must be unique');
assert(unique(dataset.pair_labels.map((pair) => pair.pair_id)), 'pair_id values must be unique');
assert(unique(dataset.queries.map((query) => query.query_id)), 'query_id values must be unique');

const recordClusterSplits = new Map();
for (const record of dataset.records) {
  assert(dataset.splits.includes(record.split), `${record.record_id} uses an undeclared split`);
  const prior = recordClusterSplits.get(record.gold_cluster_id);
  assert(prior === undefined || prior === record.split, `gold cluster ${record.gold_cluster_id} leaks across splits`);
  recordClusterSplits.set(record.gold_cluster_id, record.split);
  const cluster = clusters.get(record.gold_cluster_id);
  assert(cluster, `${record.record_id} refers to missing gold cluster ${record.gold_cluster_id}`);
  if (cluster) {
    assert(cluster.split === record.split, `${record.record_id} split differs from its cluster split`);
    assert(cluster.member_record_ids.includes(record.record_id), `${record.record_id} missing from its cluster member list`);
  }
  for (const name of record.name_assertions) {
    if (name.representation === 'transliteration') {
      assert(name.normalization_version !== 'fixture-raw-v1', `${record.record_id} transliteration needs a distinct method/version tag`);
    }
  }
}
for (const cluster of dataset.clusters) {
  assert(dataset.splits.includes(cluster.split), `${cluster.gold_cluster_id} uses an undeclared split`);
  for (const recordId of cluster.member_record_ids) {
    const record = records.get(recordId);
    assert(record, `${cluster.gold_cluster_id} references missing record ${recordId}`);
    if (record) assert(record.gold_cluster_id === cluster.gold_cluster_id, `${recordId} has a different cluster than its membership list`);
  }
}

for (const pair of dataset.pair_labels) {
  const left = records.get(pair.left_record_id);
  const right = records.get(pair.right_record_id);
  assert(left && right, `${pair.pair_id} references a missing record`);
  if (left && right) {
    assert(left.record_id !== right.record_id, `${pair.pair_id} must compare two distinct records`);
    assert(left.split === right.split && pair.split === left.split, `${pair.pair_id} crosses split boundaries`);
    if (pair.label === 'match') assert(left.gold_cluster_id === right.gold_cluster_id, `${pair.pair_id} match label disagrees with gold clusters`);
    if (pair.label === 'non_match') assert(left.gold_cluster_id !== right.gold_cluster_id, `${pair.pair_id} non_match label disagrees with gold clusters`);
  }
}

for (const query of dataset.queries) {
  const record = records.get(query.query_record_id);
  assert(record, `${query.query_id} refers to missing query record`);
  if (!record) continue;
  assert(query.split === record.split, `${query.query_id} split differs from query record`);
  assert(unique(query.eligible_match_record_ids), `${query.query_id} repeats a positive candidate`);
  assert(unique(query.hard_negative_record_ids), `${query.query_id} repeats a hard negative`);
  const positiveIds = dataset.records.filter((candidate) => candidate.gold_cluster_id === record.gold_cluster_id && candidate.record_id !== record.record_id && candidate.split === record.split).map((candidate) => candidate.record_id).sort();
  assert(JSON.stringify([...query.eligible_match_record_ids].sort()) === JSON.stringify(positiveIds), `${query.query_id} positive truth is not exhaustive for its split`);
  for (const candidateId of [...query.eligible_match_record_ids, ...query.hard_negative_record_ids]) {
    const candidate = records.get(candidateId);
    assert(candidate, `${query.query_id} references missing candidate ${candidateId}`);
    if (candidate) assert(candidate.split === query.split, `${query.query_id} candidate ${candidateId} crosses splits`);
    assert(candidateId !== query.query_record_id, `${query.query_id} lists itself as a candidate`);
  }
  for (const candidateId of query.hard_negative_record_ids) {
    const candidate = records.get(candidateId);
    if (candidate) assert(candidate.gold_cluster_id !== record.gold_cluster_id, `${query.query_id} hard negative ${candidateId} is a positive cluster member`);
  }
  assert(query.eligible_match_record_ids.every((id) => !query.hard_negative_record_ids.includes(id)), `${query.query_id} positive and negative sets overlap`);
}

const expectedCaseFamilies = ['common-name', 'household-shared-contact', 'twin-hard-negative', 'cross-script', 'partial-date', 'conflicting-authoritative-id', 'canonical-equivalence', 'exact-name-control', 'orthographic-near-neighbor', 'no-transliteration-control', 'approximate-candidate-only-control'];
const observedTags = new Set(dataset.pair_labels.flatMap((pair) => pair.case_tags));
for (const tag of expectedCaseFamilies) assert(observedTags.has(tag), `required synthetic case family missing: ${tag}`);
const requiredScripts = ['Laoo', 'Thai', 'Khmr', 'Latn', 'Deva', 'Arab', 'Cyrl'];
const observedScripts = new Set(dataset.records.flatMap((record) => record.name_assertions
  .filter((name) => name.representation === 'source_original' && typeof name.script === 'string')
  .map((name) => name.script)));
for (const script of requiredScripts) assert(observedScripts.has(script), `required multilingual script missing: ${script}`);
for (const script of requiredScripts) {
  const controls = dataset.records.filter((record) => record.case_tags.includes('approximate-candidate-only-control') && record.name_assertions.some((name) => name.representation === 'source_original' && name.script === script));
  assert(controls.length > 0, `approximate candidate-generation control missing for ${script}`);
  assert(controls.every((record) => record.date_of_birth === null), `approximate candidate-generation control for ${script} must omit DOB to isolate name retrieval`);
}
assert(dataset.records.some((record) => record.name_assertions.some((name) => name.representation === 'source_original' && !name.script)), 'fixture must include a source name with missing script metadata');
assert(dataset.records.some((record) => record.name_assertions.some((name) => name.representation === 'source_original' && !name.language)), 'fixture must include a source name with missing language metadata');
for (const query of dataset.queries.filter((entry) => entry.split === 'held_out_test')) {
  assert(query.case_tags.length > 0, `${query.query_id} should state the synthetic case family it exercises`);
}

if (errors.length) {
  console.error(`Fixture validation failed (${errors.length} issue(s)):`);
  for (const error of errors) console.error(`- ${error}`);
  process.exitCode = 1;
} else {
  console.log(`Fixture checks passed: ${dataset.records.length} records, ${dataset.clusters.length} clusters, ${dataset.pair_labels.length} labeled pairs, ${dataset.queries.length} queries.`);
  console.log('Checks cover JSON parsing, fixture provenance, IDs/references, cluster-level split isolation, exhaustive query positives, pair-label/cluster consistency, hard-negative separation, and required case tags.');
  console.log('This is a fixture-mechanics check, not JSON Schema engine validation or matching-quality evidence.');
}
