import { defineConfig } from '@playwright/test';

const baseURL = process.env.LITECR_E2E_BASE_URL ?? 'http://127.0.0.1:4173';
const storageState = process.env.LITECR_E2E_STORAGE_STATE;
const serverCommand = process.env.LITECR_E2E_APP_COMMAND;

export default defineConfig({
  testDir: '.',
  testMatch: '**/tests/e2e/**/*.spec.ts',
  fullyParallel: false,
  timeout: 60_000,
  use: { baseURL, ...(storageState ? { storageState } : {}), trace: 'retain-on-failure', screenshot: 'only-on-failure' },
  ...(serverCommand ? { webServer: { command: serverCommand, url: baseURL, reuseExistingServer: !process.env.CI } } : {}),
});
