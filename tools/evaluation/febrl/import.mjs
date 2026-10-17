#!/usr/bin/env node
// Convert an official FEBRL generated CSV into litecr-eval's current mechanics input.
// No FEBRL records are downloaded or bundled by this tool.
import { readFile, writeFile } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { pathToFileURL } from 'node:url';

const SOURCE = 'FEBRL project dataset1.csv; operator must verify exact source release and bundled license notice';
const MAX_INPUT_BYTES = 32 * 1024 * 1024;
const MAX_RECORDS = 5_000;
// The evaluator compares every query against the included record universe.
// FEBRL dataset1 has two records per person cluster: 158 clusters produce
// 316 queries × 316 records = 99,856 comparisons, within its 100k cap.
const MAX_INCLUDED_CLUSTERS = 158;
const MAX_QUERY_RECORD_COMPARISONS = 100_000;

function usage() {
  return 'usage: node tools/evaluation/febrl/import.mjs <dataset1.csv> <output.json>';
}

export function parseCsv(text) {
  const rows = [];
  let row = [];
  let field = '';
  let quoted = false;
  let afterQuote = false;
  for (let i = 0; i < text.length; i += 1) {
    const ch = text[i];
    if (quoted) {
      if (ch === '"' && text[i + 1] === '"') {
        field += '"';
        i += 1;
      } else if (ch === '"') {
        quoted = false;
        afterQuote = true;
      } else {
        field += ch;
      }
    } else if (ch === ',') {
      row.push(field.trim());
      field = '';
      afterQuote = false;
    } else if (ch === '\n' || ch === '\r') {
      if (ch === '\r' && text[i + 1] === '\n') i += 1;
      row.push(field.trim());
      field = '';
      if (row.some((value) => value !== '')) rows.push(row);
      row = [];
      afterQuote = false;
    } else {
      if (afterQuote) throw new Error(`unexpected character after a quoted CSV field at character ${i + 1}`);
      if (ch === '"') {
        if (field.length !== 0) throw new Error(`quote inside an unquoted CSV field at character ${i + 1}`);
        quoted = true;
        continue;
      }
      field += ch;
    }
  }
  if (quoted) throw new Error('CSV ends inside a quoted field');
  if (field.length > 0 || row.length > 0) {
    row.push(field.trim());
    if (row.some((value) => value !== '')) rows.push(row);
  }
  if (rows.length < 2) throw new Error('CSV must contain a header and records');
  const headers = rows.shift().map((header) => header.replace(/^\uFEFF/, '').trim());
  if (new Set(headers).size !== headers.length || headers.some((header) => !header)) {
    throw new Error('CSV header names must be unique and non-empty');
  }
  return rows.map((values, line) => {
    if (values.length !== headers.length) {
      throw new Error(`CSV row ${line + 2} has ${values.length} fields; expected ${headers.length}`);
    }
    return Object.fromEntries(headers.map((header, index) => [header, values[index]]));
  });
}

function parseFebrlKey(value) {
  const match = /^rec-(\d+)-(?:org|dup-\d+)$/.exec(value);
  if (!match) throw new Error(`unsupported FEBRL record identifier: ${value}`);
  return match[1];
}

function normalizedDate(value) {
  const raw = value.trim();
  if (!raw) return null;
  const match = /^(\d{4})(?:-?(\d{2})(?:-?(\d{2}))?)?$/.exec(raw);
  if (!match) return null;
  const [, year, month, day] = match;
  if (month && (Number(month) < 1 || Number(month) > 12)) return null;
  if (day) {
    const monthNumber = Number(month);
    const dayNumber = Number(day);
    if (dayNumber < 1) return null;
    const yearNumber = Number(year);
    const leapYear = yearNumber % 4 === 0 && (yearNumber % 100 !== 0 || yearNumber % 400 === 0);
    const daysInMonth = [31, leapYear ? 29 : 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    if (dayNumber > daysInMonth[monthNumber - 1]) return null;
  }
  return {
    original_value: `${year}${month ? `-${month}` : ''}${day ? `-${day}` : ''}`,
    precision: day ? 'day' : month ? 'month' : 'year',
    calendar: 'gregorian',
    estimated: false,
  };
}

export function convert(rows, sourceCsvSha256) {
  if (rows.length > MAX_RECORDS) throw new Error(`record count exceeds the ${MAX_RECORDS} row safety limit`);
  const headerSet = new Set(Object.keys(rows[0] ?? {}));
  for (const header of ['rec_id', 'given_name', 'surname']) {
    if (!headerSet.has(header)) throw new Error(`missing required FEBRL column: ${header}`);
  }

  const ids = new Set();
  const sourceClusterRows = new Map();
  for (const source of rows) {
    const key = parseFebrlKey(source.rec_id.trim());
    if (!sourceClusterRows.has(key)) sourceClusterRows.set(key, []);
    sourceClusterRows.get(key).push(source);
  }
  const sourceIsDataset1 = rows.length === 1_000 && sourceClusterRows.size === 500
    && [...sourceClusterRows.values()].every((members) => members.length === 2);
  if (!sourceIsDataset1 && rows.length > 998) {
    throw new Error('input must be FEBRL dataset1.csv (1,000 rows, 500 two-record clusters), or a test-sized extract of at most 998 rows');
  }
  const selectedSourceKeys = [...sourceClusterRows.keys()]
    .sort((left, right) => Number(left) - Number(right))
    .slice(0, MAX_INCLUDED_CLUSTERS);
  const selectedRows = selectedSourceKeys.flatMap((key) => sourceClusterRows.get(key));
  if (selectedRows.length > 998) throw new Error('selected complete FEBRL clusters exceed schema-safe record limit');
  const clusterIds = new Map(selectedSourceKeys.map((sourceKey, index) => [sourceKey, `syn-person-${String(index + 1).padStart(3, '0')}`]));

  const clusters = new Map();
  const records = [];
  let unsupportedDateValues = 0;
  for (const [index, source] of selectedRows.entries()) {
    const sourceId = source.rec_id.trim();
    if (!sourceId || ids.has(sourceId)) throw new Error(`empty or duplicate rec_id at CSV row ${index + 2}`);
    ids.add(sourceId);
    const given = source.given_name.trim();
    const surname = source.surname.trim();
    const fullName = [given, surname].filter(Boolean).join(' ');
    if (!fullName) throw new Error(`FEBRL record ${sourceId} has no given_name or surname`);

    const goldKey = parseFebrlKey(sourceId);
    const clusterId = clusterIds.get(goldKey);
    const recordId = `syn-rec-${String(index + 1).padStart(3, '0')}`;
    if (!clusters.has(clusterId)) clusters.set(clusterId, []);
    clusters.get(clusterId).push(recordId);

    const dob = source.date_of_birth ? normalizedDate(source.date_of_birth) : null;
    if (source.date_of_birth?.trim() && !dob) unsupportedDateValues += 1;
    records.push({
      record_id: recordId,
      source_system: 'urn:litecr:synthetic:source:febrl',
      gold_cluster_id: clusterId,
      split: 'held_out_test',
      name_assertions: [{
        original_value: fullName,
        script: 'Latn',
        language: 'en-AU',
        representation: 'source_original',
        normalization_version: 'febrl-input-as-supplied-v1',
      }],
      date_of_birth: dob,
      identifiers: [],
      contacts: [],
      address_token: null,
      case_tags: ['febrl-generated'],
    });
  }

  const clustersOut = [...clusters.entries()].map(([gold_cluster_id, member_record_ids]) => ({
    gold_cluster_id,
    split: 'held_out_test',
    member_record_ids,
    label_status: 'synthetic_asserted',
    provenance_ref: 'febrl-record-key-ground-truth',
  }));

  const queries = [];
  const labeledPairCandidates = new Map();
  const recordsByCluster = new Map();
  for (const record of records) {
    if (!recordsByCluster.has(record.gold_cluster_id)) recordsByCluster.set(record.gold_cluster_id, []);
    recordsByCluster.get(record.gold_cluster_id).push(record);
  }
  const allRecords = records;
  for (const [index, record] of records.entries()) {
    const members = recordsByCluster.get(record.gold_cluster_id);
    const positives = members.filter((member) => member.record_id !== record.record_id);
    const otherRecords = allRecords.filter((candidate) => candidate.gold_cluster_id !== record.gold_cluster_id);
    // One deterministic negative per query keeps the scorer workload bounded. This is a
    // sampled negative set, never a full-population false-positive denominator.
    const selectedNegative = otherRecords.length === 0 ? [] : [otherRecords[index % otherRecords.length]];
    queries.push({
      query_id: `syn-query-${String(index + 1).padStart(3, '0')}`,
      query_record_id: record.record_id,
      split: 'held_out_test',
      eligible_match_record_ids: positives.map((member) => member.record_id),
      hard_negative_record_ids: selectedNegative.map((member) => member.record_id),
      case_tags: ['febrl-generated'],
    });
    for (const candidate of [...positives, ...selectedNegative]) {
      // Deduplicate pairs independently of query direction; the query label set still
      // contains the positive peer for every anchor, as required by the evaluator.
      if (record.record_id.localeCompare(candidate.record_id) >= 0) continue;
      const pairKey = [record.record_id, candidate.record_id].sort().join('\0');
      if (labeledPairCandidates.has(pairKey)) continue;
      const samePerson = record.gold_cluster_id === candidate.gold_cluster_id;
      labeledPairCandidates.set(pairKey, {
        left_record_id: record.record_id,
        right_record_id: candidate.record_id,
        samePerson,
      });
    }
  }
  if (queries.length * records.length > MAX_QUERY_RECORD_COMPARISONS) {
    throw new Error(`selected fixture exceeds the ${MAX_QUERY_RECORD_COMPARISONS} query-record comparison limit`);
  }

  const pairCandidates = [...labeledPairCandidates.values()]
    .sort((left, right) => Number(right.samePerson) - Number(left.samePerson)
      || left.left_record_id.localeCompare(right.left_record_id)
      || left.right_record_id.localeCompare(right.right_record_id))
    .slice(0, 999);
  const pairLabels = pairCandidates.map((candidate, index) => ({
    pair_id: `syn-pair-${String(index + 1).padStart(3, '0')}`,
    left_record_id: candidate.left_record_id,
    right_record_id: candidate.right_record_id,
    split: 'held_out_test',
    label: candidate.samePerson ? 'match' : 'non_match',
    case_tags: ['febrl-generated'],
    rationale: 'Deterministic FEBRL generator key establishes synthetic gold identity.',
    label_guide_version: 'febrl-record-key-v1',
    provenance_ref: 'febrl-record-key-ground-truth',
  }));

  return {
    schema_version: '1.0',
    dataset_id: 'febrl-dataset1-subset',
    dataset_status: 'synthetic_fixture',
    purpose: 'mechanics_only',
    provenance: {
      description: `Synthetic English/Australian person records from FEBRL dataset1; exact release supplied by operator; generator-key truth, not human adjudication. source_csv_sha256=${sourceCsvSha256}; source_records=${rows.length}; included_records=${records.length}; included_clusters=${clustersOut.length}; omitted_complete_clusters=${sourceClusterRows.size - clustersOut.length}; unsupported_date_value_count=${unsupportedDateValues}; one deterministic sampled negative per query, not an exhaustive denominator. All included clusters remain in held_out_test; no model or threshold tuning.`,
      contains_real_person_data: false,
      generator: SOURCE,
      seed: 'source archive fixture; original generator seed unavailable here',
      created_at: '2005-07-01',
    },
    splits: ['held_out_test'],
    records,
    clusters: clustersOut,
    pair_labels: pairLabels,
    queries,
  };
}

async function main() {
  const args = process.argv.slice(2);
  if (args.length !== 2) throw new Error(usage());
  const [inputPath, outputPath] = args;
  const bytes = await readFile(inputPath);
  if (bytes.length > MAX_INPUT_BYTES) throw new Error(`CSV exceeds the ${MAX_INPUT_BYTES}-byte safety limit`);
  const rows = parseCsv(bytes.toString('utf8'));
  const dataset = convert(rows, createHash('sha256').update(bytes).digest('hex'));
  await writeFile(outputPath, `${JSON.stringify(dataset, null, 2)}\n`, { flag: 'wx' });
  process.stdout.write(JSON.stringify({
    output: outputPath,
    records: dataset.records.length,
    gold_clusters: dataset.clusters.length,
    labeled_pairs: dataset.pair_labels.length,
    queries: dataset.queries.length,
    source_csv_sha256: `sha256:${createHash('sha256').update(bytes).digest('hex')}`,
  }) + '\n');
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().catch((error) => {
    process.stderr.write(`${error.message}\n`);
    process.exitCode = 1;
  });
}
