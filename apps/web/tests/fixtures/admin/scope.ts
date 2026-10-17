import { existsSync, readFileSync } from 'node:fs';
import { basename, dirname, resolve } from 'node:path';

const runId = process.env.LITECR_E2E_ADMIN_RUN_ID ?? '';
const fixtureDir = process.env.LITECR_E2E_ADMIN_FIXTURE_DIR ?? '';
const stateFiles = {
  translator: process.env.LITECR_E2E_TRANSLATOR_STORAGE_STATE ?? '',
  reviewer: process.env.LITECR_E2E_REVIEWER_STORAGE_STATE ?? '',
  publisher: process.env.LITECR_E2E_PUBLISHER_STORAGE_STATE ?? '',
  administrator: process.env.LITECR_E2E_ADMINISTRATOR_STORAGE_STATE ?? '',
};

export const adminFixture = {
  runId,
  databaseName: process.env.LITECR_E2E_ADMIN_DATABASE_NAME ?? '',
  registryId: `litecr-admin-e2e-${runId}`,
  organizationId: `litecr-admin-org-${runId}`,
  locale: process.env.LITECR_E2E_ADMIN_LOCALE ?? '',
  stateFiles,
  // A missing local fixture remains a skip for normal web development. Once
  // any fixture configuration is supplied, invalid/incomplete setup is a hard
  // failure in beforeAll rather than a misleading skipped acceptance run.
  started: Boolean(runId || fixtureDir || process.env.LITECR_E2E_ADMIN_DATABASE_NAME ||
    process.env.LITECR_E2E_ADMIN_LOCALE || Object.values(stateFiles).some(Boolean)),
  ready: Boolean(runId && fixtureDir && process.env.LITECR_E2E_ADMIN_DATABASE_NAME &&
    process.env.LITECR_E2E_ADMIN_LOCALE && Object.values(stateFiles).every(Boolean)),
};

export function validateAdminFixture(): void {
  const errors: string[] = [];
  if (!/^[a-f0-9]{16}$/.test(adminFixture.runId)) errors.push('LITECR_E2E_ADMIN_RUN_ID must be 16 lowercase hex characters.');
  if (adminFixture.databaseName !== `litecr_admin_e2e_${adminFixture.runId}`) errors.push('The database must be the fresh run-scoped litecr_admin_e2e_<run-id> database.');
  if (process.env.LITECR_E2E_ADMIN_DISPOSABLE_DATABASE !== 'true') errors.push('Set LITECR_E2E_ADMIN_DISPOSABLE_DATABASE=true only after verifying this database is disposable.');
  if (process.env.LITECR_E2E_ADMIN_ALLOW_DATABASE_DROP !== 'true') errors.push('Set LITECR_E2E_ADMIN_ALLOW_DATABASE_DROP=true to authorize runner cleanup of this exact disposable database.');
  if (!fixtureDir || !existsSync(fixtureDir)) errors.push('LITECR_E2E_ADMIN_FIXTURE_DIR must point to the private per-run fixture directory.');
  if (fixtureDir && (basename(resolve(fixtureDir)) !== adminFixture.runId ||
      basename(dirname(resolve(fixtureDir))) !== 'admin-e2e' ||
      basename(dirname(dirname(resolve(fixtureDir)))) !== '.litecr-dev')) {
    errors.push('The per-run storage states must be located below ~/.litecr-dev/admin-e2e/<run-id>.');
  }

  const rolePaths = Object.entries(stateFiles);
  const uniquePaths = new Set(rolePaths.map(([, file]) => file.toLowerCase()));
  if (uniquePaths.size !== rolePaths.length) errors.push('Translator, reviewer, publisher and administrator need four distinct storage-state paths.');
  for (const [role, file] of rolePaths) {
    if (fixtureDir && file && resolve(file) !== resolve(fixtureDir, `${role}.json`)) {
      errors.push(`${role} state must be the exact ${role}.json file inside this disposable run directory.`);
    }
    if (!file || !existsSync(file)) {
      errors.push(`Missing ${role} storage state.`);
      continue;
    }
    try {
      const state = JSON.parse(readFileSync(file, 'utf8')) as { cookies?: Array<{ name?: string; value?: string }> };
      const session = state.cookies?.find((cookie) => cookie.name === 'litecr_session')?.value;
      if (!session) errors.push(`${role} state has no liteCR browser session cookie.`);
    } catch {
      errors.push(`${role} storage state is not valid Playwright JSON.`);
    }
  }
  if (errors.length) throw new Error(`Unsafe/incomplete admin E2E fixture:\n- ${errors.join('\n- ')}`);
}
