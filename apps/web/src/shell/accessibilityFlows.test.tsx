import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { Link, MemoryRouter, Route, Routes } from 'react-router-dom';
import { RegisterScreen } from '../features/registry/RegistryScreens';
import { createUiTranslator } from '../../../../packages/localization/src/webCatalogPreview';
import laoCatalog from '../../../../packages/localization/catalogs/lo.json';
import { App } from './App';
import { RouteFocusManager } from './RouteFocusManager';

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

let root: Root | undefined;
let container: HTMLDivElement | undefined;

afterEach(async () => {
  if (root) await act(async () => root?.unmount());
  root = undefined;
  container?.remove();
  container = undefined;
  vi.unstubAllGlobals();
});

describe('accessible keyboard flows', () => {
  it('announces the registry scope disclosure state and returns focus after closing it', async () => {
    localStorage.setItem('litecr.registry-context.v1', JSON.stringify({ registryId: 'registry-a', organizationId: 'org-a' }));
    Object.defineProperty(window, 'matchMedia', { configurable: true, value: () => ({ matches: false, addEventListener: vi.fn(), removeEventListener: vi.fn() }) });
    vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL) => {
      const response = String(input).endsWith('/access/session')
        ? { actor_kind: 'human', actor_id: 'reviewer-1', request_id: 'request-1' }
        : null;
      return new Response(JSON.stringify(response), { status: 200 });
    }));
    container = document.createElement('div'); document.body.append(container); root = createRoot(container);
    await act(async () => {
      root?.render(createElement(MemoryRouter, null, createElement(App)));
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    const toggle = container.querySelector<HTMLButtonElement>('[aria-controls="registry-scope-form"]')!;
    const form = container.querySelector<HTMLFormElement>('#registry-scope-form')!;
    expect(toggle.getAttribute('aria-expanded')).toBe('false');
    expect(form.hidden).toBe(true);

    toggle.focus();
    await act(async () => toggle.dispatchEvent(new MouseEvent('click', { bubbles: true })));
    expect(toggle.getAttribute('aria-expanded')).toBe('true');
    expect(form.hidden).toBe(false);

    const close = form.querySelector<HTMLButtonElement>('button[type="button"]')!;
    await act(async () => close.dispatchEvent(new MouseEvent('click', { bubbles: true })));
    expect(toggle.getAttribute('aria-expanded')).toBe('false');
    expect(form.hidden).toBe(true);
    expect(document.activeElement).toBe(toggle);
  });

  it('preserves initial keyboard entry and focuses the main landmark after route changes', async () => {
    container = document.createElement('div'); document.body.append(container); root = createRoot(container);
    await act(async () => {
      root?.render(createElement(MemoryRouter, { initialEntries: ['/'] },
        createElement(RouteFocusManager, { ready: false }),
        createElement('a', { className: 'skip-link', href: '#main-content' }, 'Skip to main content'),
        createElement(Link, { to: '/next' }, 'Next page'),
        createElement('main', { id: 'main-content', tabIndex: -1 },
          createElement(Routes, null,
            createElement(Route, { path: '/', element: createElement('h1', null, 'First page') }),
            createElement(Route, { path: '/next', element: createElement('h1', null, 'Second page') })))));
    });
    const main = container.querySelector('main');
    expect(document.activeElement).not.toBe(main);

    await act(async () => {
      root?.render(createElement(MemoryRouter, { initialEntries: ['/'] },
        createElement(RouteFocusManager, { ready: true }),
        createElement('a', { className: 'skip-link', href: '#main-content' }, 'Skip to main content'),
        createElement(Link, { to: '/next' }, 'Next page'),
        createElement('main', { id: 'main-content', tabIndex: -1 },
          createElement(Routes, null,
            createElement(Route, { path: '/', element: createElement('h1', null, 'First page') }),
            createElement(Route, { path: '/next', element: createElement('h1', null, 'Second page') })))));
    });
    expect(document.activeElement).not.toBe(main);

    await act(async () => {
      container?.querySelector('a[href="/next"]')?.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true, button: 0 }));
    });
    expect(container.textContent).toContain('Second page');
    expect(document.activeElement).toBe(container.querySelector('main'));
  });

  it('focuses a linked registration error summary and keeps the inline field error connected', async () => {
    container = document.createElement('div'); document.body.append(container); root = createRoot(container);
    await act(async () => root?.render(createElement(MemoryRouter, null,
      createElement(RegisterScreen, {
        scope: { registryId: 'registry', organizationId: 'organization' },
        labels: createUiTranslator(laoCatalog),
      }))));

    const form = container.querySelector('form')!;
    await act(async () => {
      form.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
    });

    const summary = container.querySelector<HTMLElement>('.form-error-summary');
    const requiredInput = container.querySelector<HTMLInputElement>('#source-system-id')!;
    expect(summary?.getAttribute('role')).toBe('alert');
    expect(summary?.getAttribute('tabindex')).toBe('-1');
    expect(summary?.querySelector('h2')?.textContent).toBe(laoCatalog['registry.register.validation.summary']);
    expect(summary?.querySelector('a[href="#source-system-id"]')).not.toBeNull();
    expect(document.activeElement).toBe(summary);
    expect(requiredInput.getAttribute('aria-invalid')).toBe('true');
    expect(requiredInput.getAttribute('aria-describedby')).toBe('source-system-id-error');
    expect(container.querySelector('#source-system-id-error')?.textContent).toBe(laoCatalog['registry.register.validation.required']);
    expect(summary?.textContent).not.toContain(requiredInput.validationMessage);

    requiredInput.focus();
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set?.call(requiredInput, 'system-1');
      requiredInput.dispatchEvent(new Event('input', { bubbles: true }));
    });
    expect(container.querySelector('.form-error-summary')).toBe(summary);
    expect(document.activeElement).toBe(requiredInput);
    expect(summary?.querySelector('a[href="#source-system-id"]')).toBeNull();
    expect(summary?.querySelector('a[href="#source-record-key"]')).not.toBeNull();
    expect(requiredInput.hasAttribute('aria-invalid')).toBe(false);
  });

  it('uses localized guidance for a non-required control with invalid input', async () => {
    container = document.createElement('div'); document.body.append(container); root = createRoot(container);
    await act(async () => root?.render(createElement(MemoryRouter, null,
      createElement(RegisterScreen, {
        scope: { registryId: 'registry', organizationId: 'organization' },
        labels: createUiTranslator(laoCatalog),
      }))));

    const sourceVersion = container.querySelector<HTMLInputElement>('#source-version')!;
    sourceVersion.setCustomValidity('browser-only validation text');
    await act(async () => {
      container?.querySelector('form')?.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
    });

    expect(container.querySelector('#source-version-error')?.textContent).toBe(laoCatalog['registry.register.validation.invalid']);
    expect(container.querySelector('.form-error-summary')?.textContent).not.toContain('browser-only validation text');
  });
});
