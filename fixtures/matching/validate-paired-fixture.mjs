import { readFile } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const here = path.dirname(fileURLToPath(import.meta.url));
const fixturePath = path.join(here, 'paired', 'benchmark.synthetic.json');
const schemaPath = path.join(here, 'paired', 'schema', 'benchmark.schema.json');
const expectedSystemIds = ['litecr', 'opencr'];
const expectedFocusLocales = ['lo-LA', 'en-GB', 'th-TH', 'vi-VN', 'hi-IN', 'km-KH', 'ar', 'ru'];
const expectedFocusScripts = ['Laoo', 'Latn', 'Thai', 'Deva', 'Khmr', 'Arab', 'Cyrl'];
const splits = new Set(['train', 'development', 'held_out_test']);
const ownKeys = (object, keys) => object && typeof object === 'object' && !Array.isArray(object)
  && Object.keys(object).sort().join('|') === [...keys].sort().join('|');
const unique = (values) => new Set(values).size === values.length;
const digest = (value) => typeof value === 'string' && /^sha256:[0-9a-f]{64}$/.test(value);
const sha256 = (value) => `sha256:${createHash('sha256').update(JSON.stringify(value)).digest('hex')}`;
const id = (value) => typeof value === 'string' && /^fixture-[a-z0-9-]{3,60}$/.test(value);
const text = (value) => typeof value === 'string' && value.length > 0;

function validate(benchmark) {
  const errors = [];
  const add = (condition, message) => { if (!condition) errors.push(message); };
  add(ownKeys(benchmark, ['schema_version', 'benchmark_id', 'data_class', 'purpose', 'split', 'source_snapshot_digest', 'label_guide_version', 'partition_policy', 'records', 'queries', 'pairs', 'systems']), 'top-level fields must exactly match schema');
  add(benchmark?.schema_version === '1.0', 'schema_version must be 1.0');
  add(typeof benchmark?.benchmark_id === 'string' && /^synthetic-control-[a-z0-9-]{3,48}$/.test(benchmark.benchmark_id), 'benchmark_id must identify a synthetic control fixture');
  add(benchmark?.data_class === 'synthetic_fixture' && benchmark?.purpose === 'mechanics_only', 'only synthetic_fixture / mechanics_only inputs are accepted');
  add(benchmark?.split === 'held_out_test', 'benchmark split must be held_out_test');
  add(digest(benchmark?.source_snapshot_digest), 'source_snapshot_digest must use sha256:<64 lowercase hex>');
  add(typeof benchmark?.label_guide_version === 'string' && /^synthetic-guide-[a-z0-9.-]+$/.test(benchmark.label_guide_version), 'label_guide_version must be explicitly synthetic');
  add(benchmark?.partition_policy === 'identity_connected_v1', 'partition_policy must be identity_connected_v1');
  for (const field of ['records', 'queries', 'pairs', 'systems']) add(Array.isArray(benchmark?.[field]), `${field} must be an array`);
  if (!['records', 'queries', 'pairs', 'systems'].every((field) => Array.isArray(benchmark?.[field]))) return errors;

  const recordKeys = ['record_id', 'gold_cluster_id', 'partition_group_id', 'split', 'language_tag', 'script', 'source_bucket', 'completeness_bucket'];
  const queryKeys = ['query_id', 'anchor_record_id', 'split', 'independent_group_id'];
  const pairKeys = ['pair_id', 'query_id', 'candidate_record_id', 'split', 'label', 'label_status', 'case_tags'];
  const queryResultKeys = ['query_id', 'status', 'candidate_record_ids', 'elapsed_microseconds', 'truncated'];
  const pairResultKeys = ['pair_id', 'native_outcome', 'mapped_outcome', 'action'];
  const recordMap = new Map();
  const queryMap = new Map();
  const pairMap = new Map();
  const idLeakage = new Map();
  for (const [index, record] of benchmark.records.entries()) {
    add(ownKeys(record, recordKeys), `record[${index}] fields must exactly match schema`);
    add(id(record?.record_id) && id(record?.gold_cluster_id) && id(record?.partition_group_id), `record[${index}] identifiers must be fixture-only IDs`);
    add(splits.has(record?.split), `record[${index}] has unsupported split`);
    add(record?.language_tag === null || (typeof record?.language_tag === 'string' && /^[a-z]{2,3}(-[A-Z]{2})?$/.test(record.language_tag)), `record[${index}] language_tag is malformed`);
    add(record?.script === null || (typeof record?.script === 'string' && /^[A-Z][a-z]{3}$/.test(record.script)), `record[${index}] script is malformed`);
    for (const field of ['source_bucket', 'completeness_bucket']) add(record?.[field] === null || (typeof record?.[field] === 'string' && /^[a-zA-Z0-9][a-zA-Z0-9._-]{0,63}$/.test(record[field])), `record[${index}] ${field} is malformed`);
    add(!recordMap.has(record?.record_id), `duplicate record_id ${record?.record_id}`);
    if (!recordMap.has(record?.record_id)) recordMap.set(record?.record_id, record);
    for (const field of ['gold_cluster_id', 'partition_group_id']) {
      const prior = idLeakage.get(`${field}:${record?.[field]}`);
      add(prior === undefined || prior === record?.split, `${field} ${record?.[field]} leaks across splits`);
      idLeakage.set(`${field}:${record?.[field]}`, record?.split);
    }
  }
  add(benchmark.source_snapshot_digest === sha256(benchmark.records), 'source_snapshot_digest must match the synthetic records snapshot');
  const observedLocales = new Set(benchmark.records.map((record) => record?.language_tag));
  const observedScripts = new Set(benchmark.records.map((record) => record?.script));
  for (const locale of expectedFocusLocales) add(observedLocales.has(locale), `required synthetic locale stratum is missing: ${locale}`);
  for (const script of expectedFocusScripts) add(observedScripts.has(script), `required synthetic script stratum is missing: ${script}`);

  for (const [index, query] of benchmark.queries.entries()) {
    add(ownKeys(query, queryKeys), `query[${index}] fields must exactly match schema`);
    add(id(query?.query_id) && id(query?.anchor_record_id) && id(query?.independent_group_id), `query[${index}] identifiers must be fixture-only IDs`);
    add(splits.has(query?.split), `query[${index}] has unsupported split`);
    add(!queryMap.has(query?.query_id), `duplicate query_id ${query?.query_id}`);
    if (!queryMap.has(query?.query_id)) queryMap.set(query?.query_id, query);
    const anchor = recordMap.get(query?.anchor_record_id);
    add(Boolean(anchor), `query ${query?.query_id} references missing anchor`);
    if (anchor) {
      add(anchor.split === query.split, `query ${query.query_id} split differs from anchor`);
      add(anchor.partition_group_id === query.independent_group_id, `query ${query.query_id} independent_group_id differs from anchor partition group`);
    }
  }

  const pairCandidatesByQuery = new Map();
  for (const [index, pair] of benchmark.pairs.entries()) {
    add(ownKeys(pair, pairKeys), `pair[${index}] fields must exactly match schema`);
    add(id(pair?.pair_id) && id(pair?.query_id) && id(pair?.candidate_record_id), `pair[${index}] identifiers must be fixture-only IDs`);
    add(splits.has(pair?.split), `pair[${index}] has unsupported split`);
    add(['match', 'non_match', 'uncertain'].includes(pair?.label), `pair[${index}] has invalid label`);
    add(['synthetic_asserted', 'adjudicated'].includes(pair?.label_status), `pair[${index}] has invalid label_status`);
    add(Array.isArray(pair?.case_tags) && pair.case_tags.every((tag) => typeof tag === 'string' && /^synthetic-[a-z0-9-]{2,48}$/.test(tag)) && unique(pair.case_tags), `pair[${index}] case_tags must be unique synthetic tags`);
    add(!pairMap.has(pair?.pair_id), `duplicate pair_id ${pair?.pair_id}`);
    if (!pairMap.has(pair?.pair_id)) pairMap.set(pair?.pair_id, pair);
    const query = queryMap.get(pair?.query_id);
    const candidate = recordMap.get(pair?.candidate_record_id);
    add(Boolean(query), `pair ${pair?.pair_id} references missing query`);
    add(Boolean(candidate), `pair ${pair?.pair_id} references missing candidate record`);
    if (query && candidate) {
      add(query.split === pair.split && candidate.split === pair.split, `pair ${pair.pair_id} split differs from query or candidate`);
      add(query.anchor_record_id !== candidate.record_id, `pair ${pair.pair_id} compares anchor with itself`);
      if (pair.label === 'match') add(recordMap.get(query.anchor_record_id)?.gold_cluster_id === candidate.gold_cluster_id, `pair ${pair.pair_id} match label conflicts with synthetic gold clusters`);
      if (pair.label === 'non_match') add(recordMap.get(query.anchor_record_id)?.gold_cluster_id !== candidate.gold_cluster_id, `pair ${pair.pair_id} non_match label conflicts with synthetic gold clusters`);
    }
    const candidates = pairCandidatesByQuery.get(pair?.query_id) ?? new Set();
    add(!candidates.has(pair?.candidate_record_id), `query ${pair?.query_id} repeats candidate ${pair?.candidate_record_id} in pairs`);
    candidates.add(pair?.candidate_record_id);
    pairCandidatesByQuery.set(pair?.query_id, candidates);
  }

  add(benchmark.systems.length === 2, 'exactly two systems are required');
  const observedSystemIds = benchmark.systems.map((system) => system?.system_id).sort();
  add(unique(observedSystemIds) && JSON.stringify(observedSystemIds) === JSON.stringify(expectedSystemIds), 'system IDs must be exactly opencr and litecr');
  for (const [index, system] of benchmark.systems.entries()) {
    add(ownKeys(system, ['system_id', 'system_version', 'config_digest', 'mapping_version', 'query_results', 'pair_results']), `system[${index}] fields must exactly match schema`);
    add(expectedSystemIds.includes(system?.system_id), `system[${index}] has unsupported system_id`);
    add(typeof system?.system_version === 'string' && /^synthetic-[a-zA-Z0-9._+-]{1,48}$/.test(system.system_version), `system ${system?.system_id} must use a synthetic version`);
    add(digest(system?.config_digest), `system ${system?.system_id} config_digest is malformed`);
    add(typeof system?.mapping_version === 'string' && /^synthetic-map-[a-zA-Z0-9._+-]{1,48}$/.test(system.mapping_version), `system ${system?.system_id} mapping_version must be synthetic`);
    add(system?.config_digest === sha256({ benchmark_id: benchmark.benchmark_id, system_id: system?.system_id, system_version: system?.system_version, mapping_version: system?.mapping_version, synthetic_config: true }), `system ${system?.system_id} config_digest must match its synthetic configuration descriptor`);
    add(Array.isArray(system?.query_results), `system ${system?.system_id} query_results must be an array`);
    add(Array.isArray(system?.pair_results), `system ${system?.system_id} pair_results must be an array`);
    if (!Array.isArray(system?.query_results) || !Array.isArray(system?.pair_results)) continue;
    const queryRows = new Map();
    for (const [rowIndex, row] of system.query_results.entries()) {
      add(ownKeys(row, queryResultKeys), `${system.system_id} query_result[${rowIndex}] fields must exactly match schema`);
      add(id(row?.query_id), `${system.system_id} query_result[${rowIndex}] query_id is malformed`);
      add(['complete', 'failed', 'unavailable'].includes(row?.status), `${system.system_id} query_result[${rowIndex}] status is invalid`);
      add(Array.isArray(row?.candidate_record_ids) && row.candidate_record_ids.every(id) && unique(row.candidate_record_ids), `${system.system_id} query_result[${rowIndex}] candidates must be unique fixture record IDs`);
      add(row?.elapsed_microseconds === null || (Number.isSafeInteger(row?.elapsed_microseconds) && row.elapsed_microseconds >= 0), `${system.system_id} query_result[${rowIndex}] elapsed time must be a nonnegative integer or null`);
      add(typeof row?.truncated === 'boolean', `${system.system_id} query_result[${rowIndex}] truncated must be boolean`);
      add(!queryRows.has(row?.query_id), `${system.system_id} has duplicate query_result ${row?.query_id}`);
      if (!queryRows.has(row?.query_id)) queryRows.set(row?.query_id, row);
      add(queryMap.has(row?.query_id), `${system.system_id} query_result references unknown query ${row?.query_id}`);
      for (const recordId of row?.candidate_record_ids ?? []) {
        const candidate = recordMap.get(recordId);
        add(Boolean(candidate), `${system.system_id} query_result references unknown record ${recordId}`);
        if (candidate) add(candidate.split === queryMap.get(row?.query_id)?.split, `${system.system_id} candidate ${recordId} crosses query split`);
        add(recordId !== queryMap.get(row?.query_id)?.anchor_record_id, `${system.system_id} query_result contains its own anchor`);
      }
    }
    add(queryRows.size === queryMap.size && [...queryMap.keys()].every((key) => queryRows.has(key)), `${system?.system_id} query_results must cover each query exactly once`);

    const pairRows = new Map();
    for (const [rowIndex, row] of system.pair_results.entries()) {
      add(ownKeys(row, pairResultKeys), `${system.system_id} pair_result[${rowIndex}] fields must exactly match schema`);
      add(id(row?.pair_id), `${system.system_id} pair_result[${rowIndex}] pair_id is malformed`);
      add(row?.native_outcome === null || (typeof row?.native_outcome === 'string' && row.native_outcome.length <= 128), `${system.system_id} pair_result[${rowIndex}] native_outcome must be string or null`);
      add(['same_person', 'different_person', 'review', 'not_evaluated', 'unavailable'].includes(row?.mapped_outcome), `${system.system_id} pair_result[${rowIndex}] mapped_outcome is invalid`);
      add(['none', 'link', 'merge', 'not_applicable'].includes(row?.action), `${system.system_id} pair_result[${rowIndex}] action is invalid`);
      add(!pairRows.has(row?.pair_id), `${system.system_id} has duplicate pair_result ${row?.pair_id}`);
      if (!pairRows.has(row?.pair_id)) pairRows.set(row?.pair_id, row);
      add(pairMap.has(row?.pair_id), `${system.system_id} pair_result references unknown pair ${row?.pair_id}`);
    }
    add(pairRows.size === pairMap.size && [...pairMap.keys()].every((key) => pairRows.has(key)), `${system?.system_id} pair_results must cover each pair exactly once`);
  }
  return errors;
}

const schema = JSON.parse(await readFile(schemaPath, 'utf8'));
if (schema.$schema !== 'https://json-schema.org/draft/2020-12/schema') throw new Error('Schema must declare JSON Schema 2020-12');
const benchmark = JSON.parse(await readFile(fixturePath, 'utf8'));
const errors = validate(benchmark);
if (errors.length) {
  console.error(`Paired fixture validation failed (${errors.length} issue(s)):`);
  for (const error of errors) console.error(`- ${error}`);
  process.exitCode = 1;
} else {
  console.log(`Paired fixture checks passed: ${benchmark.records.length} synthetic records, ${benchmark.queries.length} queries, ${benchmark.pairs.length} labeled pairs, ${benchmark.systems.length} system outputs.`);
  console.log(`Focus locale strata: ${expectedFocusLocales.map((locale) => `${locale} (${benchmark.records.filter((record) => record.language_tag === locale).length})`).join(', ')}.`);
  console.log(`Focus script strata: ${expectedFocusScripts.map((script) => `${script} (${benchmark.records.filter((record) => record.script === script).length})`).join(', ')}.`);
  console.log('Checks cover synthetic-only metadata, exact fields, unique IDs, references, split/identity leakage, query independence, pair uniqueness/labels, complete paired result coverage, and required locale/script strata.');
  console.log('Mechanics-only fixture: it contains no names or source identifiers and provides no OpenCR compatibility or matching-quality evidence.');
}

if (process.argv.includes('--self-test')) {
  const mutations = [
    ['split leakage', (copy) => { copy.records[0].split = 'train'; }],
    ['missing query output', (copy) => { copy.systems[0].query_results.pop(); }],
    ['duplicate candidate pair', (copy) => { copy.pairs[1].candidate_record_id = copy.pairs[0].candidate_record_id; }],
    ['invalid independent group', (copy) => { copy.queries[0].independent_group_id = 'fixture-group-control-999'; }],
    ['missing locale and script strata', (copy) => {
      for (const record of copy.records.filter((row) => row.language_tag === 'lo-LA')) {
        record.language_tag = 'en-GB';
        record.script = 'Latn';
      }
      copy.source_snapshot_digest = sha256(copy.records);
    }],
  ];
  for (const [name, mutate] of mutations) {
    const copy = structuredClone(benchmark);
    mutate(copy);
    if (validate(copy).length === 0) {
      console.error(`Self-test failed to reject mutation: ${name}`);
      process.exitCode = 1;
    } else console.log(`Self-test rejected mutation: ${name}`);
  }
  if (!process.exitCode) console.log(`Self-tests passed: ${mutations.length} malformed mutations rejected.`);
}
