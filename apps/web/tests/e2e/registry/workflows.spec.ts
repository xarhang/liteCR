import { expect, test, type Page } from '@playwright/test';

const registryId = process.env.LITECR_E2E_REGISTRY_ID;
const organizationId = process.env.LITECR_E2E_ORGANIZATION_ID;
const registrarStorageState = process.env.LITECR_E2E_REGISTRAR_STORAGE_STATE;
const enabled = Boolean(registryId && organizationId && process.env.LITECR_E2E_STORAGE_STATE && registrarStorageState);

test.describe('authenticated synthetic registry workflows', () => {
  test.skip(!enabled, 'Run only with an authenticated storage state and an isolated synthetic registry.');

  test('registers a synthetic source record, searches by returned ID, and opens source history', async ({ browser }) => {
    const registrarContext = await browser.newContext({ storageState: registrarStorageState! });
    const registrarPage = await registrarContext.newPage();
    let reviewerContext: Awaited<ReturnType<typeof browser.newContext>> | undefined;
    try {
    await registrarPage.goto('/');
    await setScope(registrarPage);
    await expect(registrarPage.getByRole('heading', { name: 'Find a registry record' })).toBeVisible();
    await registrarPage.getByRole('link', { name: 'Open registration form' }).click();
    await registrarPage.getByLabel('Source system ID').fill('litecr-e2e-synthetic');
    await registrarPage.getByLabel('Source record key').fill('synthetic-' + crypto.randomUUID());
    await registrarPage.getByLabel('Source version').fill('1');
    await registrarPage.getByLabel('Name as provided by source').fill('ຄຳທົດສອບ Synthetic Sample');
    await registrarPage.getByLabel('Language tag (optional)').fill('lo');
    await registrarPage.getByLabel('Script label (optional)').fill('Lao');
    await registrarPage.getByRole('button', { name: 'Register source record' }).click();
    const receipt = registrarPage.getByRole('status').filter({ hasText: 'Server confirmed:' });
    await expect(receipt).toContainText('Server confirmed: accepted');
    // The receipt also contains a source-record ID in its second code element.
    const publicId = await receipt.locator('code').first().textContent();
    expect(publicId).toBeTruthy();
    reviewerContext = await browser.newContext({ storageState: process.env.LITECR_E2E_STORAGE_STATE! });
    const reviewerPage = await reviewerContext.newPage();
    await reviewerPage.goto('/');
    await setScope(reviewerPage);
    await reviewerPage.getByLabel('Public ID or alias').fill(publicId!);
    await reviewerPage.getByRole('button', { name: 'Search records' }).click();
    await expect(reviewerPage.getByRole('link', { name: 'View source history' })).toBeVisible();
    await reviewerPage.getByRole('link', { name: 'View source history' }).click();
    await expect(reviewerPage.getByRole('heading', { name: publicId! })).toBeVisible();
    const sourceHistory = reviewerPage.getByRole('heading', { name: 'Source history' }).locator('..');
    await expect(sourceHistory.getByText('ຄຳທົດສອບ Synthetic Sample')).toBeVisible();
    } finally {
      await Promise.all([registrarContext.close(), ...(reviewerContext ? [reviewerContext.close()] : [])]);
    }
  });

  test('saves theme, contrast, density and locale through the appearance preference endpoint', async ({ page }) => {
    await page.goto('/');
    await setScope(page);
    await saveLocale(page, 'ar');
    await expect(page.locator('html')).toHaveAttribute('dir', 'rtl');
    await page.getByLabel('Theme').selectOption('dark');
    await expectSaved(page);
    await expect(page.locator('html')).toHaveAttribute('data-mode', 'dark');
    await page.getByLabel('Contrast').selectOption('high');
    await expectSaved(page);
    await expect(page.locator('html')).toHaveAttribute('data-contrast', 'high');
    await page.getByLabel('Density').selectOption('compact');
    await expectSaved(page);
    await expect(page.locator('html')).toHaveAttribute('data-density', 'compact');
    await page.setViewportSize({ width: 375, height: 812 });
    await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
    await page.setViewportSize({ width: 812, height: 375 });
    await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
    await page.getByLabel('Theme').selectOption('system');
    await expectSaved(page);
    await saveLocale(page, 'en');
    await page.getByLabel('Contrast').selectOption('standard');
    await expectSaved(page);
    await page.getByLabel('Density').selectOption('comfortable');
    await expectSaved(page);
  });
});

async function setScope(page: Page) {
  // The fixture page can render before the session and its saved registry
  // context have hydrated. Wait for the authenticated shell before deciding
  // whether the initially-open scope editor needs to be toggled.
  await expect(page.getByRole('button', { name: 'Sign out' })).toBeVisible();
  const registryField = page.getByLabel('Registry ID');
  if (!(await registryField.isVisible())) {
    await page.getByRole('button', { name: 'Edit registry context' }).click();
  }
  await registryField.fill(registryId!);
  await page.getByLabel('Organization ID').fill(organizationId!);
  await page.getByRole('button', { name: 'Use this context' }).click();
}

async function saveLocale(page: Page, locale: string) {
  const preferences = page.locator('details.workspace-preferences');
  if (!(await preferences.evaluate((element) => (element as HTMLDetailsElement).open))) {
    await preferences.locator('summary').click();
  }
  await page.getByLabel('Language preference').fill(locale);
  await page.getByRole('button', { name: 'Save language' }).click();
  await expectSaved(page);
}

async function expectSaved(page: Page) {
  await expect(page.getByRole('status').filter({ hasText: 'Preference saved.' })).toBeVisible();
}
