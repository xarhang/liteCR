import { createHash } from 'node:crypto';
import { readFile, writeFile } from 'node:fs/promises';

const [cargoPath, cargoLockPath, npmLockPath, versionPath, cargoOutPath, npmOutPath, noticesPath, releaseOutPath, sbomOutPath] = process.argv.slice(2);
if (!sbomOutPath) throw new Error('expected cargo metadata, Cargo.lock, npm lock, version and output paths');
const cargo = JSON.parse(await readFile(cargoPath, 'utf8'));
const cargoLockChecksums = parseCargoLock(await readFile(cargoLockPath, 'utf8'));
const version = JSON.parse(await readFile(versionPath, 'utf8'));
const npm = JSON.parse(await readFile(npmLockPath, 'utf8'));
const sourceDateEpochText = process.env.SOURCE_DATE_EPOCH;
const sourceRevisionText = process.env.LITECR_SOURCE_REVISION ?? '';
const sourceRevisionStatus = process.env.LITECR_SOURCE_REVISION_STATUS ?? 'no_git_metadata';
if (!sourceDateEpochText || !/^\d+$/.test(sourceDateEpochText)) {
  throw new Error('SOURCE_DATE_EPOCH must be set to a non-negative Unix timestamp in seconds');
}
const sourceDateEpoch = BigInt(sourceDateEpochText);
if (sourceDateEpoch > 253_402_300_799n) {
  throw new Error('SOURCE_DATE_EPOCH is outside the supported timestamp range');
}
const created = new Date(Number(sourceDateEpoch) * 1000).toISOString();
if (typeof version.application_version !== 'string' || !Number.isInteger(version.schema_version)) {
  throw new Error('binary version response did not match the frozen JSON contract');
}
const sourceRevision = sourceRevisionText || null;
const sourceRevisionStatuses = new Set([
  'clean_git_worktree',
  'dirty_git_worktree',
  'git_revision_unavailable',
  'git_status_unavailable',
  'no_git_metadata',
]);
if (!sourceRevisionStatuses.has(sourceRevisionStatus)) {
  throw new Error('source revision status is not recognized');
}
if (sourceRevision !== null && !/^([a-f0-9]{40}|[a-f0-9]{64})$/.test(sourceRevision)) {
  throw new Error('source revision must be a full lowercase Git object ID');
}
if ((sourceRevisionStatus === 'clean_git_worktree') !== (sourceRevision !== null)) {
  throw new Error('a source revision is recorded only for a clean Git worktree');
}
const application = cargo.packages.find((pkg) => pkg.name === 'litecr-server');
if (!application) throw new Error('cargo metadata does not include litecr-server');
const cargoPackages = cargo.packages
  .filter((pkg) => pkg.name !== 'litecr-server')
  .map(({ name, version: packageVersion, license, source }) => ({
    name,
    version: packageVersion,
    license: license ?? null,
    source: source ?? 'workspace',
  }))
  .sort((a, b) => compare(a.name, b.name) || compare(a.version, b.version));
const npmPackages = Object.entries(npm.packages ?? {})
  .filter(([path]) => path.startsWith('node_modules/'))
  .map(([path, pkg]) => ({
    name: pkg.name ?? path.slice('node_modules/'.length),
    version: pkg.version ?? null,
    license: pkg.license ?? null,
    integrity: pkg.integrity ?? null,
    downloadLocation: pkg.resolved ?? 'NOASSERTION',
    lockScope: {
      dev: pkg.dev ?? null,
      optional: pkg.optional ?? null,
      devOptional: pkg.devOptional ?? null,
      os: pkg.os ?? null,
      cpu: pkg.cpu ?? null,
      libc: pkg.libc ?? null,
    },
  }))
  .sort((a, b) => compare(a.name, b.name) || compare(String(a.version), String(b.version)));
const distributionStatus = application.license === 'UNLICENSED' || !application.license
  ? 'blocked_unlicensed'
  : 'review_required';
const releaseInputs = JSON.stringify({
  application_version: version.application_version,
  schema_version: version.schema_version,
  platform: 'linux-x86_64',
  project_license: application.license ?? null,
  cargo_packages: cargoPackages,
  npm_packages: npmPackages,
  source_date_epoch: sourceDateEpoch.toString(),
  source_revision: sourceRevision,
  source_revision_status: sourceRevisionStatus,
});
const releaseInputDigest = createHash('sha256').update(releaseInputs).digest('hex');
await writeFile(cargoOutPath, `${JSON.stringify(cargoPackages, null, 2)}\n`);
await writeFile(npmOutPath, `${JSON.stringify(npmPackages, null, 2)}\n`);
await writeFile(releaseOutPath, `${JSON.stringify({
  application_version: version.application_version,
  schema_version: version.schema_version,
  platform: 'linux-x86_64',
  package_format: 1,
  project_license: application.license ?? null,
  distribution_status: distributionStatus,
  source_revision: sourceRevision,
  source_revision_status: sourceRevisionStatus,
}, null, 2)}\n`);
const sbomPackages = [
  {
    name: 'litecr-server',
    version: version.application_version,
    license: application.license ?? null,
    licenseMetadata: application.license ?? null,
    purlType: 'cargo',
  },
  ...cargoPackages.map((pkg) => ({
    ...pkg,
    checksums: cargoPackageChecksums(pkg, cargoLockChecksums),
    purlType: 'cargo',
  })),
  ...npmPackages.map((pkg) => ({
    ...pkg,
    checksums: pkg.integrity === null ? [] : npmIntegrityChecksums(pkg.integrity),
    purlType: 'npm',
  })),
];
const verifiedCargoSlashLicenses = new Map([
  ['curve25519-dalek-derive\u00000.1.1\u0000MIT/Apache-2.0', 'MIT OR Apache-2.0'],
  ['ff\u00000.13.1\u0000MIT/Apache-2.0', 'MIT OR Apache-2.0'],
  ['flume\u00000.11.1\u0000Apache-2.0/MIT', 'Apache-2.0 OR MIT'],
  ['fnv\u00001.0.7\u0000Apache-2.0 / MIT', 'Apache-2.0 OR MIT'],
  ['group\u00000.13.0\u0000MIT/Apache-2.0', 'MIT OR Apache-2.0'],
  ['ident_case\u00001.0.1\u0000MIT/Apache-2.0', 'MIT OR Apache-2.0'],
  ['itertools\u00000.10.5\u0000MIT/Apache-2.0', 'MIT OR Apache-2.0'],
  ['num-bigint-dig\u00000.8.6\u0000MIT/Apache-2.0', 'MIT OR Apache-2.0'],
  ['plain\u00000.2.3\u0000MIT/Apache-2.0', 'MIT OR Apache-2.0'],
  ['serde_urlencoded\u00000.7.1\u0000MIT/Apache-2.0', 'MIT OR Apache-2.0'],
  ['serde_plain\u00001.0.2\u0000MIT/Apache-2.0', 'MIT OR Apache-2.0'],
  ['stringprep\u00000.1.5\u0000MIT/Apache-2.0', 'MIT OR Apache-2.0'],
  ['unicode-properties\u00000.1.4\u0000MIT/Apache-2.0', 'MIT OR Apache-2.0'],
  ['version_check\u00000.9.5\u0000MIT/Apache-2.0', 'MIT OR Apache-2.0'],
  ['vcpkg\u00000.2.15\u0000MIT/Apache-2.0', 'MIT OR Apache-2.0'],
]);

const spdxPackages = sbomPackages.map((pkg, index) => {
  const license = spdxLicense(pkg.name, pkg.version, pkg.license, pkg.licenseMetadata);
  return {
    name: pkg.name,
    SPDXID: `SPDXRef-Package-${index + 1}`,
    versionInfo: pkg.version ?? 'NOASSERTION',
    downloadLocation: pkg.source === 'workspace' ? 'NOASSERTION' : (pkg.downloadLocation ?? pkg.source),
    filesAnalyzed: false,
    licenseConcluded: 'NOASSERTION',
    licenseDeclared: license.declared,
    ...(license.comment ? { licenseComments: license.comment } : {}),
    ...(pkg.purlType === 'npm' ? { packageComment: npmLockScopeComment(pkg.lockScope) } : {}),
    primaryPackagePurpose: index === 0 ? 'APPLICATION' : 'LIBRARY',
    ...(pkg.checksums?.length ? { checksums: pkg.checksums } : {}),
    externalRefs: pkg.version ? [{
      referenceCategory: 'PACKAGE-MANAGER',
      referenceType: 'purl',
      referenceLocator: `pkg:${pkg.purlType}/${pkg.purlType === 'npm' && pkg.name.startsWith('@') ? `%40${pkg.name.slice(1)}` : pkg.name}@${pkg.version}`,
    }] : [],
  };
});
await writeFile(sbomOutPath, `${JSON.stringify({
  spdxVersion: 'SPDX-2.3',
  dataLicense: 'CC0-1.0',
  SPDXID: 'SPDXRef-DOCUMENT',
  name: `litecr-${version.application_version}-linux-x86_64`,
  documentNamespace: `https://litecr.invalid/spdx/sha256-${releaseInputDigest}`,
  creationInfo: { creators: ['Tool: litecr-release-metadata'], created },
  packages: spdxPackages,
  relationships: [
    { spdxElementId: 'SPDXRef-DOCUMENT', relationshipType: 'DESCRIBES', relatedSpdxElement: 'SPDXRef-Package-1' },
  ],
}, null, 2)}\n`);
const lines = [
  '# Third-party dependency license index',
  '',
  'This index records license identifiers from the locked Cargo metadata and npm package lock. It is dependency metadata, not legal advice or a substitute for reviewing each upstream license and notice.',
  '',
  `Application version: ${version.application_version}`,
  `Schema version: ${version.schema_version}`,
  `Project license: ${application.license ?? 'UNLICENSED / not declared'}`,
  `Distribution status: ${distributionStatus} (metadata does not grant distribution rights)`,
  '',
  '## Rust dependencies',
  '',
  ...cargoPackages.map((pkg) => `- ${pkg.name} ${pkg.version} — ${pkg.license ?? 'license metadata missing'}`),
  '',
  '## Locked npm packages',
  '',
  'This is the full npm lockfile inventory, including development, test, optional, and platform-specific packages. Lock classifications describe build inputs; they do not establish which package files are embedded or distributed.',
  '',
  ...npmPackages.map((pkg) => {
    const scope = npmLockScopeSummary(pkg.lockScope);
    return `- ${pkg.name} ${pkg.version ?? 'unknown'} — ${pkg.license ?? 'license metadata missing'}${scope ? ` (npm lock: ${scope})` : ''}`;
  }),
  '',
];
await writeFile(noticesPath, `${lines.join('\n')}\n`);

function compare(left, right) {
  return left < right ? -1 : left > right ? 1 : 0;
}

function npmLockScopeSummary(scope) {
  const fields = [];
  for (const key of ['dev', 'optional', 'devOptional']) {
    if (typeof scope?.[key] === 'boolean') fields.push(`${key}=${scope[key]}`);
  }
  for (const key of ['os', 'cpu', 'libc']) {
    const values = scope?.[key];
    if (Array.isArray(values) && values.every((value) => typeof value === 'string')) {
      fields.push(`${key}=${values.join('|')}`);
    }
  }
  return fields.join(', ');
}

function npmLockScopeComment(scope) {
  const summary = npmLockScopeSummary(scope);
  return summary
    ? `npm package-lock scope metadata (not a runtime inclusion determination): ${summary}.`
    : 'npm package-lock scope metadata contains no dev, optional, or platform classification fields.';
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
    const packageVersion = readString('version');
    const source = readString('source') ?? 'workspace';
    const checksum = readString('checksum');
    if (!name || !packageVersion) throw new Error('Cargo.lock contains a package without a name or version');
    if (checksum !== null && !/^[a-f0-9]{64}$/.test(checksum)) {
      throw new Error(`Cargo.lock contains a malformed checksum for ${name} ${packageVersion}`);
    }
    packages.set(cargoLockKey(name, packageVersion, source), checksum);
  }
  return packages;
}

function cargoLockKey(name, packageVersion, source) {
  return `${name}\u0000${packageVersion}\u0000${source}`;
}

function cargoPackageChecksums(pkg, lockChecksums) {
  if (!pkg.source?.startsWith('registry+')) return [];
  const checksum = lockChecksums.get(cargoLockKey(pkg.name, pkg.version, pkg.source));
  if (!checksum || !/^[a-f0-9]{64}$/.test(checksum)) {
    throw new Error(`Cargo.lock is missing a valid registry checksum for ${pkg.name} ${pkg.version}`);
  }
  return [{ algorithm: 'SHA256', checksumValue: checksum }];
}

function spdxLicense(name, packageVersion, raw, metadata) {
  const value = typeof raw === 'string' ? raw.trim() : '';
  if (!value || value === 'NOASSERTION') {
    if (typeof metadata !== 'string' || !metadata.trim()) return { declared: 'NOASSERTION' };
    return {
      declared: 'NOASSERTION',
      comment: `Package metadata reports license value ${JSON.stringify(metadata.trim())}; it is preserved in the dependency metadata index but is not asserted as an SPDX expression.`,
    };
  }
  // SPDX expressions use AND/OR/WITH; a slash in ecosystem metadata is
  // ambiguous and is not a valid SPDX operator. Preserve it in source
  // metadata, but make no SPDX license assertion for that package.
  if (value === 'UNLICENSED' || value.includes('/')) {
    const verifiedExpression = verifiedCargoSlashLicenses.get(`${name}\u0000${packageVersion}\u0000${value}`);
    if (verifiedExpression) return { declared: verifiedExpression };
    const reported = typeof metadata === 'string' ? metadata : value;
    return {
      declared: 'NOASSERTION',
      comment: `Package metadata reports license value ${JSON.stringify(reported)}; it is preserved in the dependency metadata index but is not asserted as an SPDX expression.`,
    };
  }
  return { declared: value };
}

function npmIntegrityChecksums(integrity) {
  if (typeof integrity !== 'string' || integrity.length === 0) {
    throw new Error('npm package integrity must be a non-empty SRI string');
  }
  const algorithms = new Map([
    ['sha512', { spdx: 'SHA512', bytes: 64 }],
    ['sha384', { spdx: 'SHA384', bytes: 48 }],
    ['sha256', { spdx: 'SHA256', bytes: 32 }],
    ['sha1', { spdx: 'SHA1', bytes: 20 }],
  ]);
  const checksums = new Map();
  for (const token of integrity.trim().split(/\s+/)) {
    const match = /^(sha512|sha384|sha256|sha1)-([A-Za-z0-9+/]+={0,2})(?:\?.*)?$/.exec(token);
    if (!match) throw new Error('npm package integrity contains an unsupported SRI token');
    const algorithm = algorithms.get(match[1]);
    const digest = Buffer.from(match[2], 'base64');
    if (digest.length !== algorithm.bytes
      || digest.toString('base64').replace(/=+$/, '') !== match[2].replace(/=+$/, '')) {
      throw new Error('npm package integrity contains an invalid digest');
    }
    checksums.set(algorithm.spdx, digest.toString('hex'));
  }
  return [...checksums.entries()]
    .sort(([left], [right]) => compare(left, right))
    .map(([algorithm, checksumValue]) => ({ algorithm, checksumValue }));
}
