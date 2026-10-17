import { createHash } from 'node:crypto';
import { gunzipSync } from 'node:zlib';
import { lstat, mkdir, readFile, readdir, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const [cargoMetadataPath, cargoLockPath, npmLockPath, cargoHome, npmNodeModules, npmCache, outputDir] = process.argv.slice(2);
if (!outputDir) {
  throw new Error('usage: collect-third-party-texts.mjs <cargo-metadata.json> <Cargo.lock> <package-lock.json> <CARGO_HOME> <npm-node_modules> <npm-cache> <empty-output-dir>');
}

const cargoMetadata = JSON.parse(await readFile(cargoMetadataPath, 'utf8'));
const cargoLock = parseCargoLock(await readFile(cargoLockPath, 'utf8'));
const npmLock = JSON.parse(await readFile(npmLockPath, 'utf8'));
const sourceBundleDirectory = path.join(path.dirname(fileURLToPath(import.meta.url)), 'third-party-license-sources');
const pinnedSourceManifest = parsePinnedSourceManifest(JSON.parse(await readFile(path.join(sourceBundleDirectory, 'manifest.json'), 'utf8')));
const glibcVersion = process.platform === 'linux'
  ? process.report?.getReport?.().header?.glibcVersionRuntime ?? null
  : null;
const collectionTarget = {
  os: process.platform,
  cpu: process.arch,
  libc: glibcVersion ? 'glibc' : null,
};
const registryCacheRoot = path.join(cargoHome, 'registry', 'cache');
const registrySourceRoot = path.join(cargoHome, 'registry', 'src');
const packageSources = [];
await mkdir(outputDir, { recursive: false });

for (const pkg of cargoMetadata.packages ?? []) {
  if (!pkg.source) continue;
  const key = cargoLockKey(pkg.name, pkg.version, pkg.source);
  const lockEntry = cargoLock.get(key);
  const identity = {
    ecosystem: 'cargo',
    name: pkg.name,
    version: pkg.version,
    packagePath: `${pkg.name}-${pkg.version}`,
    source: pkg.source,
    declaredLicenseMetadata: pkg.license ?? null,
    registryChecksumSha256: lockEntry?.checksum ?? null,
  };
  const candidates = await cargoArchiveCandidates(registryCacheRoot, `${identity.packagePath}.crate`);
  let record;
  if (!lockEntry) {
    record = emptyRecord(identity, 'missing_lock_entry', 'Cargo.lock has no exact source/name/version entry.');
  } else if (!lockEntry.checksum || !pkg.source.startsWith('registry+')) {
    record = emptyRecord(identity, 'unsupported_or_missing_integrity', 'Only checksum-pinned Cargo registry packages are collected by this version.');
  } else if (candidates.length === 0) {
    record = await collectCargoSourceDirectory(identity, registrySourceRoot, lockEntry.checksum, outputDir);
  } else if (candidates.length > 1) {
    record = emptyRecord(identity, 'ambiguous_source', `Found ${candidates.length} candidate source directories; collector refuses to choose one.`);
    record.sourceCandidates = candidates.map((candidate) => candidate.relativePath);
  } else {
    try {
      const archive = await readFile(candidates[0].absolutePath);
      const archiveDigest = createHash('sha256').update(archive).digest('hex');
      if (archiveDigest !== lockEntry.checksum) {
        record = emptyRecord(identity, 'source_checksum_mismatch', 'Cargo .crate archive SHA-256 does not match Cargo.lock.');
        record.sourcePath = candidates[0].relativePath;
      } else {
        const archiveContents = cargoArchiveContents(gunzipSync(archive));
        const archiveFiles = archiveContents.licenseTextFiles;
        record = await writeCollectedFiles(identity, candidates[0].relativePath, 'cargo', outputDir, archiveFiles);
        record.sourceIntegrityVerifiedBy = 'SHA-256 of exact .crate archive matched Cargo.lock';
        await appendPinnedCargoSourceCandidates(record, identity, lockEntry.checksum, archiveContents.vcsInfo, sourceBundleDirectory, outputDir);
      }
    } catch (error) {
      record = emptyRecord(identity, 'invalid_source_archive', `Could not read or inspect the exact .crate archive: ${error.message}`);
      record.sourcePath = candidates[0].relativePath;
    }
  }
  packageSources.push(record);
}

for (const [lockPath, pkg] of Object.entries(npmLock.packages ?? {})) {
  if (!lockPath.startsWith('node_modules/')) continue;
  const packageName = pkg.name ?? lockPath.slice('node_modules/'.length).split('/node_modules/').at(-1);
  const identity = {
    ecosystem: 'npm',
    name: packageName,
    version: pkg.version ?? null,
    packagePath: lockPath,
    source: pkg.resolved ?? null,
    declaredLicenseMetadata: pkg.license ?? null,
    integrity: pkg.integrity ?? null,
    optional: pkg.optional === true,
  };
  const sourcePath = safePackagePath(npmNodeModules, lockPath);
  if (pkg.link === true) {
    packageSources.push(emptyRecord(identity, 'workspace_or_link_dependency', 'Package-lock entry is a local link, not a third-party registry package.'));
    continue;
  }
  if (!sourcePath) {
    packageSources.push(emptyRecord(identity, 'unsafe_package_path', 'Package-lock install path escapes node_modules or is not a node_modules path.'));
    continue;
  }
  if (!pkg.version || !pkg.resolved || !pkg.integrity) {
    packageSources.push(emptyRecord(identity, 'unsupported_or_missing_integrity', 'Package-lock entry lacks a registry version, resolved URL, or integrity value.'));
    continue;
  }
  const incompatibility = optionalPackageTargetMismatch(pkg, collectionTarget);
  if (incompatibility) {
    const record = emptyRecord(identity, 'not_applicable_to_target', incompatibility);
    record.applicabilityTarget = collectionTarget;
    packageSources.push(record);
    continue;
  }
  const cacheEntries = await npmCacheEntries(npmCache, pkg.resolved);
  if (cacheEntries.length > 1) {
    const record = emptyRecord(identity, 'ambiguous_source', `Found ${cacheEntries.length} npm cache records for this resolved URL; collector refuses to choose one.`);
    record.sourceCandidates = cacheEntries.map((entry) => entry.relativePath);
    packageSources.push(record);
    continue;
  }
  if (cacheEntries.length === 1) {
    try {
      const archive = await readFile(cacheEntries[0].absolutePath);
      if (!verifySRI(archive, pkg.integrity)) {
        const record = emptyRecord(identity, 'source_checksum_mismatch', 'Cached npm package tarball does not match package-lock integrity.');
        record.sourcePath = cacheEntries[0].relativePath;
        packageSources.push(record);
        continue;
      }
      const archiveFiles = licenseTextFilesFromTar(gunzipSync(archive));
      const record = await writeCollectedFiles(identity, cacheEntries[0].relativePath, 'npm', outputDir, archiveFiles);
      record.sourceIntegrityVerifiedBy = 'npm package tarball SRI verified against package-lock integrity';
      await appendPinnedNpmSourceCandidates(record, identity, pkg, sourceBundleDirectory, outputDir);
      packageSources.push(record);
      continue;
    } catch (error) {
      const record = emptyRecord(identity, 'invalid_source_archive', `Could not read or inspect cached npm tarball: ${error.message}`);
      record.sourcePath = cacheEntries[0].relativePath;
      packageSources.push(record);
      continue;
    }
  }
  let packageJson;
  try {
    const stat = await lstat(sourcePath);
    if (!stat.isDirectory() || stat.isSymbolicLink()) throw new Error('package source is not a real directory');
    if (!(await noSymlinkPath(npmNodeModules, sourcePath))) throw new Error('package source traverses a symbolic link');
    const packageManifest = path.join(sourcePath, 'package.json');
    const manifestStat = await lstat(packageManifest);
    if (!manifestStat.isFile() || manifestStat.isSymbolicLink()) throw new Error('package.json is not a regular file');
    packageJson = JSON.parse(await readFile(packageManifest, 'utf8'));
  } catch (error) {
    packageSources.push(emptyRecord(identity, 'missing_or_invalid_source', `Locked npm source unavailable or unreadable: ${error.message}`));
    continue;
  }
  if (packageJson.name !== identity.name || packageJson.version !== identity.version) {
    packageSources.push(emptyRecord(identity, 'source_identity_mismatch', 'Installed package.json name/version does not match package-lock.json.'));
    continue;
  }
  const relativePath = path.relative(npmNodeModules, sourcePath).split(path.sep).join('/');
  const record = await collectPackageFiles(identity, sourcePath, relativePath, 'npm', outputDir);
  record.sourceIntegrityVerifiedBy = 'package.json name/version matched package-lock.json; package-lock integrity is recorded but installed-tree files are not rehashed as an npm tarball';
  await appendPinnedNpmSourceCandidates(record, identity, pkg, sourceBundleDirectory, outputDir);
  packageSources.push(record);
}

packageSources.sort((a, b) => compare(a.ecosystem, b.ecosystem)
  || compare(a.name, b.name)
  || compare(String(a.version), String(b.version))
  || compare(a.packagePath, b.packagePath));

const summary = Object.fromEntries(['cargo', 'npm'].map((ecosystem) => [ecosystem, {
  packages: packageSources.filter((record) => record.ecosystem === ecosystem).length,
  withTextFiles: packageSources.filter((record) => record.ecosystem === ecosystem && record.files.length > 0).length,
  notApplicableToTarget: packageSources.filter((record) => record.ecosystem === ecosystem && record.status === 'not_applicable_to_target').length,
  missingOrAmbiguous: packageSources.filter((record) => record.ecosystem === ecosystem
    && record.status !== 'collected' && record.status !== 'not_applicable_to_target').length,
  collectedFiles: packageSources.filter((record) => record.ecosystem === ecosystem).reduce((count, record) => count + record.files.length, 0),
}]));
const manifest = {
  formatVersion: 2,
  collector: 'litecr-locked-third-party-text-collector',
  target: collectionTarget,
  collectionSemantics: 'Copies candidate license/notice text files from exact locked sources. Missing target-inapplicable optional platform packages are reported separately. No SPDX expression inference, compliance determination, legal approval, or distribution authorization is made.',
  summary,
  packages: packageSources,
};
await writeFile(path.join(outputDir, 'collection.json'), `${JSON.stringify(manifest, null, 2)}\n`);
await writeFile(path.join(outputDir, 'README.md'), renderReadme(manifest));

async function collectPackageFiles(identity, sourceRoot, sourcePath, ecosystem, collectionRoot) {
  const issues = [];
  const candidates = [];
  await visit(sourceRoot, '');
  return writeCollectedFiles(identity, sourcePath, ecosystem, collectionRoot, candidates, issues);

  async function visit(directory, relative) {
    let entries;
    try {
      entries = await readdir(directory, { withFileTypes: true });
    } catch (error) {
      issues.push(`unreadable directory ${relative || '.'}: ${error.message}`);
      return;
    }
    entries.sort((a, b) => compare(a.name, b.name));
    for (const entry of entries) {
      if (entry.name === 'node_modules' || entry.name === '.git') continue;
      const entryRelative = relative ? `${relative}/${entry.name}` : entry.name;
      const absolute = path.join(directory, entry.name);
      if (entry.isSymbolicLink()) {
        if (isLicenseTextPath(entryRelative)) issues.push(`license/notice candidate is a symlink and was not followed: ${entryRelative}`);
        continue;
      }
      if (entry.isDirectory()) {
        await visit(absolute, entryRelative);
        continue;
      }
      if (!entry.isFile() || !isLicenseTextPath(entryRelative)) continue;
      try {
        const bytes = await readFile(absolute);
        new TextDecoder('utf-8', { fatal: true }).decode(bytes);
        if (!isLicenseTextPath(entryRelative, bytes)) continue;
        candidates.push({ sourceFilePath: entryRelative.split(path.sep).join('/'), bytes });
      } catch (error) {
        issues.push(`candidate text could not be copied as UTF-8: ${entryRelative}: ${error.message}`);
      }
    }
  }
}

function emptyRecord(identity, status, issue) {
  return { ...identity, status, sourcePath: null, files: [], issues: [issue] };
}

function parsePinnedSourceManifest(manifest) {
  if (manifest?.formatVersion !== 1 || !Array.isArray(manifest.packages)) {
    throw new Error('Invalid exact-source license bundle manifest format.');
  }
  const entries = new Map();
  for (const entry of manifest.packages) {
    if (!entry || !['cargo', 'npm'].includes(entry.ecosystem) || !entry.name || !entry.version) {
      throw new Error('Exact-source license bundle contains an invalid package identity.');
    }
    const key = `${entry.ecosystem}\0${entry.name}\0${entry.version}`;
    if (entries.has(key)) throw new Error(`Duplicate exact-source license bundle record: ${entry.name}@${entry.version}`);
    entries.set(key, entry);
  }
  return entries;
}

async function appendPinnedNpmSourceCandidates(record, identity, lockPackage, bundleRoot, outputRoot) {
  const entry = pinnedSourceManifest.get(`npm\0${identity.name}\0${identity.version}`);
  if (!entry) return;
  const reject = (reason) => {
    record.issues.push(`Pinned exact-source candidate bundle was not used: ${reason}`);
    record.pinnedSourceFallback = { status: 'rejected', reason };
  };
  if ((lockPackage.name !== undefined && lockPackage.name !== entry.name) || identity.name !== entry.name) {
    return reject('package-lock key or optional name field does not match the pinned npm package identity.');
  }
  if (lockPackage.version !== entry.version || identity.version !== entry.version) return reject('package-lock version does not match the pinned npm package identity.');
  if (identity.packagePath !== entry.packagePath) return reject('package-lock install path does not match the pinned npm package identity.');
  if (typeof entry.resolved !== 'string' || identity.source !== entry.resolved) return reject('package-lock resolved source URL does not match the pinned npm source.');
  if (typeof entry.integrity !== 'string' || identity.integrity !== entry.integrity) return reject('package-lock integrity does not match the pinned npm source.');
  if (typeof entry.declaredLicenseMetadata !== 'string' || identity.declaredLicenseMetadata !== entry.declaredLicenseMetadata) {
    return reject('declared npm license metadata does not match the pinned source record.');
  }
  if (typeof entry.repositoryUrl !== 'string' || !/^https:\/\/github\.com\/[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(entry.repositoryUrl)
    || typeof entry.sourceCommit !== 'string' || !/^[a-f0-9]{40}$/.test(entry.sourceCommit)
    || typeof entry.pathInVcs !== 'string' || (entry.pathInVcs && !isSafeArchivePath(entry.pathInVcs))
    || entry.reviewState !== 'candidate_text_provenance_verified_human_license_and_attribution_review_pending') {
    return reject('manifest repository, commit, source path, or pending human-review state is invalid.');
  }
  if (!Array.isArray(entry.files) || entry.files.length === 0) return reject('manifest has no pinned source files.');

  const verifiedCandidates = [];
  const pinnedPaths = new Set();
  for (const file of entry.files) {
    if (!file || !isSafeArchivePath(file.sourceFilePath) || !isSafeArchivePath(file.upstreamFilePath)
      || !isSafeArchivePath(file.bundlePath) || !Number.isSafeInteger(file.byteLength) || file.byteLength < 0
      || !/^[a-f0-9]{64}$/.test(file.sha256 ?? '') || !isLicenseTextPath(file.sourceFilePath)
      || typeof file.immutableCommitUrl !== 'string') {
      return reject('manifest contains an invalid npm file pin or provenance record.');
    }
    if (pinnedPaths.has(file.sourceFilePath)) return reject(`manifest contains duplicate npm source file path: ${file.sourceFilePath}`);
    pinnedPaths.add(file.sourceFilePath);
    const expectedCommitUrl = `${entry.repositoryUrl}/blob/${entry.sourceCommit}/${file.upstreamFilePath}`;
    if (file.immutableCommitUrl !== expectedCommitUrl) return reject(`immutable npm commit URL does not match its pinned source path: ${file.upstreamFilePath}`);
    try {
      const candidatePath = path.join(bundleRoot, ...file.bundlePath.split('/'));
      if (!(await noSymlinkPath(bundleRoot, candidatePath))) return reject(`pinned npm bundle path traverses a symbolic link or escapes its root: ${file.bundlePath}`);
      const stat = await lstat(candidatePath);
      if (!stat.isFile() || stat.isSymbolicLink()) return reject(`pinned npm bundle file is not a regular file: ${file.bundlePath}`);
      const bytes = await readFile(candidatePath);
      new TextDecoder('utf-8', { fatal: true }).decode(bytes);
      if (!isLicenseTextPath(file.sourceFilePath, bytes)) return reject(`pinned candidate is not a recognized license/notice text: ${file.sourceFilePath}`);
      const digest = createHash('sha256').update(bytes).digest('hex');
      if (bytes.length !== file.byteLength || digest !== file.sha256) {
        return reject(`pinned npm bundle byte length or SHA-256 mismatch: ${file.bundlePath}`);
      }
      verifiedCandidates.push({
        sourceFilePath: file.sourceFilePath,
        bytes,
        externalSourceProvenance: {
          repositoryUrl: entry.repositoryUrl,
          sourceCommit: entry.sourceCommit,
          pathInVcs: entry.pathInVcs,
          upstreamFilePath: file.upstreamFilePath,
          immutableCommitUrl: file.immutableCommitUrl,
          npmResolved: entry.resolved,
          npmIntegrity: entry.integrity,
          reviewState: entry.reviewState,
          source: 'checked-in exact-commit candidate bundle; bytes verified against manifest',
        },
      });
    } catch (error) {
      return reject(`could not read pinned npm bundle file ${file.bundlePath}: ${error.message}`);
    }
  }

  const existingPaths = new Set(record.files.map((file) => file.sourceFilePath));
  for (const candidate of verifiedCandidates) {
    if (existingPaths.has(candidate.sourceFilePath)) {
      return reject(`pinned npm candidate path duplicates an archive candidate: ${candidate.sourceFilePath}`);
    }
  }
  record.issues = record.issues.filter((issue) => !issue.startsWith('No UTF-8 license/notice text candidate was found'));
  for (const candidate of verifiedCandidates) {
    const digest = createHash('sha256').update(candidate.bytes).digest('hex');
    const collectionRelative = path.posix.join('files', 'npm', record.collectionId, candidate.sourceFilePath);
    const destination = path.join(outputRoot, ...collectionRelative.split('/'));
    await mkdir(path.dirname(destination), { recursive: true });
    await writeFile(destination, candidate.bytes, { flag: 'wx' });
    record.files.push({
      sourceFilePath: candidate.sourceFilePath,
      sha256: digest,
      collectionPath: collectionRelative,
      byteLength: candidate.bytes.length,
      externalSourceProvenance: candidate.externalSourceProvenance,
    });
  }
  record.files.sort((a, b) => compare(a.sourceFilePath, b.sourceFilePath));
  record.status = record.issues.length > 0 ? 'partial' : 'collected';
  record.pinnedSourceFallback = {
    status: 'collected_candidate_texts',
    sourceCommit: entry.sourceCommit,
    pathInVcs: entry.pathInVcs,
    reviewState: entry.reviewState,
    legalReview: 'pending',
  };
}

async function appendPinnedCargoSourceCandidates(record, identity, lockChecksum, archiveVcsInfo, bundleRoot, outputRoot) {
  const key = `cargo\0${identity.name}\0${identity.version}`;
  const entry = pinnedSourceManifest.get(key);
  if (!entry) return;
  const reject = (reason) => {
    record.issues.push(`Pinned exact-source candidate bundle was not used: ${reason}`);
    record.pinnedSourceFallback = { status: 'rejected', reason };
  };
  if (entry.source !== identity.source) return reject('Cargo source does not match the manifest source.');
  if (entry.registryChecksumSha256 !== lockChecksum || identity.registryChecksumSha256 !== lockChecksum) {
    return reject('exact registry archive checksum does not match the pinned manifest and Cargo.lock.');
  }
  if (entry.declaredLicenseMetadata !== identity.declaredLicenseMetadata) {
    return reject('declared Cargo license metadata does not match the pinned manifest.');
  }
  if (!archiveVcsInfo || archiveVcsInfo.error) {
    return reject(archiveVcsInfo?.error ?? 'exact archive has no .cargo_vcs_info.json.');
  }
  const archiveCommit = archiveVcsInfo.git?.sha1;
  const archivePath = archiveVcsInfo.path_in_vcs ?? '';
  if (archiveCommit !== entry.sourceCommit || archivePath !== entry.pathInVcs) {
    return reject(`archive .cargo_vcs_info.json commit/path (${archiveCommit ?? 'missing'} / ${JSON.stringify(archivePath)}) does not match pinned provenance.`);
  }
  if (!Array.isArray(entry.files) || entry.files.length === 0) return reject('manifest has no pinned source files.');

  const verifiedCandidates = [];
  const pinnedPaths = new Set();
  for (const file of entry.files) {
    if (!file || !isSafeArchivePath(file.sourceFilePath) || !isSafeArchivePath(file.bundlePath)
      || !Number.isSafeInteger(file.byteLength) || file.byteLength < 0 || !/^[a-f0-9]{64}$/.test(file.sha256 ?? '')
      || !isLicenseTextPath(file.sourceFilePath) || typeof file.upstreamFilePath !== 'string'
      || typeof entry.repositoryUrl !== 'string' || typeof entry.sourceCommit !== 'string'
      || typeof file.immutableCommitUrl !== 'string') {
      return reject('manifest contains an invalid file pin or provenance record.');
    }
    if (pinnedPaths.has(file.sourceFilePath)) return reject(`manifest contains duplicate source file path: ${file.sourceFilePath}`);
    pinnedPaths.add(file.sourceFilePath);
    const expectedCommitUrl = `${entry.repositoryUrl.replace(/\/$/, '')}/blob/${entry.sourceCommit}/${file.upstreamFilePath}`;
    if (file.immutableCommitUrl !== expectedCommitUrl) return reject(`immutable commit URL does not match its pinned source path: ${file.upstreamFilePath}`);
    try {
      const candidatePath = path.join(bundleRoot, ...file.bundlePath.split('/'));
      if (!(await noSymlinkPath(bundleRoot, candidatePath))) return reject(`pinned bundle path traverses a symbolic link or escapes its root: ${file.bundlePath}`);
      const stat = await lstat(candidatePath);
      if (!stat.isFile() || stat.isSymbolicLink()) return reject(`pinned bundle file is not a regular file: ${file.bundlePath}`);
      const bytes = await readFile(candidatePath);
      new TextDecoder('utf-8', { fatal: true }).decode(bytes);
      if (!isLicenseTextPath(file.sourceFilePath, bytes)) return reject(`pinned Cargo candidate is not a recognized license/notice text: ${file.sourceFilePath}`);
      const digest = createHash('sha256').update(bytes).digest('hex');
      if (bytes.length !== file.byteLength || digest !== file.sha256) {
        return reject(`pinned bundle byte length or SHA-256 mismatch: ${file.bundlePath}`);
      }
      verifiedCandidates.push({
        sourceFilePath: file.sourceFilePath,
        bytes,
        externalSourceProvenance: {
          repositoryUrl: entry.repositoryUrl,
          sourceCommit: entry.sourceCommit,
          pathInVcs: entry.pathInVcs,
          upstreamFilePath: file.upstreamFilePath,
          immutableCommitUrl: file.immutableCommitUrl,
          reviewState: entry.reviewState,
          source: 'checked-in exact-commit candidate bundle; bytes verified against manifest',
        },
      });
    } catch (error) {
      return reject(`could not read pinned bundle file ${file.bundlePath}: ${error.message}`);
    }
  }

  const existingPaths = new Set(record.files.map((file) => file.sourceFilePath));
  for (const candidate of verifiedCandidates) {
    if (existingPaths.has(candidate.sourceFilePath)) {
      return reject(`pinned candidate path duplicates an archive candidate: ${candidate.sourceFilePath}`);
    }
  }
  record.issues = record.issues.filter((issue) => !issue.startsWith('No UTF-8 license/notice text candidate was found'));
  for (const candidate of verifiedCandidates) {
    const digest = createHash('sha256').update(candidate.bytes).digest('hex');
    const id = record.collectionId;
    const collectionRelative = path.posix.join('files', 'cargo', id, candidate.sourceFilePath);
    const destination = path.join(outputRoot, ...collectionRelative.split('/'));
    await mkdir(path.dirname(destination), { recursive: true });
    await writeFile(destination, candidate.bytes, { flag: 'wx' });
    record.files.push({
      sourceFilePath: candidate.sourceFilePath,
      sha256: digest,
      collectionPath: collectionRelative,
      byteLength: candidate.bytes.length,
      externalSourceProvenance: candidate.externalSourceProvenance,
    });
  }
  record.files.sort((a, b) => compare(a.sourceFilePath, b.sourceFilePath));
  record.status = record.issues.length > 0 ? 'partial' : 'collected';
  record.pinnedSourceFallback = {
    status: 'collected_candidate_texts',
    sourceCommit: entry.sourceCommit,
    pathInVcs: entry.pathInVcs,
    reviewState: entry.reviewState,
    legalReview: 'pending',
  };
}

function optionalPackageTargetMismatch(pkg, target) {
  if (pkg.optional !== true) return null;
  const mismatches = [];
  for (const [key, actual] of [['os', target.os], ['cpu', target.cpu], ['libc', target.libc]]) {
    if (actual === null || actual === undefined || pkg[key] === undefined) continue;
    const constraints = Array.isArray(pkg[key]) ? pkg[key] : [pkg[key]];
    const positive = constraints.filter((value) => typeof value === 'string' && !value.startsWith('!'));
    const negative = constraints.filter((value) => typeof value === 'string' && value.startsWith('!')).map((value) => value.slice(1));
    if ((positive.length > 0 && !positive.includes('*') && !positive.includes(actual)) || negative.includes(actual)) {
      mismatches.push(`${key}=${actual} excluded by lockfile constraint ${JSON.stringify(pkg[key])}`);
    }
  }
  return mismatches.length > 0
    ? `Optional locked package is not applicable to this build target: ${mismatches.join('; ')}.`
    : null;
}

function isLicenseTextName(name) {
  return /^(?:(?:third[._ -]?party|3rd[._ -]?party)[._ -]*)?(?:licen[cs]es?|copying|notices?|copyright)(?:$|[._ -])/i.test(name);
}

function isLicenseTextPath(filePath, content = null) {
  const normalized = filePath.split('\\').join('/');
  const name = path.posix.basename(normalized);
  if (isLicenseTextName(name)) return true;
  // r-efi 6.0.0 ships its project license notice and the MIT/Apache/LGPL
  // license sections in AUTHORS rather than a conventional LICENSE file.
  // Require the complete section markers so ordinary author lists are ignored.
  if (/^AUTHORS$/i.test(name)) {
    if (content === null) return true;
    const text = Buffer.isBuffer(content) ? content.toString('utf8') : String(content);
    return /^LICENSE:\s*$/im.test(text)
      && /^AUTHORS-MIT:\s*$/im.test(text)
      && /^AUTHORS-ASL:\s*$/im.test(text)
      && /^AUTHORS-LGPL:\s*$/im.test(text);
  }
  const inLicenseDirectory = normalized.split('/').slice(0, -1).some((part) => /^licenses?$/i.test(part));
  return inLicenseDirectory && /^[A-Za-z0-9][A-Za-z0-9.+-]*\.txt$/i.test(name)
    && !/^(?:README|CHANGELOG|NOTICE)\.txt$/i.test(name);
}

async function writeCollectedFiles(identity, sourcePath, ecosystem, collectionRoot, candidates, issues = []) {
  const id = createHash('sha256').update(`${ecosystem}\0${identity.name}\0${identity.version}\0${identity.packagePath}\0${identity.source ?? ''}`).digest('hex').slice(0, 20);
  const files = [];
  candidates.sort((a, b) => compare(a.sourceFilePath, b.sourceFilePath));
  for (const candidate of candidates) {
    if (candidate.unsafePath) {
      issues.push(`unsafe archive path was not collected: ${candidate.sourceFilePath}`);
      continue;
    }
    const bytes = Buffer.from(candidate.bytes);
    try {
      new TextDecoder('utf-8', { fatal: true }).decode(bytes);
      const digest = createHash('sha256').update(bytes).digest('hex');
      const collectionRelative = path.posix.join('files', ecosystem, id, candidate.sourceFilePath);
      const destination = path.join(collectionRoot, ...collectionRelative.split('/'));
      await mkdir(path.dirname(destination), { recursive: true });
      await writeFile(destination, bytes, { flag: 'wx' });
      files.push({
        sourceFilePath: candidate.sourceFilePath,
        sha256: digest,
        collectionPath: collectionRelative,
        byteLength: bytes.length,
      });
    } catch (error) {
      issues.push(`candidate text could not be copied as UTF-8: ${candidate.sourceFilePath}: ${error.message}`);
    }
  }
  if (files.length === 0 && issues.length === 0) {
    issues.push('No UTF-8 license/notice text candidate was found (LICENSE/LICENCE, COPYING, NOTICE, COPYRIGHT, or third-party license/notice filename).');
  }
  return {
    ...identity,
    status: files.length === 0 ? 'missing_text_files' : (issues.length ? 'partial' : 'collected'),
    sourcePath,
    collectionId: id,
    files,
    issues,
  };
}

async function collectCargoSourceDirectory(identity, registrySourceRoot, lockChecksum, outputDir) {
  const candidates = await cargoSourceCandidates(registrySourceRoot, identity.packagePath);
  if (candidates.length === 0) {
    return emptyRecord(identity, 'missing_source', 'No matching exact Cargo .crate archive or unpacked registry source directory is present in CARGO_HOME.');
  }
  if (candidates.length > 1) {
    const record = emptyRecord(identity, 'ambiguous_source', `Found ${candidates.length} unpacked source directories; collector refuses to choose one.`);
    record.sourceCandidates = candidates.map((candidate) => candidate.relativePath);
    return record;
  }
  const candidate = candidates[0];
  try {
    const checksumFile = JSON.parse(await readFile(path.join(candidate.absolutePath, '.cargo-checksum.json'), 'utf8'));
    if (checksumFile.package !== lockChecksum) {
      const record = emptyRecord(identity, 'source_checksum_mismatch', 'Unpacked Cargo source checksum does not match Cargo.lock.');
      record.sourcePath = candidate.relativePath;
      return record;
    }
    const record = await collectPackageFiles(identity, candidate.absolutePath, candidate.relativePath, 'cargo', outputDir);
    record.sourceIntegrityVerifiedBy = '.cargo-checksum.json package value matched Cargo.lock checksum';
    return record;
  } catch (error) {
    const record = emptyRecord(identity, 'missing_source', `Matching unpacked source has no verifiable .cargo-checksum.json and no exact .crate archive is cached: ${error.message}`);
    record.sourcePath = candidate.relativePath;
    return record;
  }
}

async function cargoArchiveCandidates(cacheRoot, crateFile) {
  let registries;
  try {
    registries = await readdir(cacheRoot, { withFileTypes: true });
  } catch {
    return [];
  }
  const candidates = [];
  for (const registry of registries.filter((entry) => entry.isDirectory()).sort((a, b) => compare(a.name, b.name))) {
    const absolutePath = path.join(cacheRoot, registry.name, crateFile);
    try {
      const stat = await lstat(absolutePath);
      if (stat.isFile() && !stat.isSymbolicLink()) {
        candidates.push({ absolutePath, relativePath: path.posix.join('registry', 'cache', registry.name, crateFile) });
      }
    } catch {
      // A missing archive under this registry is expected; all misses are reported by the caller.
    }
  }
  return candidates;
}

function licenseTextFilesFromTar(tar) {
  return cargoArchiveContents(tar).licenseTextFiles;
}

function cargoArchiveContents(tar) {
  const files = [];
  const vcsMetadata = [];
  let offset = 0;
  let nextPath = null;
  while (offset + 512 <= tar.length) {
    const header = tar.subarray(offset, offset + 512);
    if (header.every((byte) => byte === 0)) break;
    const name = readTarString(header, 0, 100);
    const prefix = readTarString(header, 345, 155);
    const type = String.fromCharCode(header[156] || 0);
    const size = readTarSize(header.subarray(124, 136));
    const bodyStart = offset + 512;
    const bodyEnd = bodyStart + size;
    if (bodyEnd > tar.length) throw new Error('truncated tar archive member');
    const body = tar.subarray(bodyStart, bodyEnd);
    let memberPath = nextPath ?? (prefix ? `${prefix}/${name}` : name);
    nextPath = null;
    if (type === 'L') {
      nextPath = body.toString('utf8').replace(/\0.*$/s, '').replace(/\n$/, '');
    } else if (type === 'x') {
      const pax = parsePaxPath(body.toString('utf8'));
      if (pax) nextPath = pax;
    } else if (type === '0' || type === '\0' || type === '7') {
      memberPath = memberPath.replace(/^\.\//, '');
      if (memberPath.startsWith('package/')) memberPath = memberPath.slice('package/'.length);
      if (path.posix.basename(memberPath) === '.cargo_vcs_info.json') {
        try {
          vcsMetadata.push(JSON.parse(body.toString('utf8')));
        } catch (error) {
          vcsMetadata.push({ error: `invalid .cargo_vcs_info.json: ${error.message}` });
        }
      }
      const isTextCandidate = isLicenseTextPath(memberPath, body);
      if (isTextCandidate && isSafeArchivePath(memberPath)) {
        files.push({ sourceFilePath: memberPath, bytes: Buffer.from(body) });
      } else if (isTextCandidate) {
        files.push({ sourceFilePath: `unsafe-archive-path:${memberPath}`, bytes: Buffer.from(body), unsafePath: true });
      }
    }
    offset = bodyStart + Math.ceil(size / 512) * 512;
  }
  const vcsInfo = vcsMetadata.length === 1
    ? vcsMetadata[0]
    : { error: vcsMetadata.length === 0 ? 'exact archive has no .cargo_vcs_info.json.' : 'exact archive contains multiple .cargo_vcs_info.json files.' };
  return { licenseTextFiles: files, vcsInfo };
}

function parsePaxPath(text) {
  let offset = 0;
  while (offset < text.length) {
    const space = text.indexOf(' ', offset);
    if (space < 0) break;
    const length = Number(text.slice(offset, space));
    if (!Number.isSafeInteger(length) || length <= space - offset + 1) break;
    const record = text.slice(space + 1, offset + length - 1);
    if (record.startsWith('path=')) return record.slice(5);
    offset += length;
  }
  return null;
}

function isSafeArchivePath(value) {
  return value.length > 0 && !value.startsWith('/') && !value.includes('\\')
    && value.split('/').every((part) => part && part !== '.' && part !== '..');
}

function readTarString(header, start, length) {
  const bytes = header.subarray(start, start + length);
  const end = bytes.indexOf(0);
  return bytes.subarray(0, end < 0 ? bytes.length : end).toString('utf8');
}

function readTarSize(bytes) {
  const text = bytes.toString('ascii').replace(/\0.*$/, '').trim();
  if (!text) return 0;
  const size = Number.parseInt(text, 8);
  if (!Number.isSafeInteger(size) || size < 0) throw new Error('invalid tar archive member size');
  return size;
}

async function cargoSourceCandidates(sourceRoot, packageDirName) {
  let registries;
  try {
    registries = await readdir(sourceRoot, { withFileTypes: true });
  } catch {
    return [];
  }
  const candidates = [];
  for (const registry of registries.filter((entry) => entry.isDirectory()).sort((a, b) => compare(a.name, b.name))) {
    const absolutePath = path.join(sourceRoot, registry.name, packageDirName);
    try {
      const stat = await lstat(absolutePath);
      if (stat.isDirectory() && !stat.isSymbolicLink()) {
        candidates.push({ absolutePath, relativePath: path.posix.join('registry', 'src', registry.name, packageDirName) });
      }
    } catch {
      // A missing candidate under this registry is expected; all misses are reported by the caller.
    }
  }
  return candidates;
}

function safePackagePath(nodeModulesRoot, lockPath) {
  if (!lockPath.startsWith('node_modules/')) return null;
  const relative = lockPath.slice('node_modules/'.length).split('/');
  if (relative.length === 0 || relative.some((part) => !part || part === '.' || part === '..')) return null;
  const root = path.resolve(nodeModulesRoot);
  const resolved = path.resolve(root, ...relative);
  return resolved.startsWith(`${root}${path.sep}`) ? resolved : null;
}

async function npmCacheEntries(cacheRoot, resolvedUrl) {
  if (!cacheRoot || !resolvedUrl) return [];
  const key = `make-fetch-happen:request-cache:${resolvedUrl}`;
  const keyHash = createHash('sha256').update(key).digest('hex');
  const indexPath = path.join(cacheRoot, '_cacache', 'index-v5', keyHash.slice(0, 2), keyHash.slice(2, 4), keyHash.slice(4));
  let indexText;
  try {
    indexText = await readFile(indexPath, 'utf8');
  } catch {
    return [];
  }
  const entries = [];
  for (const line of indexText.split(/\r?\n/)) {
    const tab = line.indexOf('\t');
    if (tab < 0) continue;
    try {
      const entry = JSON.parse(line.slice(tab + 1));
      if (entry.key !== key || typeof entry.integrity !== 'string') continue;
      const digest = entry.integrity.split(/\s+/).map(parseSRI).find(Boolean);
      if (!digest) continue;
      const digestHex = Buffer.from(digest.value, 'base64').toString('hex');
      const contentRelative = path.posix.join('_cacache', 'content-v2', digest.algorithm, digestHex.slice(0, 2), digestHex.slice(2, 4), digestHex.slice(4));
      entries.push({
        absolutePath: path.join(cacheRoot, ...contentRelative.split('/')),
        relativePath: contentRelative,
      });
    } catch {
      // Corrupt index lines are not treated as source; the lock/package path fallback remains explicit.
    }
  }
  return [...new Map(entries.map((entry) => [entry.relativePath, entry])).values()];
}

function parseSRI(value) {
  const match = /^(sha1|sha256|sha384|sha512)-([A-Za-z0-9+/=]+)(?:\?.*)?$/.exec(value);
  return match ? { algorithm: match[1], value: match[2] } : null;
}

function verifySRI(bytes, integrity) {
  for (const token of integrity.split(/\s+/)) {
    const digest = parseSRI(token);
    if (!digest) continue;
    const actual = createHash(digest.algorithm).update(bytes).digest('base64');
    if (actual === digest.value) return true;
  }
  return false;
}

async function noSymlinkPath(root, target) {
  const relative = path.relative(path.resolve(root), path.resolve(target));
  if (!relative || relative.startsWith('..') || path.isAbsolute(relative)) return false;
  let current = path.resolve(root);
  for (const component of relative.split(path.sep)) {
    current = path.join(current, component);
    const stat = await lstat(current);
    if (stat.isSymbolicLink()) return false;
  }
  return true;
}

function parseCargoLock(text) {
  const packages = new Map();
  const sections = text.split(/(?=^\[\[package\]\]\s*$)/m)
    .filter((section) => /^\[\[package\]\]\s*$/m.test(section));
  for (const section of sections) {
    const readString = (key) => {
      const match = new RegExp(`^${key}\\s*=\\s*("(?:[^"\\\\]|\\\\.)*")\\s*$`, 'm').exec(section);
      return match ? JSON.parse(match[1]) : null;
    };
    const name = readString('name');
    const version = readString('version');
    const source = readString('source');
    if (!name || !version || !source) continue;
    const checksum = readString('checksum');
    if (checksum && !/^[a-f0-9]{64}$/.test(checksum)) throw new Error(`Malformed Cargo.lock checksum for ${name} ${version}`);
    packages.set(cargoLockKey(name, version, source), { checksum });
  }
  return packages;
}

function cargoLockKey(name, version, source) {
  return `${name}\0${version}\0${source}`;
}

function renderReadme(manifest) {
  const lines = [
    '# Collected third-party license and notice texts',
    '',
    'This is an automated source-text collection from locked dependency sources available during this build. It is not legal advice, a license determination, a compliance review, or permission to distribute.',
    'No SPDX expressions are inferred here. See `collection.json` for the exact package/version/source path, the file path and SHA-256, and every missing, mismatched, ambiguous, or unreadable source reported by the collector.',
    '',
    `Build target: \`${manifest.target.os}-${manifest.target.cpu}${manifest.target.libc ? `-${manifest.target.libc}` : ''}\`. Optional lockfile packages excluded by target OS/CPU/libc are listed separately and are not treated as missing source text.`,
    '',
    '| Ecosystem | Packages | Packages with text files | Not applicable to target | Missing/ambiguous source or text | Files copied |',
    '|---|---:|---:|---:|---:|---:|',
  ];
  for (const ecosystem of ['cargo', 'npm']) {
    const row = manifest.summary[ecosystem];
    lines.push(`| ${ecosystem} | ${row.packages} | ${row.withTextFiles} | ${row.notApplicableToTarget} | ${row.missingOrAmbiguous} | ${row.collectedFiles} |`);
  }
  lines.push('', '## Package report', '');
  for (const pkg of manifest.packages) {
    lines.push(`- ${pkg.ecosystem} \`${pkg.name}@${pkg.version ?? 'unknown'}\` — **${pkg.status}**; source: \`${pkg.packagePath}\`; declared metadata: \`${pkg.declaredLicenseMetadata ?? 'missing'}\`; files: ${pkg.files.length}.`);
    for (const issue of pkg.issues) lines.push(`  - Issue: ${issue}`);
    for (const file of pkg.files) lines.push(`  - \`${file.sourceFilePath}\` → \`${file.collectionPath}\` (SHA-256 \`${file.sha256}\`).`);
  }
  lines.push('');
  return `${lines.join('\n')}\n`;
}

function compare(left, right) {
  return left < right ? -1 : left > right ? 1 : 0;
}
