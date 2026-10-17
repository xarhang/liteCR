import { expect, test, type Browser, type BrowserContext, type Page } from '@playwright/test';
import { adminFixture, validateAdminFixture } from '../../fixtures/admin/scope';

test.describe('role-separated admin workflows on a disposable synthetic database', () => {
  test.skip(!adminFixture.started, 'Run tests/fixtures/admin/run-admin-e2e.ps1 with a fresh database and four independent OIDC sessions.');

  test.beforeAll(() => {
    if (adminFixture.started) validateAdminFixture();
  });

  test('enforces author/reviewer/publisher separation and proves live bundle publish/rollback', async ({ browser }) => {
    const invocationId = crypto.randomUUID().replaceAll('-', '').slice(0, 8);
    const locale = `${adminFixture.locale}-x-${invocationId}`;
    const namespace = `e2e_${adminFixture.runId}_${invocationId}`;
    const key = `label_${adminFixture.runId}_${invocationId}`;
    const translator = await session(browser, adminFixture.stateFiles.translator);
    const reviewer = await session(browser, adminFixture.stateFiles.reviewer);
    const publisher = await session(browser, adminFixture.stateFiles.publisher);
    try {
      await expectDistinctActors([translator.page, reviewer.page, publisher.page]);

      await openAdmin(translator.page, '/admin/translations');
      const locales = translator.page.getByLabel('Active locale');
      await expect(locales.locator(`option[value="${locale}"]`)).toHaveCount(0);
      await translator.page.getByLabel('BCP 47 locale tag').fill(locale);
      await translator.page.getByRole('button', { name: 'Add locale', exact: true }).click();
      await expect(locales.locator(`option[value="${locale}"]`)).toHaveCount(1);
      await locales.selectOption(locale);
      await translator.page.getByLabel('Text group', { exact: true }).fill(namespace);
      await translator.page.getByLabel('Message ID', { exact: true }).fill(key);
      await translator.page.getByRole('button', { name: 'Load message', exact: true }).click();
      await expect(translator.page.getByRole('status').filter({ hasText: 'No draft exists for this key.' })).toBeVisible();
      await translator.page.getByLabel('Source text').fill('Hello {{name}}');
      await translator.page.getByLabel('Required placeholders').fill('name');
      await translator.page.getByRole('button', { name: 'Save source key' }).click();
      await expect(translator.page.getByRole('status').filter({ hasText: 'Source message saved.' })).toBeVisible();
      await translator.page.getByLabel('Translation draft').fill('Xin chào {{name}}');
      await translator.page.getByRole('button', { name: 'Save draft' }).click();
      await expect(translator.page.getByRole('status').filter({ hasText: 'Draft saved.' })).toBeVisible();

      const translationBase = translationUrl(locale, namespace, key);
      await expectDenied(translator.page, `${translationBase}/review`, { decision: 'approve' });
      await expectDenied(translator.page, `/api/v1/config/${adminFixture.registryId}/${adminFixture.organizationId}/locales/${locale}/${namespace}/publish`, { expected_revision: null });

      await openAdmin(reviewer.page, '/admin/translations');
      await selectTranslation(reviewer.page, namespace, key, locale);
      await reviewer.page.getByRole('button', { name: 'Approve draft' }).click();
      await expect(reviewer.page.getByRole('status').filter({ hasText: 'Draft approved.' })).toBeVisible();
      const bundleUrl = `/api/v1/config/${adminFixture.registryId}/${adminFixture.organizationId}/locales/${locale}/${namespace}/publish`;
      await expectDenied(reviewer.page, bundleUrl, { expected_revision: null });

      await openAdmin(publisher.page, '/admin/translations');
      await selectTranslation(publisher.page, namespace, key, locale);
      await publisher.page.getByRole('button', { name: 'Publish translations' }).click();
      const firstPublishConfirmation = publisher.page.getByRole('alert').filter({ hasText: /all messages in the .* text group .* not just the message open now/i });
      await expect(firstPublishConfirmation).toContainText('Current published revision: not loaded');
      await expect(firstPublishConfirmation.getByRole('button', { name: 'Confirm publish' })).toBeVisible();
      await expect(publisher.page.getByRole('status').filter({ hasText: 'Published revision 1' })).toHaveCount(0);
      await firstPublishConfirmation.getByRole('button', { name: 'Confirm publish' }).click();
      await expect(publisher.page.getByRole('status').filter({ hasText: 'Published as revision 1.' })).toBeVisible();
      await expect.poll(() => publishedValue(publisher.page, locale, namespace, key)).toBe('Xin chào {{name}}');

      await translator.page.getByLabel('Translation draft').fill('Hola {{name}}');
      await translator.page.getByRole('button', { name: 'Save draft' }).click();
      await selectTranslation(reviewer.page, namespace, key, locale);
      await reviewer.page.getByRole('button', { name: 'Approve draft' }).click();
      await selectTranslation(publisher.page, namespace, key, locale);
      await publisher.page.getByRole('button', { name: 'Publish translations' }).click();
      const secondPublishConfirmation = publisher.page.getByRole('alert').filter({ hasText: /all messages in the .* text group .* not just the message open now/i });
      await expect(secondPublishConfirmation).toContainText('Current published revision: 1');
      await secondPublishConfirmation.getByRole('button', { name: 'Confirm publish' }).click();
      await expect(publisher.page.getByRole('status').filter({ hasText: 'Published as revision 2.' })).toBeVisible();
      await publisher.page.getByLabel('Rollback target revision').fill('1');
      await publisher.page.getByRole('button', { name: 'Publish rollback' }).click();
      await expect(publisher.page.getByRole('status').filter({ hasText: 'Rollback published as new revision 3' })).toBeVisible();
      await expect(publisher.page.getByText('Xin chào {{name}}', { exact: true })).toBeVisible();
      await expect.poll(() => publishedValue(publisher.page, locale, namespace, key)).toBe('Xin chào {{name}}');
    } finally {
      await translator.context.close(); await reviewer.context.close(); await publisher.context.close();
    }
  });

  test('rejects inaccessible branding, then publishes and restores bounded appearance and country profiles', async ({ browser }) => {
    const sessions = await Promise.all(Object.entries(adminFixture.stateFiles).map(async ([role, path]) => [
      role, await session(browser, path),
    ] as const));
    const byRole = Object.fromEntries(sessions) as Record<keyof typeof adminFixture.stateFiles, Awaited<ReturnType<typeof session>>>;
    const administrator = byRole.administrator;
    try {
      await expectDistinctActors(sessions.map(([, value]) => value.page));
      await openAdmin(administrator.page, '/admin/appearance');
      await administrator.page.getByLabel('Accent color').fill('#FFFFFF');
      await administrator.page.getByLabel('Text on accent').fill('#FFFFFF');
      await expect(administrator.page.getByRole('button', { name: 'Save theme draft' })).toBeDisabled();
      await expect(administrator.page.getByRole('alert').filter({ hasText: 'contrast failures' })).toBeVisible();

      await administrator.page.getByLabel('Accent color').fill('#0F766E');
      await administrator.page.getByLabel('Text on accent').fill('#FFFFFF');
      const originalName = await administrator.page.getByLabel('Display name').inputValue();
      await administrator.page.getByRole('button', { name: 'Save theme draft' }).click();
      await expect(administrator.page.getByRole('status').filter({ hasText: 'Theme draft saved.' })).toBeVisible();
      await administrator.page.getByRole('button', { name: 'Publish theme' }).click();
      await expect(administrator.page.getByRole('status').filter({ hasText: /Theme revision 1 is published/ })).toBeVisible();
      await administrator.page.getByLabel('Display name').fill(`Synthetic ${adminFixture.runId}`);
      await administrator.page.getByRole('button', { name: 'Save theme draft' }).click();
      await administrator.page.getByRole('button', { name: 'Publish theme' }).click();
      await expect(administrator.page.getByRole('status').filter({ hasText: /Theme revision 2 is published/ })).toBeVisible();
      await administrator.page.getByLabel('Rollback target version').fill('1');
      await administrator.page.getByRole('button', { name: 'Publish rollback' }).click();
      await expect(administrator.page.getByRole('status').filter({ hasText: /Rollback published as theme revision 3/ })).toBeVisible();
      await expect(administrator.page.getByLabel('Display name')).toHaveValue(originalName);

      await openAdmin(administrator.page, '/admin/country-profiles');
      await expect(administrator.page.getByRole('heading', { name: 'Country profiles' })).toBeVisible();
      await expect(administrator.page.getByText(/cannot set matching rules/i)).toBeVisible();
      await expect(administrator.page.getByRole('status').filter({ hasText: 'No published country profile exists' })).toBeVisible();
      await administrator.page.getByLabel('Default locale tag').fill('en');
      await administrator.page.getByLabel('Calendar identifier').fill('gregory');
      await administrator.page.getByLabel('IANA time zone').fill('Asia/Vientiane');
      await administrator.page.getByLabel('Phone region').fill('LA');
      const baselineTimezone = await administrator.page.getByLabel('IANA time zone').inputValue();
      const baselineLocale = await administrator.page.getByLabel('Default locale tag').inputValue();
      await administrator.page.getByRole('button', { name: 'Save profile draft' }).click();
      await administrator.page.getByRole('button', { name: 'Publish country profile' }).click();
      await expect(administrator.page.getByRole('status').filter({ hasText: /Country profile LA published at version 1/ })).toBeVisible();
      await administrator.page.getByLabel('Default locale tag').fill(adminFixture.locale);
      await administrator.page.getByLabel('Calendar identifier').fill('gregory');
      await administrator.page.getByLabel('IANA time zone').fill('Asia/Singapore');
      await administrator.page.getByLabel('Phone region').fill('LA');
      await administrator.page.getByRole('button', { name: 'Save profile draft' }).click();
      await administrator.page.getByRole('button', { name: 'Publish country profile' }).click();
      await expect(administrator.page.getByRole('status').filter({ hasText: /Country profile LA published at version 2/ })).toBeVisible();
      await administrator.page.getByLabel('Previously published version to restore').fill('1');
      await administrator.page.getByRole('button', { name: 'Restore country profile' }).click();
      await expect(administrator.page.getByRole('status').filter({ hasText: /Country profile LA restored as new version 3/ })).toBeVisible();
      await expect(administrator.page.getByLabel('IANA time zone')).toHaveValue(baselineTimezone);
      await expect(administrator.page.getByLabel('Default locale tag')).toHaveValue(baselineLocale);
      await expect(administrator.page.getByLabel('Color mode')).toHaveCount(0);
    } finally { await Promise.all(sessions.map(([, value]) => value.context.close())); }
  });
});

async function session(browser: Browser, stateFile: string): Promise<{ context: BrowserContext; page: Page }> {
  const context = await browser.newContext({ storageState: stateFile });
  const page = await context.newPage();
  await page.goto('/');
  return { context, page };
}

async function openAdmin(page: Page, path: string) {
  await page.goto(path);
  await page.getByRole('button', { name: 'Edit registry context' }).waitFor({ state: 'visible' });
  const registry = page.getByLabel('Registry ID');
  if (!(await registry.isVisible())) await page.getByRole('button', { name: 'Edit registry context' }).click();
  await registry.waitFor({ state: 'visible' });
  await registry.fill(adminFixture.registryId);
  await page.getByLabel('Organization ID').fill(adminFixture.organizationId);
  await page.getByRole('button', { name: 'Use this context' }).click();
  await expect(page.getByText(adminFixture.registryId, { exact: true })).toBeVisible();
  await expect(page.getByText(adminFixture.organizationId, { exact: true })).toBeVisible();
}

async function selectTranslation(page: Page, namespace: string, key: string, locale = adminFixture.locale) {
  await page.getByLabel('Active locale').selectOption(locale);
  await page.getByLabel('Text group', { exact: true }).fill(namespace);
  await page.getByLabel('Message ID', { exact: true }).fill(key);
  await page.getByRole('button', { name: 'Load message', exact: true }).click();
}

function translationUrl(locale: string, namespace: string, key: string) {
  return `/api/v1/config/${adminFixture.registryId}/${adminFixture.organizationId}/locales/${locale}/${namespace}/${key}`;
}

async function expectDenied(page: Page, url: string, body: object) {
  const result = await page.evaluate(async ({ url, body }) => {
    const csrf = document.cookie.split('; ').find((part) => part.startsWith('litecr_csrf='))?.slice('litecr_csrf='.length);
    if (!csrf) throw new Error('Authenticated browser state has no CSRF cookie.');
    const response = await fetch(url, {
      method: 'POST',
      credentials: 'same-origin',
      headers: { 'content-type': 'application/json', 'x-csrf-token': decodeURIComponent(csrf) },
      body: JSON.stringify(body),
    });
    const payload = await response.json().catch(() => null) as { code?: string } | null;
    return { status: response.status, code: payload?.code };
  }, { url, body });
  expect(result.status, `valid request must be denied for this role at ${url}`).toBe(403);
  expect(result.code, `denial at ${url} must come from server authorization`).toBe('ACCESS_DENIED');
}

async function expectDistinctActors(pages: Page[]) {
  const actorIds = await Promise.all(pages.map(async (page) => {
    const response = await page.evaluate(async () => {
      const result = await fetch('/api/v1/access/session', { credentials: 'same-origin', cache: 'no-store' });
      return { status: result.status, body: result.ok ? await result.json() as { actor_id?: string } : null };
    });
    expect(response.status).toBe(200);
    expect(response.body?.actor_id).toBeTruthy();
    return response.body!.actor_id!;
  }));
  expect(new Set(actorIds).size).toBe(actorIds.length);
}

async function publishedValue(page: Page, locale: string, namespace: string, key: string): Promise<string | undefined> {
  const result = await page.evaluate(async ({ locale, namespace, key, registryId, organizationId }) => {
    const response = await fetch(`/api/v1/bundles/${encodeURIComponent(registryId)}/${encodeURIComponent(organizationId)}/${encodeURIComponent(locale)}/${encodeURIComponent(namespace)}`, { cache: 'no-store' });
    if (!response.ok) throw new Error(`Published bundle check returned ${response.status}.`);
    const bundle = await response.json() as { entries?: Record<string, string> } | null;
    return bundle?.entries?.[key];
  }, { locale, namespace, key, registryId: adminFixture.registryId, organizationId: adminFixture.organizationId });
  return result;
}
