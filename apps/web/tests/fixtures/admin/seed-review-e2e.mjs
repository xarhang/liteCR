import { readFile, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { chromium } from '@playwright/test';

const runId = process.env.LITECR_E2E_ADMIN_RUN_ID ?? '';
const fixtureDirectory = process.env.LITECR_E2E_ADMIN_FIXTURE_DIR ?? '';
const baseURL = process.env.LITECR_E2E_BASE_URL ?? 'http://127.0.0.1:4173';
const registryId = `litecr-admin-e2e-${runId}`;
const organizationId = `litecr-admin-org-${runId}`;
const expectedFixtureDirectory = `\\\\wsl.localhost\\Ubuntu\\home\\xarhang\\.litecr-dev\\admin-e2e\\${runId}`;
if (!/^[a-f0-9]{16}$/.test(runId) || fixtureDirectory.toLowerCase() !== expectedFixtureDirectory.toLowerCase()) {
  throw new Error('Use the exact private per-run WSL-home fixture directory.');
}

const browser = await chromium.launch({ headless: true });
try {
  const registrar = await authorizedPage('registrar');
  const sourceSystem = `litecr-admin-review-e2e-${runId}`;
  const query = await createSource(registrar.page, {
    registry_id: registryId,
    organization_id: organizationId,
    source_system_id: sourceSystem,
    source_record_key: `query-${runId}`,
    source_version: '1',
    assertions: [
      { field_path: 'name', original_value: `liteCR evidence v1 ${runId}`, language_tag: 'en', script: 'Latin' },
      { field_path: 'birth_date', original_value: '1986-04' },
    ],
    identifiers: [],
  });
  const candidate = await createSource(registrar.page, {
    registry_id: registryId,
    organization_id: organizationId,
    source_system_id: sourceSystem,
    source_record_key: `candidate-${runId}`,
    source_version: '1',
    assertions: [
      { field_path: 'name', original_value: `liteCR evidence v1 ${runId}`, language_tag: 'en', script: 'Latin' },
      { field_path: 'birth_date', original_value: '1986-04-19' },
    ],
    identifiers: [],
  });
  const reviewer = await authorizedPage('reviewer');
  const historical = await createMatchCase(reviewer.page, registryId, organizationId, query, candidate);

  for (const [recordKey, changedName] of [
    [`query-${runId}`, `Later changed query ${runId}`],
    [`candidate-${runId}`, `Later changed candidate ${runId}`],
  ]) {
    await createSource(registrar.page, {
      registry_id: registryId,
      organization_id: organizationId,
      source_system_id: sourceSystem,
      source_record_key: recordKey,
      source_version: '2',
      assertions: [
        { field_path: 'name', original_value: changedName, language_tag: 'en', script: 'Latin' },
        { field_path: 'birth_date', original_value: '1986-04' },
      ],
      identifiers: [],
    }, 2);
  }
  const queryV2 = { ...query, source_receive_version: 2 };
  const candidateV2 = { ...candidate, source_receive_version: 2 };
  const current = await createMatchCase(reviewer.page, registryId, organizationId, queryV2, candidateV2);
  await reviewer.context.close();
  await registrar.context.close();

  const seed = {
    run_id: runId,
    case_id: historical.caseId,
    evidence_run_id: historical.evidenceRunId,
    decision_case_id: current.caseId,
  };
  await writeFile(path.join(fixtureDirectory, 'review-case.json'), `${JSON.stringify(seed)}\n`, { encoding: 'utf8', mode: 0o600, flag: 'wx' });
  process.stdout.write('seeded isolated historical-evidence and current-decision review cases\n');
} finally {
  await browser.close();
}

async function createMatchCase(page, registryId, organizationId, query, candidate) {
  const match = await page.evaluate(async ({ registryId, organizationId, query }) => {
    const csrf = document.cookie.split('; ').find((part) => part.startsWith('litecr_csrf='))?.slice('litecr_csrf='.length);
    if (!csrf) return { status: 0, code: 'CSRF_COOKIE_MISSING' };
    const result = await fetch('/api/v1/match', {
      method: 'POST', credentials: 'same-origin',
      headers: { 'content-type': 'application/json', 'x-csrf-token': decodeURIComponent(csrf) },
      body: JSON.stringify({ registry_id: registryId, organization_id: organizationId, query_source_record_id: query.source_record_id, candidate_limit: 50 }),
    });
    let body;
    try { body = await result.json(); } catch { body = null; }
    return { status: result.status, code: body?.error?.code ?? null, result: body?.data ?? null };
  }, { registryId, organizationId, query });
  if (match.status !== 200 || !/^[a-f0-9-]{36}$/.test(match.result?.evidence_run_id ?? '')) {
    throw new Error(`Synthetic matching evidence creation failed (HTTP ${match.status}, code ${match.code ?? 'none'}).`);
  }
  const matchedCandidate = match.result.candidates?.find((item) => item.candidate_record_id === candidate.source_record_id);
  if (!matchedCandidate || matchedCandidate.candidate_receive_version !== candidate.source_receive_version || match.result.query_receive_version !== query.source_receive_version) {
    throw new Error('The synthetic match snapshot did not contain the exact seeded source receive versions.');
  }
  const response = await page.evaluate(async ({ registryId, organizationId, query, candidate, match }) => {
    const csrf = document.cookie.split('; ').find((part) => part.startsWith('litecr_csrf='))?.slice('litecr_csrf='.length);
    if (!csrf) return { status: 0, code: 'CSRF_COOKIE_MISSING', caseId: null };
    const result = await fetch('/api/v1/review-cases', {
      method: 'POST', credentials: 'same-origin',
      headers: { 'content-type': 'application/json', 'x-csrf-token': decodeURIComponent(csrf) },
      body: JSON.stringify({
        registry_id: registryId, organization_id: organizationId,
        query_source_record_id: query.source_record_id,
        candidate_source_record_id: candidate.source_record_id,
        expected_query_receive_version: query.source_receive_version,
        expected_candidate_receive_version: candidate.source_receive_version,
        evidence_run_id: match.evidence_run_id,
      }),
    });
    let body;
    try { body = await result.json(); } catch { body = null; }
    return { status: result.status, code: body?.error?.code ?? null, caseId: body?.data?.case_id ?? null };
  }, { registryId, organizationId, query, candidate, match: match.result });
  if (response.status !== 201 || !/^[a-f0-9-]{36}$/.test(response.caseId ?? '')) {
    throw new Error(`Review case creation failed (HTTP ${response.status}, code ${response.code ?? 'none'}).`);
  }
  return { caseId: response.caseId, evidenceRunId: match.result.evidence_run_id };
}

async function authorizedPage(role) {
  const storageState = path.join(fixtureDirectory, `${role}.json`);
  const context = await browser.newContext({ storageState });
  const page = await context.newPage();
  await page.goto(baseURL);
  const session = await page.evaluate(async () => {
    const response = await fetch('/api/v1/access/session', { credentials: 'same-origin', cache: 'no-store' });
    return { status: response.status };
  });
  if (session.status !== 200) {
    await context.close();
    throw new Error(`The isolated ${role} session is not authenticated.`);
  }
  return { context, page };
}

async function createSource(page, payload, expectedReceiveVersion = 1) {
  const response = await page.evaluate(async (body) => {
    const csrf = document.cookie.split('; ').find((part) => part.startsWith('litecr_csrf='))?.slice('litecr_csrf='.length);
    if (!csrf) return { status: 0, code: 'CSRF_COOKIE_MISSING' };
    const result = await fetch('/api/v1/source-records', {
      method: 'POST',
      credentials: 'same-origin',
      headers: {
        'content-type': 'application/json',
        'idempotency-key': crypto.randomUUID(),
        'x-csrf-token': decodeURIComponent(csrf),
      },
      body: JSON.stringify(body),
    });
    let data;
    try { data = await result.json(); } catch { data = null; }
    return {
      status: result.status,
      code: data?.error?.code ?? null,
      source_record_id: data?.data?.source_record_id ?? null,
      source_receive_version: data?.data?.source_receive_version ?? null,
    };
  }, payload);
  if (response.status !== 202 || !response.source_record_id || response.source_receive_version !== expectedReceiveVersion) {
    throw new Error(`Synthetic source creation failed (HTTP ${response.status}, code ${response.code ?? 'none'}).`);
  }
  return response;
}
