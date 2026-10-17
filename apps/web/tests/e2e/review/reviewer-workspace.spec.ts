import { expect, test, type Page } from '@playwright/test';

const registryId = process.env.LITECR_E2E_REGISTRY_ID;
const organizationId = process.env.LITECR_E2E_ORGANIZATION_ID;
const caseId = process.env.LITECR_E2E_REVIEW_CASE_ID;
const evidenceRunId = process.env.LITECR_E2E_REVIEW_EVIDENCE_RUN_ID;
const decisionCaseId = process.env.LITECR_E2E_REVIEW_DECISION_CASE_ID;
const runId = process.env.LITECR_E2E_ADMIN_RUN_ID;
const enabled = Boolean(registryId && organizationId && caseId && decisionCaseId && process.env.LITECR_E2E_STORAGE_STATE);

test.describe('authenticated synthetic review workspace', () => {
  test.skip(!enabled, 'Requires an authenticated session and a pre-seeded review case in an isolated synthetic database.');

  test('keyboard reaches a later active-queue page and selects a newly revealed case', async ({ page }) => {
    const row = (caseId: string) => ({
      case_id: caseId, registry_id: registryId, organization_id: organizationId,
      query_source_record_id: 'query-source', candidate_source_record_id: 'candidate-source',
      evidence_run_id: null, query_receive_version: 1, candidate_receive_version: 1,
      query_person_version: 1, candidate_person_version: 1, case_version: 1,
      state: 'open', assigned_actor_id: null,
    });
    const firstPage = Array.from({ length: 50 }, (_, index) => row(`case-${String(index).padStart(2, '0')}`));
    await page.route('**/api/v1/review-cases**', async (route) => {
      const url = new URL(route.request().url());
      if (url.pathname === '/api/v1/review-cases' && url.searchParams.has('after_created_at')) {
        await route.fulfill({ json: { data: [row('case-newest')], meta: { has_more: false, next_cursor: null } } });
      } else if (url.pathname === '/api/v1/review-cases') {
        await route.fulfill({ json: { data: firstPage, meta: { has_more: true, next_cursor: {
          queue_filter: 'active', after_created_at: '2026-10-09T12:34:56.123456Z', after_case_id: '123e4567-e89b-12d3-a456-426614174000',
        } } } });
      } else if (url.pathname === '/api/v1/review-cases/case-newest') {
        await route.fulfill({ json: { data: { case: row('case-newest'), history: [] } } });
      } else {
        await route.fulfill({ status: 404, json: { error: { code: 'NOT_FOUND', message: 'Synthetic route not configured.' } } });
      }
    });
    await page.route('**/api/v1/registries/**/source-records/**', async (route) => {
      const sourceId = new URL(route.request().url()).pathname.split('/').at(-1)!;
      await route.fulfill({ json: { data: {
        public_id: `public-${sourceId}`, resource_version: 1, source_record_ids: [sourceId], aliases: [],
        source_system_id: 'synthetic-source', source_version: 'v1', receive_version: 1,
        assertions: [{ source_record_id: sourceId, source_system_id: 'synthetic-source', source_version: 'v1', receive_version: 1,
          field_path: 'name', original_value: `Synthetic ${sourceId}`, language_tag: 'en', script: 'Latn' }],
      } } });
    });

    await page.goto('/review');
    await setScope(page);
    const loadMore = page.getByRole('button', { name: 'Load more cases' });
    await loadMore.focus();
    await page.keyboard.press('Enter');
    const revealed = page.getByRole('button', { name: /case-newest/ });
    await expect(revealed).toBeVisible();
    await revealed.focus();
    await page.keyboard.press('Enter');
    await expect(revealed).toHaveAttribute('aria-current', 'true');
    await expect(page.getByRole('heading', { name: 'Case case-newest', exact: true })).toBeVisible();
  });

  test('opens a case and renders its exact saved evidence and receive-version source assertions', async ({ page }) => {
    expect(evidenceRunId, 'The isolated seed must bind this case to a saved evidence run.').toMatch(/^[a-f0-9-]{36}$/);
    expect(runId, 'The isolated fixture run ID is required for its synthetic source sentinels.').toMatch(/^[a-f0-9]{16}$/);
    const observedRequests: Array<{ method: string; path: string }> = [];
    let evidenceResponse: { status: number; body: unknown } | undefined;
    page.on('request', (request) => {
      const url = new URL(request.url());
      if (url.pathname.startsWith('/api/v1/match')) observedRequests.push({ method: request.method(), path: url.pathname });
    });
    page.on('response', async (response) => {
      const url = new URL(response.url());
      if (url.pathname === `/api/v1/match/evidence/${evidenceRunId}`) {
        evidenceResponse = { status: response.status(), body: await response.json().catch(() => null) };
      }
    });
    await page.goto('/review');
    await setScope(page);
    await expect(page.getByRole('heading', { name: 'Review workspace' })).toBeVisible();
    await expect(page.getByRole('complementary', { name: 'Review case queue' })).toBeVisible();
    const queueCase = page.getByRole('button', { name: new RegExp(caseId!) });
    await expect(queueCase).toContainText(caseId!);
    await queueCase.click();
    await expect(page.getByRole('heading', { name: /Case/ })).toBeVisible();
    await expect(page.getByRole('region', { name: 'Source record comparison' })).toBeVisible();
    await expect(page.getByRole('heading', { name: 'Original assertions' }).first()).toBeVisible();
    await expect(page.getByRole('heading', { name: 'Match evidence' })).toBeVisible();
    await expect(page.getByText(evidenceRunId!, { exact: true })).toBeVisible();
    const versionOneName = `liteCR evidence v1 ${runId}`;
    await expect(page.getByText(versionOneName, { exact: true })).toHaveCount(2);
    await expect(page.getByText(`Later changed query ${runId}`, { exact: true })).toHaveCount(0);
    await expect(page.getByText(`Later changed candidate ${runId}`, { exact: true })).toHaveCount(0);
    await expect.poll(() => evidenceResponse).toMatchObject({ status: 200 });
    const evidencePayload = (evidenceResponse?.body as { data?: { evidence_run_id?: string; query_receive_version?: number; candidates?: Array<{ candidate_receive_version?: number }> } } | undefined)?.data;
    expect(evidencePayload?.evidence_run_id).toBe(evidenceRunId);
    expect(evidencePayload?.query_receive_version).toBe(1);
    expect(evidencePayload?.candidates?.some((item) => item.candidate_receive_version === 1)).toBe(true);
    expect(observedRequests).toContainEqual({ method: 'GET', path: `/api/v1/match/evidence/${evidenceRunId}` });
    expect(observedRequests).not.toContainEqual({ method: 'POST', path: '/api/v1/match' });
    await expect(page.getByRole('button', { name: 'Submit decision' })).toBeVisible();
    await expect(page.getByText(/This score only ranks cases for review\. It does not estimate probability or decide identity;/)).toBeVisible();
    const reviewerCopy = await page.locator('.review-page').innerText();
    expect(reviewerCopy).not.toMatch(/raw_heuristic_points_not_probability|mutation_authority|registry-only/i);

    await page.setViewportSize({ width: 375, height: 812 });
    const ltrLayout = await layoutOverflow(page);
    expect(ltrLayout.scrollWidth, JSON.stringify(ltrLayout)).toBeLessThanOrEqual(ltrLayout.viewportWidth);
    await page.locator('html').evaluate((element) => element.setAttribute('dir', 'rtl'));
    await expect(page.getByRole('region', { name: 'Source record comparison' })).toBeVisible();
    const rtlLayout = await layoutOverflow(page);
    expect(rtlLayout.scrollWidth, JSON.stringify(rtlLayout)).toBeLessThanOrEqual(rtlLayout.viewportWidth);
  });

  test('Tab reaches the decision submit button with a visible focus indicator', async ({ page }) => {
    await page.goto('/review');
    await setScope(page);
    const queueCase = page.getByRole('button', { name: new RegExp(decisionCaseId!) });
    await expect(queueCase).toContainText(decisionCaseId!);
    await queueCase.click();
    await expect(page.getByRole('heading', { name: 'Match evidence' })).toBeVisible();

    const decisionRadio = page.getByRole('radio', { name: /Not the same person/ });
    await decisionRadio.check();
    await page.getByLabel('Reason code').fill('synthetic_focus_check');
    await page.getByLabel('Decision reason').fill('Keyboard focus check only; no decision was submitted.');

    // Begin within the form, then use real keyboard Tab input through the remaining controls.
    await decisionRadio.focus();
    const submit = page.getByRole('button', { name: 'Submit decision' });
    let reachedByTab = false;
    for (let index = 0; index < 8; index++) {
      await page.keyboard.press('Tab');
      if (await submit.evaluate((element) => element === document.activeElement)) {
        reachedByTab = true;
        break;
      }
    }

    expect(reachedByTab, 'Tab navigation should reach the enabled decision submit button.').toBe(true);
    const focusState = await submit.evaluate((element) => {
      const style = getComputedStyle(element);
      return {
        focusVisible: element.matches(':focus-visible'),
        outlineStyle: style.outlineStyle,
        outlineWidth: style.outlineWidth,
        outlineColor: style.outlineColor,
      };
    });
    expect(focusState.focusVisible, JSON.stringify(focusState)).toBe(true);
    expect(focusState.outlineStyle, JSON.stringify(focusState)).not.toBe('none');
    expect(Number.parseFloat(focusState.outlineWidth), JSON.stringify(focusState)).toBeGreaterThan(0);
  });

  test('a failed decision brings its recovery message into view and focus', async ({ page }) => {
    await page.route(`**/api/v1/review-cases/${encodeURIComponent(decisionCaseId!)}/decisions`, (route) => route.fulfill({
      status: 503,
      json: { error: { code: 'REVIEW_UNAVAILABLE', message: 'The review service is temporarily unavailable.' } },
    }));
    await page.setViewportSize({ width: 375, height: 812 });
    await page.goto('/review');
    await setScope(page);
    const queueCase = page.getByRole('button', { name: new RegExp(decisionCaseId!) });
    await queueCase.click();
    await expect(page.getByRole('heading', { name: 'Match evidence' })).toBeVisible();
    await page.getByRole('radio', { name: /Not the same person/ }).check();
    await page.getByLabel('Reason code').fill('synthetic_failure_recovery');
    await page.getByLabel('Decision reason').fill('Synthetic request was rejected before reaching the review service.');
    await page.getByRole('button', { name: 'Submit decision' }).click();

    const recovery = page.locator('.decision-error');
    await expect(recovery).toBeVisible();
    await expect(recovery).toBeFocused();
    await expect(recovery).toContainText('The service is temporarily unavailable. Your changes were not confirmed.');
    await expect(recovery).toBeInViewport({ ratio: 1 });
    await expect(page.getByRole('button', { name: 'Retry same request safely' })).toBeVisible();
  });

  test('complete queue identifiers remain distinct without narrow-screen overflow', async ({ page }) => {
    const first = {
      case_id: 'case-id-0000-middle-ALPHA-98765432', registry_id: registryId!, organization_id: organizationId!,
      query_source_record_id: 'source-id-00-name-ALPHA-87654321', candidate_source_record_id: 'source-id-00-target-ALPHA-12345678',
      evidence_run_id: null, query_receive_version: 1, candidate_receive_version: 1,
      query_person_version: 1, candidate_person_version: 1, case_version: 1, state: 'open', assigned_actor_id: null,
    };
    const second = {
      ...first, case_id: 'case-id-0000-middle-BRAVO-98765432',
      query_source_record_id: 'source-id-00-name-BRAVO-87654321', candidate_source_record_id: 'source-id-00-target-BRAVO-12345678',
    };
    await page.route('**/api/v1/review-cases**', (route) => route.fulfill({ json: { data: [first, second], meta: { has_more: false, next_cursor: null } } }));
    await page.setViewportSize({ width: 375, height: 812 });
    await page.goto('/review');
    await setScope(page);

    const firstRow = page.getByRole('button', { name: new RegExp(first.case_id) });
    const secondRow = page.getByRole('button', { name: new RegExp(second.case_id) });
    await expect(firstRow).toContainText(first.query_source_record_id);
    await expect(firstRow).toContainText(first.candidate_source_record_id);
    await expect(secondRow).toContainText(second.query_source_record_id);
    await expect(secondRow).toContainText(second.candidate_source_record_id);
    const width = await page.evaluate(() => ({ viewport: document.documentElement.clientWidth, content: document.documentElement.scrollWidth }));
    expect(width.content, JSON.stringify(width)).toBeLessThanOrEqual(width.viewport);
  });

  test('commits a confirmed non-match, rejects stale versions, and safely replays the same request', async ({ page, browser }) => {
    await page.goto('/review');
    await setScope(page);
    const queueCase = page.getByRole('button', { name: new RegExp(decisionCaseId!) });
    await expect(queueCase).toContainText(decisionCaseId!);
    await queueCase.click();
    await expect(page.getByRole('heading', { name: 'Match evidence' })).toBeVisible();

    const initial = await getCaseDetail(page, decisionCaseId!);
    expect(initial.case.state).toBe('open');
    const decisionUrl = `/api/v1/review-cases/${encodeURIComponent(decisionCaseId!)}/decisions`;
    const decisionBody = {
      registry_id: registryId!,
      organization_id: organizationId!,
      expected_case_version: initial.case.case_version,
      expected_query_receive_version: initial.case.query_receive_version,
      expected_candidate_receive_version: initial.case.candidate_receive_version,
      expected_query_person_version: initial.case.query_person_version,
      expected_candidate_person_version: initial.case.candidate_person_version,
      decision: 'confirmed_non_match',
      reason_code: 'synthetic_e2e_adjudication',
      reason: 'Synthetic acceptance: source records represent different people.',
      idempotency_key: crypto.randomUUID(),
    };

    const stale = await postJson(page, decisionUrl, {
      ...decisionBody,
      expected_case_version: initial.case.case_version + 1,
      idempotency_key: crypto.randomUUID(),
    });
    expect(stale.status).toBe(409);
    expect(stale.code).toBe('REVIEW_CONFLICT');
    expect((await getCaseDetail(page, decisionCaseId!)).history).toHaveLength(0);

    await page.getByRole('radio', { name: /Not the same person/ }).check();
    await page.getByLabel('Reason code').fill(decisionBody.reason_code);
    await page.getByLabel('Decision reason').fill(decisionBody.reason);
    const submitted = page.waitForRequest((request) => request.method() === 'POST' && request.url().includes(decisionUrl));
    await page.getByRole('button', { name: 'Submit decision' }).click();
    const decisionRequest = await submitted;
    const committedBody = JSON.parse(decisionRequest.postData() ?? '{}') as typeof decisionBody;
    expect(committedBody.decision).toBe('confirmed_non_match');
    expect(committedBody.reason_code).toBe(decisionBody.reason_code);
    expect(committedBody.reason).toBe(decisionBody.reason);

    await expect(page.getByRole('heading', { name: 'Decision history' })).toBeVisible();
    await expect(page.getByText('Synthetic acceptance: source records represent different people.')).toBeVisible();
    const committed = await getCaseDetail(page, decisionCaseId!);
    expect(committed.case.state).toBe('decided');
    expect(committed.case.case_version).toBe(initial.case.case_version + 1);
    expect(committed.history).toHaveLength(1);
    expect(committed.history[0]).toMatchObject({
      decision: 'confirmed_non_match',
      outcome: 'accepted',
      reason_code: decisionBody.reason_code,
      reason: decisionBody.reason,
    });

    const replay = await postJson(page, decisionUrl, committedBody);
    expect(replay.status).toBe(200);
    expect(replay.replayed).toBe(true);
    expect(replay.decisionId).toBe(committed.history[0].decision_id);
    const afterReplay = await getCaseDetail(page, decisionCaseId!);
    expect(afterReplay.history).toHaveLength(1);
    expect(afterReplay.history[0].decision_id).toBe(committed.history[0].decision_id);

    const registrarState = process.env.LITECR_E2E_REGISTRAR_STORAGE_STATE;
    expect(registrarState, 'The isolated fixture must provide a distinct registrar session.').toBeTruthy();
    const registrarContext = await browser.newContext({ storageState: registrarState });
    try {
      const registrarPage = await registrarContext.newPage();
      await registrarPage.goto('/');
      const denied = await postJson(registrarPage, decisionUrl, committedBody);
      expect(denied.status).toBe(403);
      expect(denied.code).toBe('ACCESS_DENIED');
    } finally {
      await registrarContext.close();
    }
  });
});

async function getCaseDetail(page: Page, requestedCaseId: string) {
  return page.evaluate(async ({ caseId, registryId, organizationId }) => {
    const query = new URLSearchParams({ registry_id: registryId!, organization_id: organizationId! });
    const response = await fetch(`/api/v1/review-cases/${encodeURIComponent(caseId!)}?${query}`, {
      credentials: 'same-origin', cache: 'no-store',
    });
    if (!response.ok) throw new Error(`Review case read returned ${response.status}.`);
    const envelope = await response.json() as { data: {
      case: { state: string; case_version: number; query_receive_version: number; candidate_receive_version: number; query_person_version: number; candidate_person_version: number };
      history: Array<{ decision_id: string; decision: string; outcome: string; reason_code: string; reason: string }>;
    } };
    return envelope.data;
  }, { caseId: requestedCaseId, registryId, organizationId });
}

async function postJson(page: Page, url: string, body: object) {
  return page.evaluate(async ({ url, body }) => {
    const csrf = document.cookie.split('; ').find((part) => part.startsWith('litecr_csrf='))?.slice('litecr_csrf='.length);
    if (!csrf) throw new Error('Authenticated browser state has no CSRF cookie.');
    const response = await fetch(url, {
      method: 'POST', credentials: 'same-origin',
      headers: { 'content-type': 'application/json', 'x-csrf-token': decodeURIComponent(csrf) },
      body: JSON.stringify(body),
    });
    const payload = await response.json().catch(() => null) as {
      error?: { code?: string }; data?: { decision_id?: string | null }; meta?: { replayed?: boolean };
    } | null;
    return {
      status: response.status,
      code: payload?.error?.code,
      replayed: payload?.meta?.replayed,
      decisionId: payload?.data?.decision_id,
    };
  }, { url, body });
}

async function layoutOverflow(page: Page) {
  return page.evaluate(() => ({
    viewportWidth: window.innerWidth,
    scrollWidth: document.documentElement.scrollWidth,
    mobileBreakpoint: window.matchMedia('(max-width: 760px)').matches,
    layoutAncestors: (() => {
      const result = [];
      let element = document.querySelector('.review-case-header');
      while (element && result.length < 8) {
        const style = getComputedStyle(element);
        const bounds = element.getBoundingClientRect();
        result.push({
          tag: element.tagName.toLowerCase(),
          className: typeof element.className === 'string' ? element.className.slice(0, 80) : '',
          left: Math.round(bounds.left),
          right: Math.round(bounds.right),
          width: Math.round(bounds.width),
          clientWidth: element.clientWidth,
          scrollWidth: element.scrollWidth,
          minWidth: style.minWidth,
          gridTemplateColumns: style.gridTemplateColumns,
        });
        element = element.parentElement;
      }
      return result;
    })(),
    overflowing: Array.from(document.querySelectorAll('body *'))
      .filter((element) => element.getBoundingClientRect().right > window.innerWidth + 1)
      .slice(0, 12)
      .map((element) => ({
        tag: element.tagName.toLowerCase(),
        className: typeof element.className === 'string' ? element.className.slice(0, 80) : '',
        right: Math.round(element.getBoundingClientRect().right),
        width: Math.round(element.getBoundingClientRect().width),
        overflowX: getComputedStyle(element).overflowX,
      })),
  }));
}

async function setScope(page: Page) {
  await expect(page.getByRole('button', { name: 'Sign out' })).toBeVisible();
  const registryField = page.getByLabel('Registry ID');
  if (!(await registryField.isVisible())) {
    await page.getByRole('button', { name: 'Edit registry context' }).click();
    await expect(registryField).toBeVisible();
  }
  await registryField.fill(registryId!);
  await page.getByLabel('Organization ID').fill(organizationId!);
  await page.getByRole('button', { name: 'Use this context' }).click();
}

