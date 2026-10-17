import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { appendFile, cp, copyFile, mkdir, mkdtemp, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { gzipSync } from 'node:zlib';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

const repo = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const collector = path.join(repo, 'tools/install/collect-third-party-texts.mjs');
const registry = 'registry+https://github.com/rust-lang/crates.io-index';
let pinnedScenarioCounter = 0;

test('collects exact locked dependency text files reproducibly and reports missing/ambiguous sources', async (t) => {
  const temp = await mkdtemp(path.join(os.tmpdir(), 'litecr-third-party-texts-'));
  t.after(() => rm(temp, { recursive: true, force: true }));
  const cargoHome = path.join(temp, 'cargo-home');
  const cargoSrc = path.join(cargoHome, 'registry', 'src');
  const cargoCache = path.join(cargoHome, 'registry', 'cache', 'index-one');
  const npmModules = path.join(temp, 'npm', 'node_modules');
  const npmCache = path.join(temp, 'npm-cache');
  await mkdir(cargoSrc, { recursive: true });
  await mkdir(cargoCache, { recursive: true });
  await mkdir(npmModules, { recursive: true });
  await mkdir(npmCache, { recursive: true });

  const exactArchive = gzipSync(createTar({
    'package/LICENSE': 'Exact MIT text from the synthetic fixture.\n',
    'package/licenses/NOTICE.md': 'Exact notice text from the synthetic fixture.\n',
    'package/LICENSES/Apache-2.0.txt': 'Exact Apache license text from the synthetic fixture.\n',
    'package/LICENSES/MIT.txt': 'Exact MIT license text from the synthetic fixture.\n',
    'package/LICENSES/README.txt': 'Not a license text and should be ignored.\n',
  }), { mtime: 0 });
  const authorsLicenseText = [
    'LICENSE:',
    'This project is triple-licensed under MIT, Apache-2.0 and LGPL-2.1-or-later.',
    '',
    'AUTHORS-MIT:',
    'MIT license terms are reproduced here.',
    '',
    'AUTHORS-ASL:',
    'Apache license notice is reproduced here.',
    '',
    'AUTHORS-LGPL:',
    'LGPL license notice is reproduced here.',
    '',
  ].join('\n');
  const authorsLicenseArchive = gzipSync(createTar({ 'package/AUTHORS': authorsLicenseText }), { mtime: 0 });
  const authorsOnlyArchive = gzipSync(createTar({ 'package/AUTHORS': 'Contributors:\nA. Person\n' }), { mtime: 0 });
  const cargoPackages = [
    { name: 'cargo-good', version: '1.0.0', checksum: createHash('sha256').update(exactArchive).digest('hex'), license: 'MIT' },
    { name: 'cargo-no-text', version: '1.1.0', checksum: 'b'.repeat(64), license: 'Apache-2.0' },
    { name: 'cargo-ambiguous', version: '2.0.0', checksum: 'c'.repeat(64), license: 'MIT/Apache-2.0' },
    { name: 'cargo-missing', version: '3.0.0', checksum: 'd'.repeat(64), license: 'MIT' },
    { name: 'cargo-bad-archive', version: '4.0.0', checksum: 'e'.repeat(64), license: 'MIT' },
    { name: 'cargo-authors-license', version: '5.0.0', checksum: createHash('sha256').update(authorsLicenseArchive).digest('hex'), license: 'MIT OR Apache-2.0 OR LGPL-2.1-or-later' },
    { name: 'cargo-authors-only', version: '6.0.0', checksum: createHash('sha256').update(authorsOnlyArchive).digest('hex'), license: 'MIT' },
  ];
  await createCargoSource(cargoSrc, 'index-one', cargoPackages[1], { 'README.md': 'No license or notice candidate.\n' });
  await createCargoSource(cargoSrc, 'index-one', cargoPackages[2], { LICENSE: 'First source copy.\n' });
  await createCargoSource(cargoSrc, 'index-two', cargoPackages[2], { LICENSE: 'Second source copy.\n' });
  await writeFile(path.join(cargoCache, 'cargo-good-1.0.0.crate'), exactArchive);
  await writeFile(path.join(cargoCache, 'cargo-bad-archive-4.0.0.crate'), exactArchive);
  await writeFile(path.join(cargoCache, 'cargo-authors-license-5.0.0.crate'), authorsLicenseArchive);
  await writeFile(path.join(cargoCache, 'cargo-authors-only-6.0.0.crate'), authorsOnlyArchive);

  const npmPackages = [
    { name: 'npm-good', version: '4.2.0', lockPath: 'node_modules/npm-good' },
    { name: '@scope/npm-good', version: '5.0.1', lockPath: 'node_modules/@scope/npm-good' },
    { name: 'npm-missing', version: '6.0.0', lockPath: 'node_modules/npm-missing' },
    { name: 'npm-bad', version: '7.0.0', lockPath: 'node_modules/npm-bad' },
    { name: 'npm-other-platform', version: '8.0.0', lockPath: 'node_modules/npm-other-platform' },
    { name: 'npm-other-cpu', version: '9.0.0', lockPath: 'node_modules/npm-other-cpu' },
  ];
  await createNpmSource(npmModules, npmPackages[0], {
    'LICENSE.txt': 'npm MIT text.\n',
    'THIRD-PARTY-NOTICES.md': 'third-party notice text.\n',
  });
  await createNpmSource(npmModules, npmPackages[1], { 'NOTICE': 'npm notice text.\n' });
  const npmArchive = gzipSync(createTar({
    'package/LICENSE': 'exact locked cached npm license text.\n',
    'package/third-party-notices.md': 'exact locked cached npm notice text.\n',
  }), { mtime: 0 });
  const npmArchiveIntegrity = `sha512-${createHash('sha512').update(npmArchive).digest('base64')}`;

  const cargoMetadataPath = path.join(temp, 'cargo-metadata.json');
  const cargoLockPath = path.join(temp, 'Cargo.lock');
  const npmLockPath = path.join(temp, 'package-lock.json');
  await writeFile(cargoMetadataPath, JSON.stringify({ packages: cargoPackages.map(({ name, version, license }) => ({
    name, version, license, source: registry,
  })) }));
  await writeFile(cargoLockPath, [
    'version = 4',
    ...cargoPackages.flatMap((pkg) => [
      '', '[[package]]', `name = "${pkg.name}"`, `version = "${pkg.version}"`,
      `source = "${registry}"`, `checksum = "${pkg.checksum}"`,
    ]),
    '',
  ].join('\n'));
  const npmLockPackages = Object.fromEntries(npmPackages.map((pkg) => [pkg.lockPath, {
    name: pkg.name,
    version: pkg.version,
    resolved: `https://registry.npmjs.org/${encodeURIComponent(pkg.name)}/-/${pkg.name.split('/').at(-1)}-${pkg.version}.tgz`,
    integrity: npmArchiveIntegrity,
    license: 'MIT',
  }]));
  npmLockPackages[npmPackages[3].lockPath].integrity = `sha512-${createHash('sha512').update('not the locked archive').digest('base64')}`;
  npmLockPackages[npmPackages[4].lockPath].optional = true;
  npmLockPackages[npmPackages[4].lockPath].os = [process.platform === 'darwin' ? 'win32' : 'darwin'];
  npmLockPackages[npmPackages[5].lockPath].optional = true;
  npmLockPackages[npmPackages[5].lockPath].os = [process.platform];
  npmLockPackages[npmPackages[5].lockPath].cpu = [process.arch === 'arm64' ? 'x64' : 'arm64'];
  await writeFile(npmLockPath, JSON.stringify({ lockfileVersion: 3, packages: npmLockPackages }));
  await installNpmCacheEntry(npmCache, npmLockPackages[npmPackages[1].lockPath].resolved, npmArchive, npmArchiveIntegrity);
  await installNpmCacheEntry(npmCache, npmLockPackages[npmPackages[3].lockPath].resolved, npmArchive, npmArchiveIntegrity);

  const outputOne = path.join(temp, 'collection-one');
  const outputTwo = path.join(temp, 'collection-two');
  runCollector({ cargoMetadataPath, cargoLockPath, npmLockPath, cargoHome, npmModules, npmCache, outputDir: outputOne });
  runCollector({ cargoMetadataPath, cargoLockPath, npmLockPath, cargoHome, npmModules, npmCache, outputDir: outputTwo });

  const manifest = JSON.parse(await readFile(path.join(outputOne, 'collection.json'), 'utf8'));
  assert.equal(manifest.formatVersion, 2);
  assert.equal(manifest.packages.length, 13);
  const find = (ecosystem, name, version) => manifest.packages.find((pkg) => pkg.ecosystem === ecosystem && pkg.name === name && pkg.version === version);
  const goodCargo = find('cargo', 'cargo-good', '1.0.0');
  assert.equal(goodCargo.status, 'collected');
  assert.equal(goodCargo.packagePath, 'cargo-good-1.0.0');
  assert.equal(goodCargo.sourcePath, 'registry/cache/index-one/cargo-good-1.0.0.crate');
  assert.match(goodCargo.sourceIntegrityVerifiedBy, /matched Cargo.lock/);
  assert.equal(goodCargo.files.length, 4);
  assert.deepEqual(goodCargo.files.map((file) => file.sourceFilePath), [
    'LICENSE', 'LICENSES/Apache-2.0.txt', 'LICENSES/MIT.txt', 'licenses/NOTICE.md',
  ]);
  assert.equal(goodCargo.spdxExpression, undefined);
  for (const file of goodCargo.files) {
    const content = await readFile(path.join(outputOne, ...file.collectionPath.split('/')));
    assert.equal(createHash('sha256').update(content).digest('hex'), file.sha256);
  }
  assert.equal(find('cargo', 'cargo-no-text', '1.1.0').status, 'missing_text_files');
  assert.equal(find('cargo', 'cargo-ambiguous', '2.0.0').status, 'ambiguous_source');
  assert.equal(find('cargo', 'cargo-missing', '3.0.0').status, 'missing_source');
  assert.equal(find('cargo', 'cargo-bad-archive', '4.0.0').status, 'source_checksum_mismatch');
  const cargoAuthorsLicense = find('cargo', 'cargo-authors-license', '5.0.0');
  assert.equal(cargoAuthorsLicense.status, 'collected');
  assert.deepEqual(cargoAuthorsLicense.files.map((file) => file.sourceFilePath), ['AUTHORS']);
  assert.equal(await readFile(path.join(outputOne, ...cargoAuthorsLicense.files[0].collectionPath.split('/')), 'utf8'), authorsLicenseText);
  assert.equal(find('cargo', 'cargo-authors-only', '6.0.0').status, 'missing_text_files');
  const scopedNpm = find('npm', '@scope/npm-good', '5.0.1');
  assert.equal(scopedNpm.status, 'collected');
  assert.equal(scopedNpm.packagePath, 'node_modules/@scope/npm-good');
  assert.equal(scopedNpm.sourceIntegrityVerifiedBy, 'npm package tarball SRI verified against package-lock integrity');
  assert.match(scopedNpm.sourcePath, /^_cacache\/content-v2\/sha512\//);
  assert.equal(scopedNpm.files[0].sourceFilePath, 'LICENSE');
  assert.equal(find('npm', 'npm-good', '4.2.0').files.length, 2);
  assert.equal(find('npm', 'npm-missing', '6.0.0').status, 'missing_or_invalid_source');
  assert.equal(find('npm', 'npm-bad', '7.0.0').status, 'source_checksum_mismatch');
  assert.equal(find('npm', 'npm-other-platform', '8.0.0').status, 'not_applicable_to_target');
  assert.equal(find('npm', 'npm-other-cpu', '9.0.0').status, 'not_applicable_to_target');
  assert.equal(manifest.target.os, process.platform);
  assert.equal(manifest.target.cpu, process.arch);
  assert.equal(manifest.summary.npm.notApplicableToTarget, 2);
  assert.equal(manifest.summary.npm.missingOrAmbiguous, 2);
  assert.equal(manifest.summary.cargo.packages, 7);
  assert.equal(manifest.summary.npm.packages, 6);

  assert.deepEqual(await treeHashes(outputOne), await treeHashes(outputTwo));
});

test('refuses to overwrite a non-empty collection output directory', async (t) => {
  const temp = await mkdtemp(path.join(os.tmpdir(), 'litecr-third-party-output-'));
  t.after(() => rm(temp, { recursive: true, force: true }));
  const outputDir = path.join(temp, 'existing');
  await mkdir(outputDir);
  await writeFile(path.join(outputDir, 'keep.txt'), 'keep');
  const result = spawnSync(process.execPath, [collector, 'missing', 'missing', 'missing', 'missing', 'missing', 'missing', outputDir], { encoding: 'utf8' });
  assert.notEqual(result.status, 0);
  assert.equal(await readFile(path.join(outputDir, 'keep.txt'), 'utf8'), 'keep');
});

test('collects exact-source candidate texts only when lock, archive provenance and pinned bytes all match', async (t) => {
  const temp = await mkdtemp(path.join(os.tmpdir(), 'litecr-exact-license-bundle-'));
  t.after(() => rm(temp, { recursive: true, force: true }));
  const bundled = await prepareIsolatedCollector(temp);

  const valid = await runPinnedScenario(bundled, temp, { name: 'valuable', version: '0.1.1', commit: '9efc29b6e58cef28f6566a47aa7e142a55fead77', pathInVcs: 'valuable' });
  const valuable = valid.packages.find((pkg) => pkg.name === 'valuable');
  assert.equal(valuable.status, 'collected', JSON.stringify(valuable));
  assert.equal(valuable.pinnedSourceFallback.status, 'collected_candidate_texts');
  assert.equal(valuable.pinnedSourceFallback.legalReview, 'pending');
  assert.equal(valuable.files.length, 1);
  assert.deepEqual(valuable.files[0].externalSourceProvenance, {
    repositoryUrl: 'https://github.com/tokio-rs/valuable',
    sourceCommit: '9efc29b6e58cef28f6566a47aa7e142a55fead77',
    pathInVcs: 'valuable',
    upstreamFilePath: 'LICENSE',
    immutableCommitUrl: 'https://github.com/tokio-rs/valuable/blob/9efc29b6e58cef28f6566a47aa7e142a55fead77/LICENSE',
    reviewState: 'candidate_text_provenance_verified_human_license_and_attribution_review_pending',
    source: 'checked-in exact-commit candidate bundle; bytes verified against manifest',
  });
  assert.equal(valuable.files[0].byteLength, 1065);
  assert.equal(valuable.files[0].sha256, 'ed60d479b8fd1f64e9cbc3de449a16a53ac1b3d1b6aeb9bf9d190a8e93061b44');
  const copied = await readFile(path.join(temp, `scenario-${pinnedScenarioCounter}`, 'collection', ...valuable.files[0].collectionPath.split('/')));
  const pinned = await readFile(path.join(bundled.bundleDir, 'cargo/valuable-0.1.1/LICENSE'));
  assert.deepEqual(copied, pinned);

  const wasiteResult = await runPinnedScenario(bundled, temp, {
    name: 'wasite', version: '0.1.0', commit: '0c72934ad329ab4670eb017581640c2d6eceb289', pathInVcs: '',
    license: 'Apache-2.0 OR BSL-1.0 OR MIT',
  });
  const wasite = wasiteResult.packages.find((pkg) => pkg.name === 'wasite');
  assert.equal(wasite.status, 'collected');
  assert.equal(wasite.files.length, 3);
  assert.equal(wasite.pinnedSourceFallback.legalReview, 'pending');
  assert.deepEqual(wasite.files.map((file) => [file.sourceFilePath, file.byteLength, file.sha256]), [
    ['LICENSE_APACHE_2_0.txt', 9723, '62c7a1e35f56406896d7aa7ca52d0cc0d272ac022b5d2796e7d6905db8a3636a'],
    ['LICENSE_BOOST_1_0.txt', 1338, 'c9bff75738922193e67fa726fa225535870d2aa1059f91452c411736284ad566'],
    ['LICENSE_MIT.txt', 1036, '508a77d2e7b51d98adeed32648ad124b7b30241a8e70b2e72c99f92d8e5874d1',],
  ]);

  const checksumMismatch = await runPinnedScenario(bundled, temp, { lockChecksum: 'a'.repeat(64) });
  const checksumRecord = checksumMismatch.packages.find((pkg) => pkg.name === 'valuable');
  assert.equal(checksumRecord.status, 'source_checksum_mismatch');
  assert.equal(checksumRecord.files.length, 0);

  const commitMismatch = await runPinnedScenario(bundled, temp, { commit: 'f'.repeat(40) });
  const commitRecord = commitMismatch.packages.find((pkg) => pkg.name === 'valuable');
  assert.equal(commitRecord.files.length, 0);
  assert.equal(commitRecord.pinnedSourceFallback.status, 'rejected');
  assert.match(commitRecord.pinnedSourceFallback.reason, /commit\/path/);

  const pathMismatch = await runPinnedScenario(bundled, temp, { pathInVcs: 'wrong-path' });
  const pathRecord = pathMismatch.packages.find((pkg) => pkg.name === 'valuable');
  assert.equal(pathRecord.files.length, 0);
  assert.equal(pathRecord.pinnedSourceFallback.status, 'rejected');
  assert.match(pathRecord.pinnedSourceFallback.reason, /commit\/path/);

  const manifestChecksumMismatch = await runPinnedScenario(bundled, temp, { manifestChecksum: 'b'.repeat(64) });
  const manifestChecksumRecord = manifestChecksumMismatch.packages.find((pkg) => pkg.name === 'valuable');
  assert.equal(manifestChecksumRecord.files.length, 0);
  assert.equal(manifestChecksumRecord.pinnedSourceFallback.status, 'rejected');
  assert.match(manifestChecksumRecord.pinnedSourceFallback.reason, /archive checksum/);

  const licenseMetadataMismatch = await runPinnedScenario(bundled, temp, { license: 'Apache-2.0' });
  const licenseRecord = licenseMetadataMismatch.packages.find((pkg) => pkg.name === 'valuable');
  assert.equal(licenseRecord.files.length, 0);
  assert.equal(licenseRecord.pinnedSourceFallback.status, 'rejected');
  assert.match(licenseRecord.pinnedSourceFallback.reason, /declared Cargo license metadata/);

  const wrongVersion = await runPinnedScenario(bundled, temp, { version: '0.2.0' });
  const versionRecord = wrongVersion.packages.find((pkg) => pkg.name === 'valuable');
  assert.equal(versionRecord.files.length, 0);
  assert.equal(versionRecord.pinnedSourceFallback, undefined);

  const tampered = await prepareIsolatedCollector(path.join(temp, 'tampered'));
  const tamperedFile = path.join(tampered.bundleDir, 'cargo/valuable-0.1.1/LICENSE');
  await appendFile(tamperedFile, Buffer.from([0x20]));
  const tamperResult = await runPinnedScenario(tampered, path.join(temp, 'tampered-case'), {});
  const tamperRecord = tamperResult.packages.find((pkg) => pkg.name === 'valuable');
  assert.equal(tamperRecord.files.length, 0);
  assert.equal(tamperRecord.pinnedSourceFallback.status, 'rejected');
  assert.match(tamperRecord.pinnedSourceFallback.reason, /byte length or SHA-256 mismatch/);

  const duplicate = await prepareIsolatedCollector(path.join(temp, 'duplicate'));
  const duplicateManifestPath = path.join(duplicate.bundleDir, 'manifest.json');
  const duplicateManifest = JSON.parse(await readFile(duplicateManifestPath, 'utf8'));
  duplicateManifest.packages.push(duplicateManifest.packages[0]);
  await writeFile(duplicateManifestPath, JSON.stringify(duplicateManifest));
  const duplicateRun = runPinnedScenario(duplicate, path.join(temp, 'duplicate-case'), {});
  await assert.rejects(duplicateRun, /Duplicate exact-source license bundle record/);
});

test('collects exact npm source pins only for matching package-lock identities and preserves shared Rollup identities', async (t) => {
  const temp = await mkdtemp(path.join(os.tmpdir(), 'litecr-npm-exact-license-bundle-'));
  t.after(() => rm(temp, { recursive: true, force: true }));
  const bundled = await prepareIsolatedCollector(temp);
  const fullManifest = JSON.parse(await readFile(path.join(bundled.bundleDir, 'manifest.json'), 'utf8'));
  const npmPins = fullManifest.packages.filter((entry) => entry.ecosystem === 'npm');
  assert.equal(npmPins.length, 6);

  const valid = await runPinnedNpmScenario(bundled, path.join(temp, 'valid'), npmPins);
  const find = (name) => valid.packages.find((pkg) => pkg.ecosystem === 'npm' && pkg.name === name);
  const candidates = await Promise.all(npmPins.map(async (pin) => {
    const record = find(pin.name);
    assert.equal(record.status, 'collected', JSON.stringify(record));
    assert.equal(record.pinnedSourceFallback.status, 'collected_candidate_texts');
    assert.equal(record.pinnedSourceFallback.legalReview, 'pending');
    assert.equal(record.files.length, pin.files.length);
    for (const file of pin.files) {
      const collected = record.files.find((candidate) => candidate.sourceFilePath === file.sourceFilePath);
      assert.ok(collected, `missing ${file.sourceFilePath} for ${pin.name}`);
      assert.equal(collected.byteLength, file.byteLength);
      assert.equal(collected.sha256, file.sha256);
      assert.deepEqual(collected.externalSourceProvenance, {
        repositoryUrl: pin.repositoryUrl,
        sourceCommit: pin.sourceCommit,
        pathInVcs: pin.pathInVcs,
        upstreamFilePath: file.upstreamFilePath,
        immutableCommitUrl: file.immutableCommitUrl,
        npmResolved: pin.resolved,
        npmIntegrity: pin.integrity,
        reviewState: pin.reviewState,
        source: 'checked-in exact-commit candidate bundle; bytes verified against manifest',
      });
      assert.equal(createHash('sha256').update(await readFile(path.join(bundled.bundleDir, ...file.bundlePath.split('/')))).digest('hex'), file.sha256);
    }
    return record;
  }));
  const windowsRollup = candidates.filter((record) => record.name.startsWith('@rollup/rollup-win32-'));
  assert.equal(windowsRollup.length, 2);
  assert.equal(windowsRollup[0].files[0].sha256, windowsRollup[1].files[0].sha256);
  assert.notEqual(windowsRollup[0].files[0].collectionPath, windowsRollup[1].files[0].collectionPath);
  const linuxRollup = find('@rollup/rollup-linux-x64-gnu');
  assert.equal(linuxRollup.files.length, 2);
  assert.equal(linuxRollup.files.find((file) => file.sourceFilePath === 'LICENSE.md').sha256,
    windowsRollup[0].files[0].sha256);
  assert.equal(linuxRollup.files.find((file) => file.sourceFilePath === 'LICENSE-CORE.md').sha256,
    '9dff12bb776f199482bc21fb2aba9615032c0403d19cfd1fa0bf101a9379b146');
  const linuxEsbuild = find('@esbuild/linux-x64');
  assert.equal(linuxEsbuild.files[0].sha256, find('@esbuild/win32-x64').files[0].sha256);
  for (const record of candidates) {
    const scenarioCollection = path.join(temp, 'valid', `scenario-${pinnedScenarioCounter}`, 'collection');
    for (const file of record.files) {
      const bytes = await readFile(path.join(scenarioCollection, ...file.collectionPath.split('/')));
      assert.equal(createHash('sha256').update(bytes).digest('hex'), file.sha256);
    }
  }

  const [esbuild] = npmPins.filter((pin) => pin.name === '@esbuild/win32-x64');
  for (const [field, value, expected] of [
    ['name', 'wrong-esbuild-package', /pinnedSourceFallback/],
    ['version', '9.9.9', /pinnedSourceFallback/],
    ['resolved', `${esbuild.resolved}?substitution=1`, /resolved source URL/],
    ['integrity', 'sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=', /integrity/],
    ['license', 'Apache-2.0', /license metadata/],
  ]) {
    const result = await runPinnedNpmScenario(bundled, path.join(temp, `wrong-${field}`), [esbuild], { [esbuild.name]: { [field]: value } });
    const actualName = field === 'name' ? value : esbuild.name;
    const record = result.packages.find((pkg) => pkg.ecosystem === 'npm' && pkg.name === actualName);
    assert.ok(record, `${field} mismatch should retain a package record`);
    assert.equal(record.files.some((file) => file.externalSourceProvenance), false, JSON.stringify(record));
    if (field === 'name' || field === 'version') {
      assert.equal(record.pinnedSourceFallback, undefined);
      assert.equal(record.status, 'missing_text_files');
    } else {
      assert.equal(record.pinnedSourceFallback.status, 'rejected');
      assert.match(record.pinnedSourceFallback.reason, expected);
    }
  }

  const tampered = await prepareIsolatedCollector(path.join(temp, 'tampered'));
  await appendFile(path.join(tampered.bundleDir, esbuild.files[0].bundlePath), Buffer.from([0x20]));
  const tamperResult = await runPinnedNpmScenario(tampered, path.join(temp, 'tampered-case'), [esbuild]);
  const tamperRecord = tamperResult.packages.find((pkg) => pkg.name === esbuild.name);
  assert.equal(tamperRecord.files.some((file) => file.externalSourceProvenance), false);
  assert.equal(tamperRecord.pinnedSourceFallback.status, 'rejected');
  assert.match(tamperRecord.pinnedSourceFallback.reason, /byte length or SHA-256 mismatch/);

  const unsafe = await prepareIsolatedCollector(path.join(temp, 'unsafe-path'));
  const unsafeManifestPath = path.join(unsafe.bundleDir, 'manifest.json');
  const unsafeManifest = JSON.parse(await readFile(unsafeManifestPath, 'utf8'));
  unsafeManifest.packages.find((pin) => pin.name === esbuild.name).files[0].bundlePath = '../LICENSE.md';
  await writeFile(unsafeManifestPath, JSON.stringify(unsafeManifest));
  const unsafeResult = await runPinnedNpmScenario(unsafe, path.join(temp, 'unsafe-case'), [esbuild]);
  const unsafeRecord = unsafeResult.packages.find((pkg) => pkg.name === esbuild.name);
  assert.equal(unsafeRecord.files.some((file) => file.externalSourceProvenance), false);
  assert.equal(unsafeRecord.pinnedSourceFallback.status, 'rejected');

  const duplicate = await prepareIsolatedCollector(path.join(temp, 'duplicate-npm'));
  const duplicateManifestPath = path.join(duplicate.bundleDir, 'manifest.json');
  const duplicateManifest = JSON.parse(await readFile(duplicateManifestPath, 'utf8'));
  duplicateManifest.packages.push(duplicateManifest.packages.find((pin) => pin.name === esbuild.name));
  await writeFile(duplicateManifestPath, JSON.stringify(duplicateManifest));
  await assert.rejects(runPinnedNpmScenario(duplicate, path.join(temp, 'duplicate-case'), [esbuild]), /Duplicate exact-source license bundle record/);
});

async function createCargoSource(registrySrc, registryName, pkg, files) {
  const root = path.join(registrySrc, registryName, `${pkg.name}-${pkg.version}`);
  await mkdir(root, { recursive: true });
  await writeFile(path.join(root, '.cargo-checksum.json'), JSON.stringify({ files: {}, package: pkg.checksum }));
  for (const [relative, text] of Object.entries(files)) {
    const destination = path.join(root, ...relative.split('/'));
    await mkdir(path.dirname(destination), { recursive: true });
    await writeFile(destination, text);
  }
}

async function createNpmSource(nodeModules, pkg, files) {
  const root = path.join(nodeModules, ...pkg.lockPath.slice('node_modules/'.length).split('/'));
  await mkdir(root, { recursive: true });
  await writeFile(path.join(root, 'package.json'), JSON.stringify({ name: pkg.name, version: pkg.version }));
  for (const [relative, text] of Object.entries(files)) {
    const destination = path.join(root, ...relative.split('/'));
    await mkdir(path.dirname(destination), { recursive: true });
    await writeFile(destination, text);
  }
}

async function prepareIsolatedCollector(root) {
  const installDir = path.join(root, 'tools', 'install');
  await mkdir(installDir, { recursive: true });
  const isolatedCollector = path.join(installDir, 'collect-third-party-texts.mjs');
  const bundleDir = path.join(installDir, 'third-party-license-sources');
  await copyFile(collector, isolatedCollector);
  await cp(path.join(repo, 'tools/install/third-party-license-sources'), bundleDir, { recursive: true });
  return { collector: isolatedCollector, bundleDir, root };
}

async function runPinnedScenario(isolated, root, options = {}) {
  await mkdir(root, { recursive: true });
  const scenarioRoot = path.join(root, `scenario-${++pinnedScenarioCounter}`);
  const cargoHome = path.join(scenarioRoot, 'cargo-home');
  const cargoCache = path.join(cargoHome, 'registry', 'cache', 'fixture-registry');
  const npmModules = path.join(scenarioRoot, 'npm', 'node_modules');
  const npmCache = path.join(scenarioRoot, 'npm-cache');
  await mkdir(cargoCache, { recursive: true });
  await mkdir(npmModules, { recursive: true });
  await mkdir(npmCache, { recursive: true });

  const name = options.name ?? 'valuable';
  const version = options.version ?? '0.1.1';
  const archive = gzipSync(createTar({
    [`${name}-${version}/.cargo_vcs_info.json`]: JSON.stringify({
      git: { sha1: options.commit ?? (name === 'wasite' ? '0c72934ad329ab4670eb017581640c2d6eceb289' : '9efc29b6e58cef28f6566a47aa7e142a55fead77') },
      path_in_vcs: options.pathInVcs ?? (name === 'wasite' ? '' : 'valuable'),
    }),
  }), { mtime: 0 });
  const actualChecksum = createHash('sha256').update(archive).digest('hex');
  const lockedChecksum = options.lockChecksum ?? actualChecksum;
  const manifestPath = path.join(isolated.bundleDir, 'manifest.json');
  const bundleManifest = JSON.parse(await readFile(manifestPath, 'utf8'));
  const packagePin = bundleManifest.packages.find((entry) => entry.name === name && entry.version === version);
  if (packagePin) packagePin.registryChecksumSha256 = options.manifestChecksum ?? actualChecksum;
  await writeFile(manifestPath, JSON.stringify(bundleManifest));
  await writeFile(path.join(cargoCache, `${name}-${version}.crate`), archive);

  const cargoMetadataPath = path.join(scenarioRoot, 'cargo-metadata.json');
  const cargoLockPath = path.join(scenarioRoot, 'Cargo.lock');
  const npmLockPath = path.join(scenarioRoot, 'package-lock.json');
  await writeFile(cargoMetadataPath, JSON.stringify({ packages: [{
    name, version, license: options.license ?? 'MIT', source: registry,
  }] }));
  await writeFile(cargoLockPath, [
    'version = 4', '', '[[package]]', `name = "${name}"`, `version = "${version}"`,
    `source = "${registry}"`, `checksum = "${lockedChecksum}"`, '',
  ].join('\n'));
  await writeFile(npmLockPath, JSON.stringify({ lockfileVersion: 3, packages: {} }));
  const outputDir = path.join(scenarioRoot, 'collection');
  const result = spawnSync(process.execPath, [
    isolated.collector, cargoMetadataPath, cargoLockPath, npmLockPath, cargoHome, npmModules, npmCache, outputDir,
  ], { encoding: 'utf8' });
  if (result.status !== 0) throw new Error(`${result.stdout}\n${result.stderr}`);
  return JSON.parse(await readFile(path.join(outputDir, 'collection.json'), 'utf8'));
}

async function runPinnedNpmScenario(isolated, root, pins, overrides = {}) {
  await mkdir(root, { recursive: true });
  const scenarioRoot = path.join(root, `scenario-${++pinnedScenarioCounter}`);
  const cargoHome = path.join(scenarioRoot, 'cargo-home');
  const npmModules = path.join(scenarioRoot, 'npm', 'node_modules');
  const npmCache = path.join(scenarioRoot, 'npm-cache');
  await mkdir(path.join(cargoHome, 'registry', 'cache', 'empty'), { recursive: true });
  await mkdir(npmModules, { recursive: true });
  await mkdir(npmCache, { recursive: true });

  const npmLockPackages = {};
  for (const pin of pins) {
    const change = overrides[pin.name] ?? {};
    const name = change.name ?? pin.name;
    const version = change.version ?? pin.version;
    const lockPath = pin.packagePath;
    npmLockPackages[lockPath] = {
      version,
      resolved: change.resolved ?? pin.resolved,
      integrity: change.integrity ?? pin.integrity,
      license: change.license ?? pin.declaredLicenseMetadata,
    };
    if (change.name !== undefined) npmLockPackages[lockPath].name = change.name;
    await createNpmSource(npmModules, { lockPath, name, version }, {});
  }

  const cargoMetadataPath = path.join(scenarioRoot, 'cargo-metadata.json');
  const cargoLockPath = path.join(scenarioRoot, 'Cargo.lock');
  const npmLockPath = path.join(scenarioRoot, 'package-lock.json');
  await writeFile(cargoMetadataPath, JSON.stringify({ packages: [] }));
  await writeFile(cargoLockPath, 'version = 4\n');
  await writeFile(npmLockPath, JSON.stringify({ lockfileVersion: 3, packages: npmLockPackages }));
  const outputDir = path.join(scenarioRoot, 'collection');
  const result = spawnSync(process.execPath, [
    isolated.collector, cargoMetadataPath, cargoLockPath, npmLockPath,
    cargoHome, npmModules, npmCache, outputDir,
  ], { encoding: 'utf8' });
  if (result.status !== 0) throw new Error(`${result.stdout}\n${result.stderr}`);
  return JSON.parse(await readFile(path.join(outputDir, 'collection.json'), 'utf8'));
}

function createTar(files) {
  const blocks = [];
  for (const [name, value] of Object.entries(files)) {
    const data = Buffer.from(value, 'utf8');
    const header = Buffer.alloc(512);
    header.write(name, 0, 100, 'utf8');
    writeOctal(header, 100, 8, 0o644);
    writeOctal(header, 108, 8, 0);
    writeOctal(header, 116, 8, 0);
    writeOctal(header, 124, 12, data.length);
    writeOctal(header, 136, 12, 0);
    header.fill(0x20, 148, 156);
    header[156] = '0'.charCodeAt(0);
    header.write('ustar\0', 257, 6, 'ascii');
    header.write('00', 263, 2, 'ascii');
    const checksum = header.reduce((sum, byte) => sum + byte, 0);
    header.write(checksum.toString(8).padStart(6, '0'), 148, 6, 'ascii');
    header[154] = 0;
    header[155] = 0x20;
    blocks.push(header, data, Buffer.alloc((512 - (data.length % 512)) % 512));
  }
  blocks.push(Buffer.alloc(1024));
  return Buffer.concat(blocks);
}

function writeOctal(header, offset, width, value) {
  header.write(value.toString(8).padStart(width - 1, '0') + '\0', offset, width, 'ascii');
}

function runCollector({ cargoMetadataPath, cargoLockPath, npmLockPath, cargoHome, npmModules, npmCache, outputDir }) {
  const result = spawnSync(process.execPath, [
    collector, cargoMetadataPath, cargoLockPath, npmLockPath, cargoHome, npmModules, npmCache, outputDir,
  ], { encoding: 'utf8' });
  assert.equal(result.status, 0, `${result.stdout}\n${result.stderr}`);
}

async function installNpmCacheEntry(cacheRoot, resolved, archive, integrity) {
  const key = `make-fetch-happen:request-cache:${resolved}`;
  const keyHash = createHash('sha256').update(key).digest('hex');
  const digestHex = Buffer.from(integrity.slice('sha512-'.length), 'base64').toString('hex');
  const indexFile = path.join(cacheRoot, '_cacache', 'index-v5', keyHash.slice(0, 2), keyHash.slice(2, 4), keyHash.slice(4));
  const contentFile = path.join(cacheRoot, '_cacache', 'content-v2', 'sha512', digestHex.slice(0, 2), digestHex.slice(2, 4), digestHex.slice(4));
  await mkdir(path.dirname(indexFile), { recursive: true });
  await mkdir(path.dirname(contentFile), { recursive: true });
  const line = `${'0'.repeat(40)}\t${JSON.stringify({ key, integrity })}\n`;
  await writeFile(indexFile, line);
  await appendFile(indexFile, line);
  await writeFile(contentFile, archive);
}

async function treeHashes(root, relative = '') {
  const result = [];
  const entries = await readdir(path.join(root, relative), { withFileTypes: true });
  entries.sort((a, b) => a.name.localeCompare(b.name));
  for (const entry of entries) {
    const child = relative ? `${relative}/${entry.name}` : entry.name;
    if (entry.isDirectory()) result.push(...await treeHashes(root, child));
    else result.push([child, createHash('sha256').update(await readFile(path.join(root, child))).digest('hex')]);
  }
  return result;
}
