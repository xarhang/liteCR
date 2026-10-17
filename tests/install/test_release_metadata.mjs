import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdtemp, mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const generator = path.join(root, 'tools/install/write-release-metadata.mjs');
const outputs = [
  'cargo.json',
  'npm.json',
  'notices.md',
  'release.json',
  'sbom.spdx.json',
];

async function makeFixture(base, { applicationLicense = 'UNLICENSED' } = {}) {
  await mkdir(base, { recursive: true });
  const cargoPath = path.join(base, 'cargo-metadata.json');
  const cargoLockPath = path.join(base, 'Cargo.lock');
  const npmPath = path.join(base, 'package-lock.json');
  const versionPath = path.join(base, 'version.json');
  const cargoChecksum = createHash('sha256').update('synthetic-alpha').digest('hex');
  const rEfiChecksum = createHash('sha256').update('synthetic-r-efi').digest('hex');
  await writeFile(cargoPath, JSON.stringify({ packages: [
    { name: 'litecr-server', version: '1.2.3', license: applicationLicense },
    { name: 'alpha-dep', version: '2.0.0', license: 'MIT', source: 'registry+https://example.invalid' },
    { name: 'r-efi', version: '6.0.0', license: 'MIT OR Apache-2.0 OR LGPL-2.1-or-later', source: 'registry+https://example.invalid' },
    { name: 'legacy-dep', version: '3.0.0', license: 'Apache-2.0 / MIT', source: 'registry+https://example.invalid' },
    { name: 'ff', version: '0.13.1', license: 'MIT/Apache-2.0' },
    { name: 'fnv', version: '1.0.7', license: 'Apache-2.0 / MIT' },
    { name: 'ident_case', version: '1.0.1', license: 'MIT/Apache-2.0' },
    { name: 'num-bigint-dig', version: '0.8.6', license: 'MIT/Apache-2.0' },
    { name: 'plain', version: '0.2.3', license: 'MIT/Apache-2.0' },
    { name: 'serde_plain', version: '1.0.2', license: 'MIT/Apache-2.0' },
    { name: 'unicode-properties', version: '0.1.4', license: 'MIT/Apache-2.0' },
    { name: 'vcpkg', version: '0.2.15', license: 'MIT/Apache-2.0' },
    { name: 'ff', version: '0.13.2', license: 'MIT/Apache-2.0' },
  ] }));
  await writeFile(cargoLockPath, [
    'version = 4',
    '',
    '[[package]]',
    'name = "alpha-dep"',
    'version = "2.0.0"',
    'source = "registry+https://example.invalid"',
    `checksum = "${cargoChecksum}"`,
    '',
    '[[package]]',
    'name = "r-efi"',
    'version = "6.0.0"',
    'source = "registry+https://example.invalid"',
    `checksum = "${rEfiChecksum}"`,
    '',
    '[[package]]',
    'name = "legacy-dep"',
    'version = "3.0.0"',
    'source = "registry+https://example.invalid"',
    `checksum = "${'b'.repeat(64)}"`,
    '',
  ].join('\n'));
  await writeFile(npmPath, JSON.stringify({ packages: {
    '': { name: 'litecr-web', version: '1.2.3' },
    'node_modules/zeta-dep': {
      name: 'zeta-dep', version: '4.5.6', license: 'Apache-2.0',
      integrity: `sha512-${createHash('sha512').update('synthetic-zeta').digest('base64')}`,
    },
    'node_modules/@napi-rs/lzma-linux-x64-gnu': {
      name: '@napi-rs/lzma-linux-x64-gnu', version: '1.5.1', license: 'MIT',
      integrity: `sha512-${createHash('sha512').update('synthetic-lzma').digest('base64')}`,
      dev: true, optional: true, os: ['linux'], cpu: ['x64'], libc: ['glibc'],
    },
  } }));
  await writeFile(versionPath, JSON.stringify({ application_version: '1.2.3', schema_version: 9 }));
  return { cargoPath, cargoLockPath, cargoChecksum, npmPath, versionPath };
}

async function createGitRevision(base, t) {
  if (spawnSync('git', ['--version'], { encoding: 'utf8' }).status !== 0) {
    t.skip('Git is needed to create a genuine fixture commit ID');
    return null;
  }
  await mkdir(base, { recursive: true });
  await writeFile(path.join(base, 'source.txt'), 'synthetic source fixture\n');
  const commands = [
    ['init', '--quiet'],
    ['add', 'source.txt'],
    ['-c', 'user.name=liteCR test', '-c', 'user.email=litecr-test@example.invalid',
      '-c', 'commit.gpgsign=false', 'commit', '--quiet', '-m', 'synthetic fixture'],
  ];
  for (const args of commands) {
    const result = spawnSync('git', args, { cwd: base, encoding: 'utf8' });
    assert.equal(result.status, 0, result.stderr);
  }
  const revision = spawnSync('git', ['rev-parse', '--verify', 'HEAD^{commit}'], {
    cwd: base,
    encoding: 'utf8',
  });
  assert.equal(revision.status, 0, revision.stderr);
  return revision.stdout.trim();
}

function invoke(inputs, outDir, epoch, sourceRevision, sourceRevisionStatus) {
  const outPaths = outputs.map((file) => path.join(outDir, file));
  const env = { ...process.env };
  if (epoch === undefined) delete env.SOURCE_DATE_EPOCH;
  else env.SOURCE_DATE_EPOCH = epoch;
  delete env.LITECR_SOURCE_REVISION;
  delete env.LITECR_SOURCE_REVISION_STATUS;
  if (sourceRevision !== undefined) env.LITECR_SOURCE_REVISION = sourceRevision;
  if (sourceRevisionStatus !== undefined) env.LITECR_SOURCE_REVISION_STATUS = sourceRevisionStatus;
  const result = spawnSync(process.execPath, [
    generator,
    inputs.cargoPath,
    inputs.cargoLockPath,
    inputs.npmPath,
    inputs.versionPath,
    ...outPaths,
  ], {
    encoding: 'utf8',
    env,
  });
  return { ...result, outPaths };
}

test('metadata and SPDX are byte-reproducible for identical release inputs', async (t) => {
  const temp = await mkdtemp(path.join(os.tmpdir(), 'litecr-release-meta-'));
  t.after(() => rm(temp, { recursive: true, force: true }));
  const inputs = await makeFixture(path.join(temp, 'inputs'));
  const firstDir = path.join(temp, 'first');
  const secondDir = path.join(temp, 'second');
  await mkdir(firstDir);
  await mkdir(secondDir);
  const first = invoke(inputs, firstDir, '1760000000');
  const second = invoke(inputs, secondDir, '1760000000');
  assert.equal(first.status, 0, first.stderr);
  assert.equal(second.status, 0, second.stderr);

  const firstFiles = await Promise.all(first.outPaths.map((file) => readFile(file)));
  const secondFiles = await Promise.all(second.outPaths.map((file) => readFile(file)));
  for (let index = 0; index < outputs.length; index += 1) {
    assert.deepEqual(firstFiles[index], secondFiles[index], outputs[index]);
  }

  const release = JSON.parse(firstFiles[3]);
  const npmPackages = JSON.parse(firstFiles[1]);
  const sbom = JSON.parse(firstFiles[4]);
  assert.equal(release.project_license, 'UNLICENSED');
  assert.equal(release.distribution_status, 'blocked_unlicensed');
  assert.match(sbom.documentNamespace, /^https:\/\/litecr\.invalid\/spdx\/sha256-[a-f0-9]{64}$/);
  assert.equal(sbom.creationInfo.created, '2025-10-09T08:53:20.000Z');
  assert.match(firstFiles[2].toString(), /Distribution status: blocked_unlicensed/);
  assert.equal(release.source_revision, null);
  assert.equal(release.source_revision_status, 'no_git_metadata');
  const zeta = sbom.packages.find((pkg) => pkg.name === 'zeta-dep');
  assert.deepEqual(zeta.checksums, [{
    algorithm: 'SHA512',
    checksumValue: createHash('sha512').update('synthetic-zeta').digest('hex'),
  }]);
  assert.deepEqual(sbom.relationships, [{
    spdxElementId: 'SPDXRef-DOCUMENT',
    relationshipType: 'DESCRIBES',
    relatedSpdxElement: 'SPDXRef-Package-1',
  }]);
  const lzmaNpm = npmPackages.find((pkg) => pkg.name === '@napi-rs/lzma-linux-x64-gnu');
  assert.deepEqual(lzmaNpm.lockScope, {
    dev: true,
    optional: true,
    devOptional: null,
    os: ['linux'],
    cpu: ['x64'],
    libc: ['glibc'],
  });
  const lzmaSpdx = sbom.packages.find((pkg) => pkg.name === '@napi-rs/lzma-linux-x64-gnu');
  assert.match(lzmaSpdx.packageComment, /not a runtime inclusion determination/);
  assert.match(lzmaSpdx.packageComment, /dev=true, optional=true, os=linux, cpu=x64, libc=glibc/);
  assert.match(firstFiles[2].toString(), /@napi-rs\/lzma-linux-x64-gnu 1\.5\.1 — MIT \(npm lock: dev=true, optional=true, os=linux, cpu=x64, libc=glibc\)/);
  assert.match(firstFiles[2].toString(), /full npm lockfile inventory, including development, test, optional, and platform-specific packages/);
  const alpha = sbom.packages.find((pkg) => pkg.name === 'alpha-dep');
  assert.deepEqual(alpha.checksums, [{ algorithm: 'SHA256', checksumValue: inputs.cargoChecksum }]);
  assert.equal(alpha.licenseDeclared, 'MIT');
  const rEfi = sbom.packages.find((pkg) => pkg.name === 'r-efi' && pkg.versionInfo === '6.0.0');
  assert.deepEqual(rEfi.checksums, [{ algorithm: 'SHA256', checksumValue: createHash('sha256').update('synthetic-r-efi').digest('hex') }]);
  assert.equal(rEfi.licenseDeclared, 'MIT OR Apache-2.0 OR LGPL-2.1-or-later');
  assert.equal(rEfi.licenseComments, undefined);
  const legacy = sbom.packages.find((pkg) => pkg.name === 'legacy-dep');
  assert.equal(legacy.licenseDeclared, 'NOASSERTION');
  assert.match(legacy.licenseComments, /Apache-2\.0 \/ MIT/);
  const verifiedSlash = sbom.packages.find((pkg) => pkg.name === 'ff' && pkg.versionInfo === '0.13.1');
  assert.equal(verifiedSlash.licenseDeclared, 'MIT OR Apache-2.0');
  assert.equal(verifiedSlash.licenseComments, undefined);
  for (const [name, version] of [
    ['ident_case', '1.0.1'],
    ['num-bigint-dig', '0.8.6'],
    ['unicode-properties', '0.1.4'],
  ]) {
    const verified = sbom.packages.find((pkg) => pkg.name === name && pkg.versionInfo === version);
    assert.equal(verified.licenseDeclared, 'MIT OR Apache-2.0', `${name} ${version}`);
    assert.equal(verified.licenseComments, undefined, `${name} ${version}`);
  }
  for (const [name, version, license] of [
    ['fnv', '1.0.7', 'Apache-2.0 OR MIT'],
    ['plain', '0.2.3', 'MIT OR Apache-2.0'],
    ['serde_plain', '1.0.2', 'MIT OR Apache-2.0'],
    ['vcpkg', '0.2.15', 'MIT OR Apache-2.0'],
  ]) {
    const verified = sbom.packages.find((pkg) => pkg.name === name && pkg.versionInfo === version);
    assert.equal(verified.licenseDeclared, license, `${name} ${version}`);
    assert.equal(verified.licenseComments, undefined, `${name} ${version}`);
  }
  const unverifiedVersion = sbom.packages.find((pkg) => pkg.name === 'ff' && pkg.versionInfo === '0.13.2');
  assert.equal(unverifiedVersion.licenseDeclared, 'NOASSERTION');
  const application = sbom.packages.find((pkg) => pkg.name === 'litecr-server');
  assert.equal(application.licenseDeclared, 'NOASSERTION');
  assert.match(application.licenseComments, /UNLICENSED/);
});

test('declared AGPLv3-or-later is emitted as review-required, not unlicensed', async (t) => {
  const temp = await mkdtemp(path.join(os.tmpdir(), 'litecr-release-agpl-'));
  t.after(() => rm(temp, { recursive: true, force: true }));
  const inputs = await makeFixture(path.join(temp, 'inputs'), {
    applicationLicense: 'AGPL-3.0-or-later',
  });
  const outDir = path.join(temp, 'output');
  await mkdir(outDir);
  const result = invoke(inputs, outDir, '1760000000');
  assert.equal(result.status, 0, result.stderr);

  const release = JSON.parse(await readFile(path.join(outDir, 'release.json'), 'utf8'));
  const sbom = JSON.parse(await readFile(path.join(outDir, 'sbom.spdx.json'), 'utf8'));
  const notices = await readFile(path.join(outDir, 'notices.md'), 'utf8');
  assert.equal(release.project_license, 'AGPL-3.0-or-later');
  assert.equal(release.distribution_status, 'review_required');
  const application = sbom.packages.find((pkg) => pkg.name === 'litecr-server');
  assert.equal(application.licenseDeclared, 'AGPL-3.0-or-later');
  assert.match(notices, /Project license: AGPL-3\.0-or-later/);
  assert.match(notices, /Distribution status: review_required/);
});

test('metadata generation fails closed when a registry dependency lacks its Cargo.lock checksum', async (t) => {
  const temp = await mkdtemp(path.join(os.tmpdir(), 'litecr-release-meta-'));
  t.after(() => rm(temp, { recursive: true, force: true }));
  const inputs = await makeFixture(path.join(temp, 'inputs'));
  const lockText = await readFile(inputs.cargoLockPath, 'utf8');
  await writeFile(inputs.cargoLockPath, lockText.replace(/\nchecksum = "[a-f0-9]{64}"/, ''));
  const outDir = path.join(temp, 'out');
  await mkdir(outDir);
  const result = invoke(inputs, outDir, '1760000000');
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /Cargo\.lock is missing a valid registry checksum for alpha-dep 2\.0\.0/);
});

test('metadata pins only a full revision from a clean Git worktree', async (t) => {
  const temp = await mkdtemp(path.join(os.tmpdir(), 'litecr-release-meta-'));
  t.after(() => rm(temp, { recursive: true, force: true }));
  const inputs = await makeFixture(path.join(temp, 'inputs'));
  const revision = await createGitRevision(path.join(temp, 'git-source'), t);
  if (revision === null) return;
  const outDir = path.join(temp, 'out');
  const noRevisionDir = path.join(temp, 'without-revision');
  await mkdir(outDir);
  await mkdir(noRevisionDir);
  const result = invoke(inputs, outDir, '1760000000', revision, 'clean_git_worktree');
  const noRevision = invoke(inputs, noRevisionDir, '1760000000');
  assert.equal(result.status, 0, result.stderr);
  assert.equal(noRevision.status, 0, noRevision.stderr);
  const release = JSON.parse(await readFile(result.outPaths[3], 'utf8'));
  const pinnedSbom = JSON.parse(await readFile(result.outPaths[4], 'utf8'));
  const unpinnedSbom = JSON.parse(await readFile(noRevision.outPaths[4], 'utf8'));
  assert.equal(release.source_revision, revision);
  assert.equal(release.source_revision_status, 'clean_git_worktree');
  assert.notEqual(pinnedSbom.documentNamespace, unpinnedSbom.documentNamespace);
});

test('metadata rejects dirty-tree revision claims and malformed object IDs', async (t) => {
  const temp = await mkdtemp(path.join(os.tmpdir(), 'litecr-release-meta-'));
  t.after(() => rm(temp, { recursive: true, force: true }));
  const inputs = await makeFixture(path.join(temp, 'inputs'));
  const revision = await createGitRevision(path.join(temp, 'git-source'), t);
  if (revision === null) return;
  const dirtyDir = path.join(temp, 'dirty');
  const malformedDir = path.join(temp, 'malformed');
  await mkdir(dirtyDir);
  await mkdir(malformedDir);
  const dirty = invoke(inputs, dirtyDir, '1760000000', revision, 'dirty_git_worktree');
  const malformed = invoke(inputs, malformedDir, '1760000000', 'not-a-git-revision', 'clean_git_worktree');
  assert.notEqual(dirty.status, 0);
  assert.match(dirty.stderr, /source revision is recorded only for a clean Git worktree/);
  assert.notEqual(malformed.status, 0);
  assert.match(malformed.stderr, /full lowercase Git object ID/);
});

test('metadata generation requires an explicit SOURCE_DATE_EPOCH', async (t) => {
  const temp = await mkdtemp(path.join(os.tmpdir(), 'litecr-release-meta-'));
  t.after(() => rm(temp, { recursive: true, force: true }));
  const inputs = await makeFixture(path.join(temp, 'inputs'));
  const outDir = path.join(temp, 'out');
  await mkdir(outDir);
  const result = invoke(inputs, outDir, undefined);
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /SOURCE_DATE_EPOCH must be set/);
});
