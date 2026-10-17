import { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { MemoryRouter } from 'react-router-dom';
import { SearchScreen } from './RegistryScreens';
import { createUiTranslator } from '../../../../../packages/localization/src/webCatalogPreview';

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

describe('search progress feedback', () => {
  let root: Root | undefined;
  let container: HTMLDivElement | undefined;

  afterEach(async () => {
    if (root) await act(async () => root?.unmount());
    vi.unstubAllGlobals();
    root = undefined;
    container?.remove();
    container = undefined;
  });

  it('announces a pending search and clears busy state when results arrive', async () => {
    let resolveSearch!: (response: Response) => void;
    const fetchMock = vi.fn(() => new Promise<Response>((resolve) => { resolveSearch = resolve; }));
    vi.stubGlobal('fetch', fetchMock);
    container = document.createElement('div');
    document.body.append(container);
    root = createRoot(container);

    await act(async () => root?.render(
      <MemoryRouter>
        <SearchScreen scope={{ registryId: 'synthetic-registry', organizationId: 'synthetic-org' }} labels={createUiTranslator({})} />
      </MemoryRouter>,
    ));

    const form = container.querySelector('form')!;
    const input = container.querySelector<HTMLInputElement>('#search-id')!;
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set?.call(input, 'synthetic-public-id');
    await act(async () => {
      input.dispatchEvent(new Event('input', { bubbles: true }));
      form.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
      await Promise.resolve();
    });

    expect(form.getAttribute('aria-busy')).toBe('true');
    expect(input.disabled).toBe(true);
    const status = container.querySelector<HTMLElement>('[role="status"]');
    expect(status?.getAttribute('aria-live')).toBe('polite');
    expect(status?.textContent).toBe('Searching…');

    await act(async () => {
      resolveSearch(new Response(JSON.stringify({ data: [] }), { status: 200 }));
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(form.hasAttribute('aria-busy')).toBe(false);
    expect(input.disabled).toBe(false);
    expect(status?.textContent).toBe('');
    expect(container.textContent).toContain('No public ID or alias matched in this registry and organization. This search does not check demographic details.');

    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set?.call(input, 'different-public-id');
    await act(async () => input.dispatchEvent(new Event('input', { bubbles: true })));
    expect(input.value).toBe('different-public-id');
    expect(container.textContent).not.toContain('No public ID or alias matched in this registry and organization. This search does not check demographic details.');
  });
});
