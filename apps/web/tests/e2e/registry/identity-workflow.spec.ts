import { expect, test, type Browser, type BrowserContext, type Page } from '@playwright/test';

const registryId = process.env.LITECR_E2E_REGISTRY_ID;
const organizationId = process.env.LITECR_E2E_ORGANIZATION_ID;
const runId = process.env.LITECR_E2E_ADMIN_RUN_ID;
const registrarState = process.env.LITECR_E2E_REGISTRAR_STORAGE_STATE;
const reviewerState = process.env.LITECR_E2E_REVIEWER_STORAGE_STATE;

test.describe('authenticated synthetic registration-to-review workflow', () => {
  test.skip(!registryId || !organizationId || !runId || !registrarState || !reviewerState,
    'Requires distinct registrar and reviewer sessions in a disposable synthetic fixture.');

  test('recovers safely when a committed registration response is lost', async ({ browser }) => {
    const registrar = await session(browser, registrarState!);
    let firstResponse: {
      status: number;
      body: {
        data: {
          outcome: string;
          source_record_id: string;
          source_receive_version: number;
          person_public_id: string;
        };
        meta: { replayed?: boolean };
      };
      idempotencyKey: string;
      requestBody: unknown;
    } | undefined;
    let requestCount = 0;

    try {
      await setScope(registrar.page);
      await registrar.page.getByRole('link', { name: 'Open registration form' }).click();

      const sourceSystemId = `litecr-response-loss-${runId}`;
      const sourceRecordKey = `committed-before-response-${runId}`;
      const name = `ທົດສອບ response ສູນຫາຍ ${runId}`;
      await registrar.page.getByLabel('Source system ID').fill(sourceSystemId);
      await registrar.page.getByLabel('Source record key').fill(sourceRecordKey);
      await registrar.page.getByLabel('Source version').fill('1');
      await registrar.page.getByLabel('Name as provided by source').fill(name);
      await registrar.page.getByLabel('Language tag (optional)').fill('lo');
      await registrar.page.getByLabel('Script label (optional)').fill('Lao');
      await registrar.page.getByLabel(/Birth date/).fill('1986-04-19');

      await registrar.page.route('**/api/v1/source-records', async (route) => {
        requestCount += 1;
        if (requestCount !== 1) {
          await route.continue();
          return;
        }

        const upstream = await route.fetch();
        const body = await upstream.json() as NonNullable<typeof firstResponse>['body'];
        firstResponse = {
          status: upstream.status(),
          body,
          idempotencyKey: route.request().headers()['idempotency-key'] ?? '',
          requestBody: route.request().postDataJSON(),
        };
        // The API has committed and returned its response to this proxy handler;
        // abort only the response that would reach the browser.
        await route.abort('failed');
      });

      await registrar.page.getByRole('button', { name: 'Register source record' }).click();
      await expect(registrar.page.getByRole('alert')).toContainText('The service could not be reached. Check the connection and retry.');
      expect(firstResponse).toBeDefined();
      expect(firstResponse!.status).toBe(202);
      expect(firstResponse!.body.meta.replayed).toBe(false);
      expect(firstResponse!.body.data.outcome).toBe('accepted');
      expect(firstResponse!.body.data.source_receive_version).toBe(1);

      // An ambiguous result must not discard the user's source assertions.
      await expect(registrar.page.getByLabel('Source system ID')).toHaveValue(sourceSystemId);
      await expect(registrar.page.getByLabel('Source record key')).toHaveValue(sourceRecordKey);
      await expect(registrar.page.getByLabel('Source version')).toHaveValue('1');
      await expect(registrar.page.getByLabel('Name as provided by source')).toHaveValue(name);
      await expect(registrar.page.getByLabel('Language tag (optional)')).toHaveValue('lo');
      await expect(registrar.page.getByLabel('Script label (optional)')).toHaveValue('Lao');
      await expect(registrar.page.getByLabel(/Birth date/)).toHaveValue('1986-04-19');

      const retryResponse = registrar.page.waitForResponse((response) =>
        response.request().method() === 'POST' && new URL(response.url()).pathname === '/api/v1/source-records');
      await registrar.page.getByRole('button', { name: 'Register source record' }).click();
      const response = await retryResponse;
      expect(response.status()).toBe(200);
      const retryBody = await response.json() as NonNullable<typeof firstResponse>['body'];
      const retryKey = response.request().headers()['idempotency-key'] ?? '';
      expect(retryKey).toBeTruthy();
      expect(retryKey).not.toBe(firstResponse!.idempotencyKey);
      expect(response.request().postDataJSON()).toEqual(firstResponse!.requestBody);
      expect(retryBody.meta.replayed).toBe(true);
      expect(retryBody.data.source_record_id).toBe(firstResponse!.body.data.source_record_id);
      expect(retryBody.data.person_public_id).toBe(firstResponse!.body.data.person_public_id);
      expect(retryBody.data.source_receive_version).toBe(firstResponse!.body.data.source_receive_version);
      expect(retryBody.data.outcome).toBe(firstResponse!.body.data.outcome);

      const receipt = registrar.page.getByRole('status').filter({ hasText: 'Server confirmed:' });
      await expect(receipt).toContainText('accepted');
      await expect(receipt).toContainText(firstResponse!.body.data.person_public_id);
      await expect(registrar.page.getByLabel('Source record key')).toHaveValue('');
      expect(requestCount).toBe(2);
    } finally {
      await registrar.context.close();
    }
  });

  test('registers two source records, searches safely, then opens their possible-duplicate evidence for review', async ({ browser }) => {
    const registrar = await session(browser, registrarState!);
    let reviewer: { context: BrowserContext; page: Page } | undefined;
    try {
      await setScope(registrar.page);
      await setLocale(registrar.page, 'lo');
      await expect(registrar.page.locator('html')).toHaveAttribute('lang', 'lo');
      await expect(registrar.page.getByRole('status').filter({ hasText: 'No published UI catalog was found. English source labels are shown until a reviewed catalog is published.' })).toBeVisible();
      await expect(registrar.page.getByRole('heading', { name: 'Find a registry record' })).toBeVisible();
      await registrar.page.getByRole('link', { name: 'Open registration form' }).click();
      await expect(registrar.page.getByRole('heading', { name: 'Register a source record' })).toBeVisible();

      // The inline field messages and focused summary give the user a recovery path.
      await registrar.page.getByRole('button', { name: 'Register source record' }).click();
      const errorSummary = registrar.page.getByRole('alert').filter({ hasText: 'Fix the following fields before continuing.' });
      await expect(errorSummary).toBeFocused();
      const firstInvalidFieldLink = errorSummary.getByRole('link', { name: /Source system ID/ });
      const sourceSystemField = registrar.page.getByLabel('Source system ID');
      await expect(firstInvalidFieldLink).toHaveAttribute('href', '#source-system-id');
      await expect(sourceSystemField).toHaveAttribute('aria-invalid', 'true');
      // Verify the recovery path is operable with the keyboard: activating the
      // focused summary link must move focus to the invalid control.
      await firstInvalidFieldLink.focus();
      await registrar.page.keyboard.press('Enter');
      await expect(sourceSystemField).toBeFocused();

      const name = `ທົດສອບຄູ່ຊ້ຳ ${runId}`;
      const system = `litecr-usability-${runId}`;
      const first = await registerSource(registrar.page, system, `possible-duplicate-a-${runId}`, name);
      await registrar.context.grantPermissions(['clipboard-read', 'clipboard-write']);
      const firstReceipt = registrar.page.getByRole('status').filter({ hasText: 'Server confirmed:' });
      await expect(firstReceipt).toContainText(first.source_record_id);
      await registrar.page.getByRole('button', { name: 'Copy source record ID' }).click();
      const handedOffSourceId = await registrar.page.evaluate(() => navigator.clipboard.readText());
      expect(handedOffSourceId).toBe(first.source_record_id);
      const second = await registerSource(registrar.page, system, `possible-duplicate-b-${runId}`, name);
      expect(first.source_record_id).not.toBe(second.source_record_id);
      expect(first.person_public_id).not.toBe(second.person_public_id);

      reviewer = await session(browser, reviewerState!);
      await setScope(reviewer.page);
      await setLocale(reviewer.page, 'lo');
      await reviewer.page.getByLabel('Public ID or alias').fill(`missing-${runId}`);
      await reviewer.page.getByRole('button', { name: 'Search records' }).click();
      await expect(reviewer.page.getByText(/No public ID or alias matched.*does not check demographic details/i)).toBeVisible();

      await reviewer.page.getByLabel('Public ID or alias').fill(first.person_public_id);
      await reviewer.page.getByRole('button', { name: 'Search records' }).click();
      await expect(reviewer.page.getByRole('link', { name: 'View source history' })).toBeVisible();
      await reviewer.page.getByRole('link', { name: 'View source history' }).click();
      await expect(reviewer.page.getByRole('heading', { name: first.person_public_id })).toBeVisible();
      const history = reviewer.page.getByRole('heading', { name: 'Source history' }).locator('..');
      await expect(history.getByText(name, { exact: true })).toBeVisible();
      await expect(history.getByText(/Language: lo/)).toBeVisible();
      await expect(history.getByText(/Script: Lao/)).toBeVisible();

      await reviewer.page.goto('/review');
      await reviewer.page.getByLabel('Source record ID to check').fill(handedOffSourceId);
      await reviewer.page.getByRole('button', { name: 'Find candidates' }).click();
      const candidateRow = reviewer.page.getByRole('list', { name: 'Matching candidates' })
        .locator('li').filter({ hasText: second.source_record_id });
      await expect(candidateRow).toBeVisible();
      await candidateRow.getByRole('button', { name: 'Open case' }).click();
      await expect(reviewer.page.getByRole('region', { name: 'Source record comparison' })).toBeVisible();
      await expect(reviewer.page.getByRole('heading', { name: 'Match evidence' })).toBeVisible();
      await expect(reviewer.page.getByText(/Evidence run/)).toBeVisible();
      await expect(reviewer.page.getByText(name, { exact: true })).toHaveCount(2);
      await expect(reviewer.page.getByText(/raw_heuristic_points_not_probability.*are not probabilities/i)).toBeVisible();
      await expect(reviewer.page.getByRole('button', { name: 'Submit decision' })).toBeVisible();
    } finally {
      await Promise.all([registrar.context.close(), ...(reviewer ? [reviewer.context.close()] : [])]);
    }
  });
});

async function session(browser: Browser, storageState: string) {
  const context = await browser.newContext({ storageState });
  const page = await context.newPage();
  await page.goto('/');
  await expect(page.getByRole('button', { name: 'Sign out' })).toBeVisible();
  return { context, page };
}

async function setScope(page: Page) {
  const registry = page.getByLabel('Registry ID');
  if (!(await registry.isVisible())) await page.getByRole('button', { name: 'Edit registry context' }).click();
  await registry.fill(registryId!);
  await page.getByLabel('Organization ID').fill(organizationId!);
  await page.getByRole('button', { name: 'Use this context' }).click();
}

async function setLocale(page: Page, locale: string) {
  const preferences = page.locator('details.workspace-preferences');
  if (!(await preferences.evaluate((element) => (element as HTMLDetailsElement).open))) {
    await preferences.locator('summary').click();
  }
  await page.getByLabel('Language preference').fill(locale);
  await page.getByRole('button', { name: 'Save language' }).click();
  await expect(page.getByRole('status').filter({ hasText: 'Preference saved.' })).toBeVisible();
}

async function registerSource(page: Page, sourceSystemId: string, sourceRecordKey: string, name: string) {
  await page.getByLabel('Source system ID').fill(sourceSystemId);
  await page.getByLabel('Source record key').fill(sourceRecordKey);
  await page.getByLabel('Source version').fill('1');
  await page.getByLabel('Name as provided by source').fill(name);
  await page.getByLabel('Language tag (optional)').fill('lo');
  await page.getByLabel('Script label (optional)').fill('Lao');
  await page.getByLabel(/Birth date/).fill('1986-04-19');
  const response = page.waitForResponse((value) => value.request().method() === 'POST' && new URL(value.url()).pathname === '/api/v1/source-records');
  await page.getByRole('button', { name: 'Register source record' }).click();
  const result = await response;
  expect(result.status()).toBe(202);
  const payload = await result.json() as { data: { source_record_id: string; person_public_id: string; source_receive_version: number; outcome: string } };
  expect(payload.data.source_receive_version).toBe(1);
  expect(payload.data.outcome).toBe('accepted');
  const receipt = page.getByRole('status').filter({ hasText: 'Server confirmed:' });
  await expect(receipt).toContainText('accepted');
  await expect(receipt).toContainText(payload.data.person_public_id);
  return payload.data;
}
