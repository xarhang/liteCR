// Deterministic synthetic linkage stress corpus. Labels and patterns are
// generated together; this is a mechanics fixture, not population evidence.
import { mkdir, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const repo = path.resolve(here, '../..');
const defaultOutput = path.join(repo, 'fixtures/matching/synthetic-realistic-v1.json');
const families = [
  'common-name', 'shared-household', 'twin', 'single-character-error',
  'partial-date', 'cross-script', 'identifier-collision',
];
const splits = ['train', 'development', 'held_out_test'];
const perFamilyPerSplit = 14;
const seed = 20261014;

const lexicons = {
  lo: {
    language: 'lo-LA', script: 'Laoo',
    given: ['ສີດາ', 'ຄຳພອນ', 'ບຸນມີ', 'ວຽງໄຊ', 'ດາລາ', 'ຈັນທະວີ'],
    family: ['ໄຊຍະວົງ', 'ພົມມະວົງ', 'ແກ້ວວິໄລ', 'ສຸວັນນະວົງ', 'ວົງສຸລິ'],
    replacement: 'ກ', latin: ['Sida Xayavong', 'Khamphon Phommavong', 'Bounmy Keovilay', 'Viengxay Souvannavong'],
  },
  th: {
    language: 'th-TH', script: 'Thai',
    given: ['สมชาย', 'สุภาวดี', 'กมล', 'อรทัย', 'ประเสริฐ', 'วรรณา'],
    family: ['ใจดี', 'ศรีสุข', 'แสงทอง', 'วงศ์คำ', 'บุญมี'],
    replacement: 'ก', latin: ['Somchai Jaidee', 'Supawadee Srisuk', 'Kamon Saengthong', 'Orathai Wongkham'],
  },
  vi: {
    language: 'vi-VN', script: 'Latn',
    given: ['Nguyễn Ánh', 'Trần Minh', 'Lê Thu', 'Phạm Huy', 'Võ Lan', 'Đặng Mai'],
    family: ['An', 'Bình', 'Châu', 'Dũng', 'Hạnh'],
    replacement: 'n', latin: ['Nguyen Anh An', 'Tran Minh Binh', 'Le Thu Chau', 'Pham Huy Dung'],
  },
  hi: {
    language: 'hi-IN', script: 'Deva',
    given: ['किरण', 'आरती', 'विजय', 'सुनीता', 'अनिल', 'मीना'],
    family: ['शर्मा', 'वर्मा', 'पटेल', 'सिंह', 'जोशी'],
    replacement: 'क', latin: ['Kiran Sharma', 'Aarti Verma', 'Vijay Patel', 'Sunita Singh'],
  },
  km: {
    language: 'km-KH', script: 'Khmr',
    given: ['សុភា', 'ដារ៉ា', 'វិសាល', 'ចន្ទា', 'មាលា', 'សុខា'],
    family: ['សុខ', 'ចាន់', 'គង់', 'លី', 'ហេង'],
    replacement: 'ក', latin: ['Sophea Sok', 'Dara Chan', 'Visal Kong', 'Chanta Ly'],
  },
  ar: {
    language: 'ar', script: 'Arab',
    given: ['نور', 'أمين', 'ليلى', 'سليم', 'مريم', 'حسن'],
    family: ['حسن', 'علي', 'كريم', 'منصور', 'يوسف'],
    replacement: 'م', latin: ['Nour Hassan', 'Amin Ali', 'Layla Karim', 'Salim Mansour'],
  },
  en: {
    language: 'en-GB', script: 'Latn',
    given: ['Alex', 'Robin', 'Sam', 'Taylor', 'Morgan', 'Casey'],
    family: ['Example', 'Sample', 'Fiction', 'Demo', 'Test'],
    replacement: 'n', latin: ['Alex Example', 'Robin Sample', 'Sam Fiction', 'Taylor Demo'],
  },
};
const familyLocale = {
  'common-name': 'th', 'shared-household': 'lo', twin: 'vi',
  'single-character-error': 'hi', 'partial-date': 'km',
  'identifier-collision': 'ar',
};
const nativeAliases = {
  lo: ['ສີດາ ໄຊຍະວົງ', 'ຄຳພອນ ພົມມະວົງ', 'ບຸນມີ ແກ້ວວິໄລ', 'ວຽງໄຊ ສຸວັນນະວົງ'],
  th: ['สมชาย ใจดี', 'สุภาวดี ศรีสุข', 'กมล แสงทอง', 'อรทัย วงศ์คำ'],
  hi: ['किरण शर्मा', 'आरती वर्मा', 'विजय पटेल', 'सुनीता सिंह'],
  km: ['សុភា សុខ', 'ដារ៉ា ចាន់', 'វិសាល គង់', 'ចន្ទា លី'],
  ar: ['نور حسن', 'أمين علي', 'ليلى كريم', 'سليم منصور'],
};

let state = seed >>> 0;
function random() {
  state ^= state << 13; state ^= state >>> 17; state ^= state << 5;
  return (state >>> 0) / 0x1_0000_0000;
}
function choose(items, index) { return items[index % items.length]; }
function makeName(locale, index) {
  return `${choose(locale.given, index)} ${choose(locale.family, index * 3)}`;
}
function typo(name, replacement) {
  const chars = Array.from(name);
  const at = chars.findLastIndex((char) => !/\s/u.test(char));
  if (at >= 0) chars[at] = replacement;
  return chars.join('');
}
function dateParts(index) {
  const year = 1965 + (index % 46);
  const month = 1 + ((index * 7) % 12);
  const day = 1 + ((index * 11) % 28);
  return { year, month: String(month).padStart(2, '0'), day: String(day).padStart(2, '0') };
}
function dob(value, precision) {
  if (value === null) return null;
  return { original_value: value, precision, calendar: 'gregorian', estimated: false };
}
function nameAssertion(value, locale, representation = 'source_original', version = 'synthetic-name-v1') {
  return {
    original_value: value, script: locale.script, language: locale.language,
    representation, normalization_version: version,
  };
}
function recordId(number) { return `syn-rec-${String(number).padStart(3, '0')}`; }
function personId(number) { return `syn-person-${String(number).padStart(3, '0')}`; }
function splitFor(position) {
  const initial = splits[Math.floor(position / perFamilyPerSplit)];
  return initial;
}
function candidateNegativeIndex(position, familyOffset) {
  // Adjacent records in the same family/split deliberately form hard-negative
  // controls (shared names/contact, twin lookalikes, or ID collisions).
  const local = position - familyOffset;
  const splitBase = Math.floor(local / perFamilyPerSplit) * perFamilyPerSplit;
  const within = local % perFamilyPerSplit;
  return familyOffset + splitBase + (within ^ 1);
}

const args = process.argv.slice(2);
let output = defaultOutput;
let onlyFamily = null;
for (const arg of args) {
  if (arg.startsWith('--out=')) output = path.resolve(arg.slice('--out='.length));
  else if (arg.startsWith('--only-family=')) onlyFamily = arg.slice('--only-family='.length);
  else throw new Error(`Unknown argument: ${arg}`);
}
if (onlyFamily && !families.includes(onlyFamily)) throw new Error(`Unknown family: ${onlyFamily}`);

const records = [];
const clusters = [];
const pairLabels = [];
const queries = [];
let clusterNumber = 0;
let pairNumber = 0;
let queryNumber = 0;
let familyOffset = 0;
for (const family of families) {
  for (let local = 0; local < perFamilyPerSplit * splits.length; local += 1) {
    const cluster = clusterNumber++;
    const split = splitFor(local);
    const localeKey = family === 'cross-script'
      ? Object.keys(nativeAliases)[local % 5]
      : familyLocale[family];
    const locale = lexicons[localeKey];
    const sourceOneId = recordId(cluster * 2 + 1);
    const sourceTwoId = recordId(cluster * 2 + 2);
    const gold = personId(cluster + 1);
    const date = dateParts(cluster + Math.floor(random() * 17));
    const twinIndex = Math.floor(local / 2);
    const nameIndex = ['twin', 'common-name', 'partial-date'].includes(family) ? twinIndex : local;
    const sourceOneName = family === 'cross-script'
      ? nativeAliases[localeKey][local % 4] : makeName(locale, nameIndex);
    let sourceTwoName = sourceOneName;
    if (family === 'single-character-error') sourceTwoName = typo(sourceOneName, locale.replacement);
    if (family === 'cross-script') sourceTwoName = locale.latin[local % locale.latin.length];

    let sourceOneDob = dob(`${date.year}-${date.month}-${date.day}`, 'day');
    let sourceTwoDob = dob(`${date.year}-${date.month}-${date.day}`, 'day');
    if (family === 'partial-date') sourceOneDob = dob(`${date.year}-${date.month}`, 'month');
    if (family === 'single-character-error' && local % 2 === 0) {
      sourceOneDob = null;
      sourceTwoDob = null;
    }
    if (family === 'cross-script') {
      sourceOneDob = local % 2 === 0 ? null : dob(String(date.year), 'year');
      sourceTwoDob = local % 2 === 0 ? null : dob(String(date.year), 'year');
    }

    const contacts = family === 'shared-household' || family === 'twin'
      ? [{ kind: 'phone', synthetic_token: `SYNTHETIC-PHONE-HH-${family.toUpperCase()}-${Math.floor(local / 2)}`, shared_or_recycled_possible: true }]
      : [];
    const address = family === 'shared-household' || family === 'twin'
      ? `SYNTHETIC-ADDRESS-HH-${family.toUpperCase()}-${Math.floor(local / 2)}`
      : null;
    const forceTwin = family === 'twin';
    if (forceTwin) {
      sourceOneDob = dob(`1990-06-${String(10 + twinIndex).padStart(2, '0')}`, 'day');
      sourceTwoDob = sourceOneDob;
    }
    const tags = [family, 'synthetic-stress-case'];
    const firstNames = [nameAssertion(sourceOneName, locale)];
    const secondNames = [nameAssertion(sourceTwoName,
      family === 'cross-script' ? { script: 'Latn', language: locale.language } : locale)];

    const identifierValue = family === 'identifier-collision'
      ? `SYNTHETIC-ID-COLLISION-${String(Math.floor(local / 2)).padStart(2, '0')}`
      : `SYNTHETIC-ID-${String(cluster + 1).padStart(4, '0')}`;
    const idStatus = family === 'identifier-collision' ? 'conflicting' : 'verified';
    const identifiers = [{
      authority: `urn:litecr:synthetic:authority:${family === 'identifier-collision' ? 'regional-registry' : 'source-registry'}`,
      type: 'synthetic-person-id', original_value: identifierValue, status: idStatus,
    }];

    for (const [record, recordDob, names] of [
      [sourceOneId, sourceOneDob, firstNames],
      [sourceTwoId, sourceTwoDob, secondNames],
    ]) {
      records.push({
        record_id: record,
        source_system: `urn:litecr:synthetic:source:${record === sourceOneId ? 'clinic-a' : 'clinic-b'}`,
        gold_cluster_id: gold, split, name_assertions: names, date_of_birth: recordDob,
        identifiers: family === 'identifier-collision' ? identifiers : [],
        contacts, address_token: address, case_tags: tags,
      });
    }
    clusters.push({
      gold_cluster_id: gold, split, member_record_ids: [sourceOneId, sourceTwoId],
      label_status: 'synthetic_asserted', provenance_ref: `generated:${family}:seed-${seed}`,
    });
    pairNumber += 1;
    pairLabels.push({
      pair_id: `syn-pair-${String(pairNumber).padStart(3, '0')}`,
      left_record_id: sourceOneId, right_record_id: sourceTwoId, split,
      label: 'match', case_tags: tags, rationale: `Generated source observations assigned to one synthetic identity in the ${family} stress family.`,
      label_guide_version: 'synthetic-guide-v1', provenance_ref: `generated:${family}:seed-${seed}`,
    });
    queryNumber += 1;
    const negativeCluster = candidateNegativeIndex(cluster, familyOffset);
    const negativeRecordId = recordId(negativeCluster * 2 + 1);
    queries.push({
      query_id: `syn-query-${String(queryNumber).padStart(3, '0')}`,
      query_record_id: sourceOneId, split, eligible_match_record_ids: [sourceTwoId],
      hard_negative_record_ids: [negativeRecordId], case_tags: tags,
    });
    pairNumber += 1;
    pairLabels.push({
      pair_id: `syn-pair-${String(pairNumber).padStart(3, '0')}`,
      left_record_id: sourceOneId, right_record_id: negativeRecordId, split,
      label: 'non_match', case_tags: [family, 'synthetic-hard-negative'],
      rationale: `Different generated identity used as a controlled hard-negative for the ${family} stress family.`,
      label_guide_version: 'synthetic-guide-v1', provenance_ref: `generated:${family}:seed-${seed}`,
    });
  }
  familyOffset += perFamilyPerSplit * splits.length;
}

const dataset = {
  schema_version: '1.0', dataset_id: 'litecr-realistic-synthetic-stress-v1',
  dataset_status: 'synthetic_fixture', purpose: 'mechanics_only',
  provenance: {
    description: 'Deterministically generated cross-source identity-linkage stress corpus with balanced failure-mode families; names and tokens are invented/generated and no real records are included. Family proportions are deliberate test controls, not prevalence estimates.',
    contains_real_person_data: false,
    generator: 'tools/evaluation/generate_synthetic_cohort.mjs', seed,
    created_at: '2026-10-14',
  },
  splits, records, clusters,
  pair_labels: onlyFamily ? pairLabels.filter((row) => row.case_tags.includes(onlyFamily)) : pairLabels,
  queries: onlyFamily ? queries.filter((row) => row.case_tags.includes(onlyFamily)) : queries,
};
await mkdir(path.dirname(output), { recursive: true });
await writeFile(output, `${JSON.stringify(dataset, null, 2)}\n`, 'utf8');
console.log(JSON.stringify({ output, family_filter: onlyFamily, seed, records: records.length, clusters: clusters.length, pairs: dataset.pair_labels.length, queries: dataset.queries.length, held_out_records: records.filter((row) => row.split === 'held_out_test').length, held_out_queries: dataset.queries.filter((row) => row.split === 'held_out_test').length }, null, 2));
