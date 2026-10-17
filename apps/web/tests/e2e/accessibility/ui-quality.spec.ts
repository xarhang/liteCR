import { expect, test } from '@playwright/test';

const enabled = Boolean(process.env.LITECR_E2E_STORAGE_STATE);
const registryId = process.env.LITECR_E2E_REGISTRY_ID;
const organizationId = process.env.LITECR_E2E_ORGANIZATION_ID;

test('sign-in entry has named landmarks, keyboard access and narrow-screen reflow', async ({ page }) => {
  // This check covers the public entry even when Playwright is configured with
  // an authenticated state for the integrated synthetic fixture.
  await page.context().clearCookies();
  await page.goto('/');
  await expect(page.getByRole('heading', { level: 1 })).toHaveCount(1);
  await expect(page.getByRole('main')).toHaveCount(1);
  await expect(page.locator('html')).toHaveAttribute('lang', 'en');
  await expect(page.locator('html')).toHaveAttribute('dir', 'ltr');
  const continueLink = page.getByRole('link', { name: 'Continue with organization sign-in' });
  await expect(continueLink).toBeVisible();
  await expect(continueLink).toHaveAttribute('href', '/api/v1/access/oidc/login');
  await page.keyboard.press('Tab');
  await expect(continueLink).toBeFocused();
  // A 1280 CSS-pixel desktop shown at 200%/400% zoom has an effective
  // layout viewport near 640/320 CSS pixels. Test those widths directly so
  // this remains deterministic across browsers and OS display scaling.
  for (const width of [640, 320, 375]) {
    await page.setViewportSize({ width, height: 800 });
    await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
    await expect(continueLink).toBeVisible();
    const bounds = await continueLink.boundingBox();
    expect(bounds).not.toBeNull();
    expect(bounds!.x).toBeGreaterThanOrEqual(0);
    expect(bounds!.x + bounds!.width).toBeLessThanOrEqual(width);
  }
});

test('keyboard focus indicator remains visible in forced-colors mode', async ({ page }) => {
  await page.context().clearCookies();
  await page.goto('/');
  const continueLink = page.getByRole('link', { name: 'Continue with organization sign-in' });
  let linkReachedByKeyboard = false;
  for (let tab = 0; tab < 8; tab++) {
    await page.keyboard.press('Tab');
    if (await continueLink.evaluate((node) => node === document.activeElement)) {
      linkReachedByKeyboard = true;
      break;
    }
  }
  expect(linkReachedByKeyboard, 'Tab navigation should reach the sign-in link').toBe(true);
  await expect(continueLink).toBeFocused();

  await page.emulateMedia({ forcedColors: 'active' });
  expect(await page.evaluate(() => matchMedia('(forced-colors: active)').matches)).toBe(true);
  const focus = await continueLink.evaluate((node) => {
    const style = getComputedStyle(node);
    return { style: style.outlineStyle, width: parseFloat(style.outlineWidth), color: style.outlineColor };
  });
  expect(focus.style).not.toBe('none');
  expect(focus.width).toBeGreaterThanOrEqual(2);
  expect(focus.color).not.toBe('transparent');
});

test.describe('integrated UI accessibility and script layout (read-only)', () => {
  test.skip(!enabled, 'Requires the isolated authenticated synthetic browser state.');

  test('review evidence table overflow is named and keyboard focusable', async ({ page }) => {
    await page.goto('/');
    await setSyntheticRegistryScope(page);
    await page.goto('/review');
    const firstCase = page.locator('.case-row').first();
    await expect(firstCase).toBeVisible();
    await firstCase.click();
    const scroller = page.locator('.evidence-table-wrap');
    await expect(scroller).toBeVisible();
    await expect(scroller).toHaveAttribute('role', 'region');
    await expect(scroller).toHaveAttribute('tabindex', '0');
    await expect(scroller).toHaveAttribute('aria-label', /Field-level evidence/);
    let reachedByKeyboard = false;
    for (let tab = 0; tab < 60; tab++) {
      await page.keyboard.press('Tab');
      if (await scroller.evaluate((element) => element === document.activeElement)) {
        reachedByKeyboard = true;
        break;
      }
    }
    expect(reachedByKeyboard, 'keyboard Tab navigation should reach the evidence region').toBe(true);
    await expect(scroller).toBeFocused();
    const outline = await scroller.evaluate((element) => getComputedStyle(element).outlineStyle);
    expect(outline).not.toBe('none');
  });

  test('keyboard entry, multilingual samples, reflow and browser accessibility modes', async ({ page }) => {
    await page.goto('/');
    await expect(page.getByRole('link', { name: 'Open registry search' })).toBeVisible();
    await expect(page.getByRole('button', { name: 'Sign out' })).toBeVisible();

    await page.keyboard.press('Tab');
    const skipLink = page.getByRole('link', { name: 'Skip to main content' });
    await expect(skipLink).toBeFocused();
    const focusStyle = await skipLink.evaluate((node) => {
      const style = getComputedStyle(node);
      return { outlineStyle: style.outlineStyle, outlineWidth: style.outlineWidth };
    });
    expect(focusStyle.outlineStyle).not.toBe('none');
    expect(parseFloat(focusStyle.outlineWidth)).toBeGreaterThanOrEqual(2);

    await page.getByRole('link', { name: 'Appearance', exact: true }).click();
    await expect(page.getByRole('heading', { name: 'Appearance' })).toBeVisible();
    await expect(page.locator('#main-content')).toBeFocused();
    for (const [tag, text] of [
      ['lo', 'ສະບາຍດີ'], ['en', 'English'], ['th', 'ภาษาไทย'], ['vi', 'Tiếng Việt'],
      ['hi-IN', 'हिन्दी'], ['ar', 'مثال اصطناعي'],
    ]) {
      await expect(page.locator(`p[lang="${tag}"]`)).toContainText(text);
    }
    await expect(page.locator('p[lang="ar"][dir="rtl"] bdi[dir="ltr"]')).toContainText('00000000-0000-4000-8000-000000000042');

    for (const width of [320, 375, 640, 768, 1280]) {
      await page.setViewportSize({ width, height: 800 });
      await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
    }
    await page.setViewportSize({ width: 812, height: 375 });
    await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);

    await page.emulateMedia({ reducedMotion: 'reduce' });
    const reducedMotion = await page.evaluate(() => ({
      matches: matchMedia('(prefers-reduced-motion: reduce)').matches,
      duration: getComputedStyle(document.querySelector('.button')!).animationDuration,
    }));
    expect(reducedMotion.matches).toBe(true);
    expect(reducedMotion.duration.split(',').every((value) => parseFloat(value) <= 0.001)).toBe(true);

    await page.emulateMedia({ forcedColors: 'active' });
    const forcedColorsApplied = await page.evaluate(() => matchMedia('(forced-colors: active)').matches);
    expect(forcedColorsApplied).toBe(true);
  });

  test('keyboard focus indicator contrasts with the dark navigation surface', async ({ page }) => {
    await page.goto('/');
    const currentLink = page.locator('.side-nav a[aria-current="page"]');
    await expect(currentLink).toBeVisible();
    let reachedByKeyboard = false;
    for (let tab = 0; tab < 12; tab++) {
      await page.keyboard.press('Tab');
      if (await currentLink.evaluate((node) => node === document.activeElement)) {
        reachedByKeyboard = true;
        break;
      }
    }
    expect(reachedByKeyboard, 'Tab navigation should reach the current side navigation link').toBe(true);

    const contrast = await currentLink.evaluate((node) => {
      const toRgb = (value: string) => {
        const match = value.match(/^rgba?\((\d+),\s*(\d+),\s*(\d+)/);
        if (!match) throw new Error(`Unexpected computed color: ${value}`);
        return [Number(match[1]), Number(match[2]), Number(match[3])];
      };
      const luminance = (color: number[]) => color.map((channel) => {
        const part = channel / 255;
        return part <= 0.04045 ? part / 12.92 : ((part + 0.055) / 1.055) ** 2.4;
      }).reduce((sum, channel, index) => sum + channel * [0.2126, 0.7152, 0.0722][index], 0);
      const focus = getComputedStyle(node).outlineColor;
      const surface = getComputedStyle(node.closest('.side-nav')!).backgroundColor;
      const a = luminance(toRgb(focus));
      const b = luminance(toRgb(surface));
      return { focus, surface, ratio: (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05) };
    });
    expect(contrast.ratio, `${contrast.focus} against ${contrast.surface}`).toBeGreaterThanOrEqual(3);
  });

  test('semantic text and action token pairs meet AA contrast in all shipped modes', async ({ page }) => {
    await page.goto('/');
    await expect(page.getByRole('link', { name: 'Open registry search' })).toBeVisible();
    const pairs = await page.evaluate(() => {
      const root = document.documentElement;
      const hex = (value: string) => {
        let normalized = value.trim();
        if (/^#[0-9a-f]{3}$/i.test(normalized)) normalized = '#' + [...normalized.slice(1)].map((digit) => digit + digit).join('');
        const match = normalized.match(/^#([0-9a-f]{6})$/i);
        if (!match) throw new Error(`Unexpected token color format: ${value}`);
        const raw = match[1];
        return [0, 2, 4].map((offset) => parseInt(raw.slice(offset, offset + 2), 16) / 255);
      };
      const luminance = (color: string) => hex(color).map((part) => part <= 0.04045 ? part / 12.92 : ((part + 0.055) / 1.055) ** 2.4)
        .reduce((sum, channel, index) => sum + channel * [0.2126, 0.7152, 0.0722][index], 0);
      const ratio = (a: string, b: string) => {
        const l1 = luminance(a); const l2 = luminance(b);
        return (Math.max(l1, l2) + 0.05) / (Math.min(l1, l2) + 0.05);
      };
      const checks: Array<{ mode: string; pair: string; ratio: number }> = [];
      for (const mode of ['light', 'dark']) for (const contrast of ['standard', 'high']) {
        root.dataset.mode = mode;
        root.dataset.contrast = contrast;
        const style = getComputedStyle(root);
        const surface = style.getPropertyValue('--color-surface').trim();
        const canvas = style.getPropertyValue('--color-canvas').trim();
        const text = style.getPropertyValue('--color-text').trim();
        const muted = style.getPropertyValue('--color-text-muted').trim();
        const action = style.getPropertyValue('--color-action').trim();
        const onAction = style.getPropertyValue('--color-on-action').trim();
        const border = style.getPropertyValue('--color-border').trim();
        const focus = style.getPropertyValue('--color-focus').trim();
        const danger = style.getPropertyValue('--color-danger').trim();
        const success = style.getPropertyValue('--color-success').trim();
        const warning = style.getPropertyValue('--color-warning').trim();
        checks.push(
          { mode: `${mode}/${contrast}`, pair: 'text/surface', ratio: ratio(text, surface) },
          { mode: `${mode}/${contrast}`, pair: 'muted/surface', ratio: ratio(muted, surface) },
          { mode: `${mode}/${contrast}`, pair: 'action/canvas', ratio: ratio(action, canvas) },
          { mode: `${mode}/${contrast}`, pair: 'on-action/action', ratio: ratio(onAction, action) },
          { mode: `${mode}/${contrast}`, pair: 'danger/surface', ratio: ratio(danger, surface) },
          { mode: `${mode}/${contrast}`, pair: 'success/surface', ratio: ratio(success, surface) },
          { mode: `${mode}/${contrast}`, pair: 'warning/surface', ratio: ratio(warning, surface) },
          { mode: `${mode}/${contrast}`, pair: 'border/surface', ratio: ratio(border, surface) },
          { mode: `${mode}/${contrast}`, pair: 'focus/surface', ratio: ratio(focus, surface) },
        );
      }
      return checks;
    });
    for (const pair of pairs) {
      const minimum = pair.pair === 'border/surface' || pair.pair === 'focus/surface' ? 3 : 4.5;
      expect(pair.ratio, `${pair.mode} ${pair.pair}`).toBeGreaterThanOrEqual(minimum);
    }
  });

  test('combined workflow routes reflow and published locale catalogs are inspected read-only', async ({ page }) => {
    test.skip(!registryId || !organizationId, 'Requires the isolated synthetic registry scope.');
    await page.goto('/');
    await expect(page.getByRole('link', { name: 'Open registry search' })).toBeVisible();
    await page.evaluate(({ registry, organization }) => {
      localStorage.setItem('litecr.registry-context.v1', JSON.stringify({ registryId: registry, organizationId: organization }));
    }, { registry: registryId!, organization: organizationId! });
    await page.reload();
    await expect(page.getByText(registryId!, { exact: true }).first()).toBeVisible();

    for (const path of ['/', '/register', '/persons/synthetic-only', '/review', '/appearance', '/admin/translations', '/admin/appearance', '/admin/country-profiles']) {
      await page.goto(path);
      await expect(page.locator('#main-content h1'), `route ${path} should render one main heading`).toHaveCount(1);
      for (const width of [320, 375, 768, 1280]) {
        await page.setViewportSize({ width, height: 900 });
        await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
      }
    }

    const catalogState = await page.evaluate(async ({ registry, organization }) => {
      const results = [];
      for (const locale of ['en', 'lo', 'th', 'vi', 'hi-IN', 'ar']) {
        const path = `/api/v1/bundles/${encodeURIComponent(registry)}/${encodeURIComponent(organization)}/${locale}/ui`;
        const response = await fetch(path, { credentials: 'same-origin', cache: 'no-store' });
        const body = await response.json();
        const bundle = body && typeof body === 'object' && 'data' in body ? body.data : body;
        results.push({ locale, status: response.status, published: Boolean(bundle), revision: bundle?.revision ?? null, keys: Object.keys(bundle?.entries ?? {}).length });
      }
      return results;
    }, { registry: registryId!, organization: organizationId! });
    console.log('Synthetic UI bundle metadata only:', JSON.stringify(catalogState));
    expect(catalogState.every((item) => item.status === 200)).toBe(true);
  });
});

async function setSyntheticRegistryScope(page: import('@playwright/test').Page) {
  await page.evaluate(({ registry, organization }) => {
    if (!registry || !organization) throw new Error('Synthetic registry scope is missing.');
    localStorage.setItem('litecr.registry-context.v1', JSON.stringify({ registryId: registry, organizationId: organization }));
  }, { registry: registryId, organization: organizationId });
}
