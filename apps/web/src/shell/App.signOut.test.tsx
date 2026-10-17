import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { MemoryRouter } from 'react-router-dom';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { App } from './App';

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

const activeSession = { actor_kind: 'human', actor_id: 'synthetic-operator', request_id: 'synthetic-request' };
let root: Root | undefined;
let container: HTMLDivElement | undefined;

async function renderApp() {
  Object.defineProperty(window, 'matchMedia', {
    configurable: true,
    value: () => ({ matches: false, addEventListener: vi.fn(), removeEventListener: vi.fn() }),
  });
  container = document.createElement('div');
  document.body.append(container);
  root = createRoot(container);
  await act(async () => {
    root?.render(createElement(MemoryRouter, null, createElement(App)));
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
  return container;
}

afterEach(async () => {
  if (root) await act(async () => root?.unmount());
  root = undefined;
  container?.remove();
  container = undefined;
  vi.unstubAllGlobals();
  localStorage.clear();
});

describe('App sign-out confirmation', () => {
  it('keeps the active workspace after an ambiguous failure and lets the user retry', async () => {
    let rejectFirstLogout!: (error: Error) => void;
    let logoutCount = 0;
    const firstLogout = new Promise<Response>((_resolve, reject) => { rejectFirstLogout = reject; });
    vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const path = String(input);
      if (path.endsWith('/access/session')) return new Response(JSON.stringify(activeSession), { status: 200 });
      if (path.endsWith('/access/logout') && init?.method === 'POST') {
        logoutCount += 1;
        return logoutCount === 1 ? firstLogout : new Response(null, { status: 204 });
      }
      return new Response('null', { status: 200 });
    }));

    const app = await renderApp();
    const signOut = app.querySelector<HTMLButtonElement>('button.button-subtle:last-child')!;
    await act(async () => signOut.dispatchEvent(new MouseEvent('click', { bubbles: true })));

    expect(signOut.disabled).toBe(true);
    expect(app.querySelector('[role="status"]')?.textContent).toContain('Signing out…');

    await act(async () => {
      rejectFirstLogout(new Error('connection closed before logout response'));
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(app.querySelector('.session-status')?.textContent).toContain('Signed in');
    expect(app.querySelector('[role="alert"]')?.textContent).toBe(
      'Sign-out could not be confirmed. You may still be signed in. Try again.',
    );
    expect(app.querySelector<HTMLButtonElement>('button.button-subtle:last-child')?.textContent).toBe('Retry sign out');
    expect(app.textContent).not.toContain('Sign in to continue');

    await act(async () => {
      app.querySelector<HTMLButtonElement>('button.button-subtle:last-child')?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(app.textContent).toContain('Sign in to continue');
    expect(app.querySelector('[role="alert"]')).toBeNull();
  });

  it('keeps the workspace and explains that sign-out is unconfirmed if session recheck also fails', async () => {
    let sessionCount = 0;
    vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const path = String(input);
      if (path.endsWith('/access/session')) {
        sessionCount += 1;
        if (sessionCount === 1) return new Response(JSON.stringify(activeSession), { status: 200 });
        throw new Error('session endpoint unavailable');
      }
      if (path.endsWith('/access/logout') && init?.method === 'POST') {
        return new Response(JSON.stringify({ error: { code: 'TEMPORARY', message: 'private server detail' } }), { status: 503 });
      }
      return new Response('null', { status: 200 });
    }));

    const app = await renderApp();
    await act(async () => {
      app.querySelector<HTMLButtonElement>('button.button-subtle:last-child')?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(app.querySelector('.session-status')?.textContent).toContain('Signed in');
    expect(app.querySelector('[role="alert"]')?.textContent).toBe(
      'Sign-out could not be confirmed. You may still be signed in. Try again.',
    );
    expect(app.textContent).not.toContain('Sign in to continue');
    expect(app.textContent).not.toContain('private server detail');
  });
});
