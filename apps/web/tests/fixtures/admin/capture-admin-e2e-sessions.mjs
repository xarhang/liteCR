import { readFile } from 'node:fs/promises';
import path from 'node:path';
import { chromium } from '@playwright/test';

const baseURL = process.env.LITECR_E2E_BASE_URL ?? 'http://127.0.0.1:4173';
const fixtureDirectory = process.env.LITECR_E2E_ADMIN_FIXTURE_DIR;
if (!fixtureDirectory || !fixtureDirectory.startsWith('\\\\wsl.localhost\\Ubuntu\\home\\')) {
  throw new Error('Use the per-run private WSL-home admin-e2e directory for browser sessions.');
}

const credentials = JSON.parse(await readFile(path.join(fixtureDirectory, 'oidc-users.json'), 'utf8'));
const browser = await chromium.launch({ headless: true });
const actors = [];
function safeLocation(value) {
  try {
    const url = new URL(value);
    return `${url.origin}${url.pathname}`;
  } catch {
    return 'invalid-url';
  }
}
try {
  for (const role of ['translator', 'reviewer', 'publisher', 'administrator', 'registrar']) {
    const context = await browser.newContext();
    const page = await context.newPage();
    const navigationEvents = [];
    const recordNavigationEvent = (event) => {
      if (navigationEvents.length < 30) navigationEvents.push(event);
    };
    page.on('framenavigated', (frame) => {
      if (frame === page.mainFrame()) recordNavigationEvent(`main-frame:${safeLocation(frame.url())}`);
    });
    page.on('requestfailed', (request) => {
      const failure = request.failure()?.errorText ?? '';
      const safeFailure = /^(?:net::)?ERR_[A-Z0-9_]{1,64}$/.test(failure) ? failure : 'unknown';
      recordNavigationEvent(`request-failed:${safeLocation(request.url())}:${safeFailure}`);
    });
    page.on('response', (response) => {
      const url = new URL(response.url());
      if (url.origin === new URL(baseURL).origin && url.pathname === '/api/v1/access/oidc/login') {
        let redirect = 'none';
        try {
          const location = response.headers().location;
          if (location) redirect = safeLocation(new URL(location, response.url()).href);
        } catch { redirect = 'invalid-location'; }
        recordNavigationEvent(`oidc-login-response:${response.status()}:${redirect}`);
      }
      if (url.origin === 'http://127.0.0.1:18081') {
        recordNavigationEvent(`idp-response:${safeLocation(response.url())}:${response.status()}`);
      }
    });
    page.on('pageerror', (error) => recordNavigationEvent(`page-error:${error.name || 'Error'}`));
    page.on('console', (message) => {
      if (message.type() === 'error') recordNavigationEvent('console-error');
    });
    await page.goto(baseURL);
    const loginResponsePromise = page.waitForResponse((response) => {
      try { return new URL(response.url()).pathname === '/api/v1/access/oidc/login'; }
      catch { return false; }
    }, { timeout: 15_000 });
    await page.locator('a[href="/api/v1/access/oidc/login"]').click();
    let loginResponse;
    try {
      loginResponse = await loginResponsePromise;
    } catch {
      const currentUrl = new URL(page.url());
      const navigationSummary = navigationEvents.length ? navigationEvents.join('|') : 'none';
      throw new Error(`OIDC login endpoint response timed out; current_url=${currentUrl.origin}${currentUrl.pathname} navigation=${navigationSummary}.`);
    }
    const locationHeader = loginResponse.headers().location;
    let safeRedirect = 'none';
    if (locationHeader) {
      try {
        const redirect = new URL(locationHeader, loginResponse.url());
        safeRedirect = `${redirect.origin}${redirect.pathname}`;
      } catch { safeRedirect = 'invalid-location'; }
    }
    let apiErrorCode = 'none';
    if (loginResponse.status() >= 400) {
      try {
        const payload = await loginResponse.json();
        if (typeof payload?.code === 'string' && /^[A-Z0-9_]{1,64}$/.test(payload.code)) {
          apiErrorCode = payload.code;
        }
      } catch { apiErrorCode = 'unreadable-error-response'; }
    }
    const expectedAuthUrl = 'http://127.0.0.1:18081/realms/litecr/protocol/openid-connect/auth';
    if (loginResponse.status() < 300 || loginResponse.status() >= 400 || safeRedirect !== expectedAuthUrl) {
      throw new Error(`OIDC login response status=${loginResponse.status()} redirect=${safeRedirect} api_error_code=${apiErrorCode}.`);
    }
    try {
      await page.waitForURL((url) => `${url.origin}${url.pathname}` === expectedAuthUrl, { timeout: 15_000 });
    } catch {
      const currentUrl = new URL(page.url());
      const navigationSummary = navigationEvents.length ? navigationEvents.join('|') : 'none';
      throw new Error(`OIDC redirect navigation timed out; login_status=${loginResponse.status()} redirect=${safeRedirect} current_url=${currentUrl.origin}${currentUrl.pathname} navigation=${navigationSummary}.`);
    }
    await page.locator('#username').fill(credentials[role].username);
    await page.locator('#password').fill(credentials[role].password);
    await page.locator('#kc-login').click();
    await page.waitForURL((url) => url.origin === new URL(baseURL).origin);
    const actor = await page.evaluate(async () => {
      const response = await fetch('/api/v1/access/session', { credentials: 'same-origin', cache: 'no-store' });
      return { status: response.status, body: response.ok ? await response.json() : null };
    });
    if (actor.status !== 200 || !actor.body?.actor_id || actor.body.actor_id !== credentials[role].subject) {
      throw new Error(`OIDC subject verification failed for the ${role} test account.`);
    }
    actors.push(actor.body.actor_id);
    await context.storageState({ path: path.join(fixtureDirectory, `${role}.json`) });
    await context.close();
    process.stdout.write(`captured ${role} session\n`);
  }
  if (new Set(actors).size !== 5) throw new Error('The five OIDC sessions must resolve to distinct actors.');
} finally {
  await browser.close();
}
