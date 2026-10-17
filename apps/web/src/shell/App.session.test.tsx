import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { MemoryRouter } from 'react-router-dom';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { App } from './App';

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

let root: Root | undefined;
let container: HTMLDivElement | undefined;

afterEach(async () => {
  if (root) await act(async () => root?.unmount());
  root = undefined;
  container?.remove();
  container = undefined;
  vi.unstubAllGlobals();
  localStorage.clear();
});

function stubMatchMedia() {
  Object.defineProperty(window, 'matchMedia', {
    configurable: true,
    value: () => ({ matches: false, addEventListener: vi.fn(), removeEventListener: vi.fn() }),
  });
}

describe('App session check recovery', () => {
  it('lets the user retry a failed session check and enters the workspace after confirmation', async () => {
    stubMatchMedia();
    let sessionChecks = 0;
    vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL) => {
      const path = new URL(String(input), 'http://localhost').pathname;
      if (path.endsWith('/access/session')) {
        sessionChecks += 1;
        if (sessionChecks === 1) {
          return new Response(JSON.stringify({ error: { code: 'TEMPORARY', message: 'private service detail' } }), { status: 503 });
        }
        return new Response(JSON.stringify({ actor_kind: 'human', actor_id: 'synthetic-operator', request_id: 'synthetic-request' }), { status: 200 });
      }
      return new Response('null', { status: 200 });
    }));

    container = document.createElement('div');
    document.body.append(container);
    root = createRoot(container);
    await act(async () => {
      root?.render(createElement(MemoryRouter, null, createElement(App)));
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(container.textContent).toContain('Sign-in status could not be checked. Try again.');
    expect(container.textContent).not.toContain('private service detail');
    const retry = container.querySelector<HTMLButtonElement>('button')!;
    expect(retry.textContent).toBe('Check again');

    await act(async () => {
      retry.click();
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(sessionChecks).toBe(2);
    expect(container.querySelector('.auth-card')).toBeNull();
    expect(container.querySelector('.session-status')?.textContent).toContain('Signed in');
    expect(container.querySelector('#main-content')).toBe(document.activeElement);
  });

  it('moves focus to the sign-in action after retry confirms no session', async () => {
    stubMatchMedia();
    let sessionChecks = 0;
    vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL) => {
      const path = new URL(String(input), 'http://localhost').pathname;
      if (path.endsWith('/access/session')) {
        sessionChecks += 1;
        return sessionChecks === 1
          ? new Response(JSON.stringify({ error: { code: 'TEMPORARY', message: 'unavailable' } }), { status: 503 })
          : new Response(null, { status: 401 });
      }
      return new Response('null', { status: 200 });
    }));

    container = document.createElement('div');
    document.body.append(container);
    root = createRoot(container);
    await act(async () => {
      root?.render(createElement(MemoryRouter, null, createElement(App)));
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    const retry = container.querySelector<HTMLButtonElement>('button')!;
    await act(async () => {
      retry.click();
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(container.querySelector<HTMLAnchorElement>('a[href="/api/v1/access/oidc/login"]')).toBe(document.activeElement);
    expect(container.querySelector('[role="alert"]')).toBeNull();
  });
});
