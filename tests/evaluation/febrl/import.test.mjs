import test from 'node:test';
import assert from 'node:assert/strict';
import { convert, parseCsv } from '../../../tools/evaluation/febrl/import.mjs';

test('FEBRL CSV parser preserves quoted commas and embedded newlines', () => {
  const rows = parseCsv('rec_id,given_name,surname\r\nrec-1-org,"A, B","Line 1\nLine 2"\r\n');
  assert.deepEqual(rows, [{ rec_id: 'rec-1-org', given_name: 'A, B', surname: 'Line 1\nLine 2' }]);
});

test('FEBRL CSV parser rejects quotes inside unquoted fields and text after a closing quote', () => {
  assert.throws(() => parseCsv('name\nAlex"andra\n'), /quote inside an unquoted CSV field/);
  assert.throws(() => parseCsv('name\n"Alex"andra\n'), /unexpected character after a quoted CSV field/);
  assert.throws(() => parseCsv('name\n"Alex" \n'), /unexpected character after a quoted CSV field/);
});

test('FEBRL importer builds label-blind fixture IDs and exhaustive positive truth', () => {
  const rows = [
    { rec_id: 'rec-1-org', given_name: 'Alex', surname: 'Same', date_of_birth: '19800101' },
    { rec_id: 'rec-1-dup-0', given_name: 'Alec', surname: 'Same', date_of_birth: '1980-01-01' },
    { rec_id: 'rec-2-org', given_name: 'Casey', surname: 'Else', date_of_birth: '' },
    { rec_id: 'rec-2-dup-0', given_name: 'Casey', surname: 'Else', date_of_birth: '' },
  ];
  const dataset = convert(rows, '0123456789abcdef');
  assert.equal(dataset.dataset_status, 'synthetic_fixture');
  assert.equal(dataset.purpose, 'mechanics_only');
  assert.equal(dataset.records.length, 4);
  assert.deepEqual(dataset.records.map(({ record_id }) => record_id), [
    'syn-rec-001', 'syn-rec-002', 'syn-rec-003', 'syn-rec-004',
  ]);
  assert.ok(dataset.records.every(({ record_id, gold_cluster_id }) => !record_id.includes(gold_cluster_id)));
  assert.ok(dataset.records.every(({ split }) => split === 'held_out_test'));
  assert.deepEqual(dataset.queries[0].eligible_match_record_ids, ['syn-rec-002']);
  assert.equal(dataset.queries.length, 4);
  assert.equal(dataset.pair_labels.filter(({ label }) => label === 'match').length, 2);
  assert.ok(dataset.pair_labels.some(({ label }) => label === 'non_match'));
  assert.equal(dataset.records[0].date_of_birth.original_value, '1980-01-01');
  assert.equal(dataset.records[2].date_of_birth, null);
  assert.ok(dataset.pair_labels.every(({ pair_id }) => /^syn-pair-\d{3}$/.test(pair_id)));
  assert.ok(dataset.queries.every(({ query_id }) => /^syn-query-\d{3}$/.test(query_id)));
  assert.equal(dataset.provenance.contains_real_person_data, false);
});

test('FEBRL importer rejects malformed identities, duplicate IDs, and non-FEBRL columns', () => {
  assert.throws(() => convert([{ rec_id: 'record-x', given_name: 'A', surname: 'B' }], 'sha'), /unsupported FEBRL record identifier/);
  assert.throws(() => convert([
    { rec_id: 'rec-1-org', given_name: 'A', surname: 'B' },
    { rec_id: 'rec-1-org', given_name: 'A', surname: 'B' },
  ], 'sha'), /empty or duplicate rec_id/);
  assert.throws(() => convert([{ rec_id: 'rec-1-org', given_name: '', surname: '' }], 'sha'), /no given_name or surname/);
});

test('FEBRL importer rejects impossible Gregorian dates and preserves valid leap days', () => {
  const rows = [
    { rec_id: 'rec-1-org', given_name: 'Alex', surname: 'Same', date_of_birth: '2000-02-29' },
    { rec_id: 'rec-1-dup-0', given_name: 'Alec', surname: 'Same', date_of_birth: '1900-02-29' },
    { rec_id: 'rec-2-org', given_name: 'Casey', surname: 'Else', date_of_birth: '2024-04-31' },
    { rec_id: 'rec-2-dup-0', given_name: 'Casey', surname: 'Else', date_of_birth: '2024-04-30' },
  ];
  const dataset = convert(rows, '0123456789abcdef');
  assert.equal(dataset.records[0].date_of_birth.original_value, '2000-02-29');
  assert.equal(dataset.records[1].date_of_birth, null);
  assert.equal(dataset.records[2].date_of_birth, null);
  assert.equal(dataset.records[3].date_of_birth.original_value, '2024-04-30');
  assert.match(dataset.provenance.description, /unsupported_date_value_count=2/);
});

test('official FEBRL 1,000-row shape stays within fixture ID and evaluator workload limits', () => {
  const rows = Array.from({ length: 500 }, (_, index) => [
    { rec_id: `rec-${index}-org`, given_name: `Given${index}`, surname: `Surname${index}` },
    { rec_id: `rec-${index}-dup-0`, given_name: `Given${index}`, surname: `Surname${index}` },
  ]).flat();
  const dataset = convert(rows, 'test-digest');
  assert.equal(dataset.records.length, 316);
  assert.equal(dataset.clusters.length, 158);
  assert.equal(dataset.queries.length, 316);
  assert.equal(dataset.queries.length * dataset.records.length, 99_856);
  assert.equal(dataset.pair_labels.filter(({ label }) => label === 'match').length, 158);
  assert.ok(dataset.pair_labels.length <= 999);
  assert.ok(dataset.records.every(({ record_id }) => /^syn-rec-[0-9]{3}$/.test(record_id)));
  assert.ok(dataset.clusters.every(({ gold_cluster_id }) => /^syn-person-[0-9]{3}$/.test(gold_cluster_id)));
  assert.ok(dataset.queries.every(({ query_id }) => /^syn-query-[0-9]{3}$/.test(query_id)));
  assert.ok(dataset.pair_labels.every(({ pair_id }) => /^syn-pair-[0-9]{3}$/.test(pair_id)));
  assert.ok(dataset.queries.every(({ eligible_match_record_ids }) => eligible_match_record_ids.length === 1));
});
