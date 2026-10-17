import { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { MemoryRouter } from 'react-router-dom';
import { RegisterScreen } from './RegistryScreens';
import { createUiTranslator } from '../../../../../packages/localization/src/webCatalogPreview';

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

describe('registration receipt handoff', () => {
  let root: Root | undefined;
  let container: HTMLDivElement | undefined;
  let originalClipboard: PropertyDescriptor | undefined;

  afterEach(async () => {
    if (root) await act(async () => root?.unmount());
    vi.unstubAllGlobals();
    if (originalClipboard) Object.defineProperty(navigator, 'clipboard', originalClipboard);
    else Reflect.deleteProperty(navigator, 'clipboard');
    root = undefined;
    container?.remove();
    container = undefined;
  });

  it('uses plain, factual labels for the source values being registered', async () => {
    container = document.createElement('div');
    document.body.append(container);
    root = createRoot(container);
    await act(async () => root?.render(
      <MemoryRouter><RegisterScreen scope={{ registryId: 'registry', organizationId: 'org' }} labels={createUiTranslator({})} /></MemoryRouter>,
    ));

    expect(container.textContent).toContain('liteCR keeps source values as entered and creates a provisional record linked to the source. It does not merge records.');
    expect(container.textContent).toContain('Values as provided');
    expect(container.textContent).toContain('Identifier issuer (assigning authority)');
    expect(container.textContent).not.toContain('source-linked provisional record');
  });

  it('shows the server source ID and copies that exact ID with accessible feedback', async () => {
    const sourceRecordId = 'synthetic-source-record-opaque-42';
    const writeText = vi.fn<Clipboard['writeText']>().mockResolvedValue(undefined);
    originalClipboard = Object.getOwnPropertyDescriptor(navigator, 'clipboard');
    Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText } });
    container = document.createElement('div');
    document.body.append(container);
    root = createRoot(container);
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify({ data: {
      outcome: 'accepted', source_record_id: sourceRecordId, person_public_id: 'litecr-person-17',
    } }), { status: 202 })));

    await act(async () => root?.render(
      <MemoryRouter><RegisterScreen scope={{ registryId: 'synthetic-registry', organizationId: 'synthetic-org' }} labels={createUiTranslator({})} /></MemoryRouter>,
    ));
    for (const [id, value] of [
      ['source-system-id', 'synthetic-source'],
      ['source-record-key', 'record-42'],
      ['source-name', 'Synthetic Example'],
    ]) {
      const input = container.querySelector<HTMLInputElement>('#' + id)!;
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set?.call(input, value);
      await act(async () => input.dispatchEvent(new Event('input', { bubbles: true })));
    }

    await act(async () => {
      container?.querySelector('form')?.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    const receipt = container.querySelector<HTMLElement>('[role="status"]')!;
    expect(receipt.textContent).toContain(sourceRecordId);
    const copy = Array.from(receipt.querySelectorAll('button')).find((button) => button.textContent === 'Copy source record ID')!;
    await act(async () => copy.click());
    expect(writeText).toHaveBeenCalledWith(sourceRecordId);
    expect(receipt.textContent).toContain('Source record ID copied.');

    writeText.mockRejectedValueOnce(new Error('clipboard unavailable'));
    await act(async () => copy.click());
    expect(receipt.textContent).toContain('Copy is unavailable. Select and copy the source record ID above.');
  });
});

describe('registration country profile suggestions', () => {
  let root: Root | undefined;
  let container: HTMLDivElement | undefined;

  afterEach(async () => {
    if (root) await act(async () => root?.unmount());
    vi.unstubAllGlobals();
    root = undefined;
    container?.remove();
    container = undefined;
  });

  async function render(scope = { registryId: 'registry-a', organizationId: 'org-a' }) {
    container = document.createElement('div');
    document.body.append(container);
    root = createRoot(container);
    await act(async () => root?.render(<MemoryRouter><RegisterScreen scope={scope} labels={createUiTranslator({})} locale="lo-LA" /></MemoryRouter>));
    return container;
  }

  async function enter(id: string, value: string) {
    const input = container!.querySelector<HTMLInputElement>('#' + id)!;
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set?.call(input, value);
    await act(async () => input.dispatchEvent(new Event('input', { bubbles: true })));
  }

  it('associates a non-empty invalid country code with its inline field error', async () => {
    await render();
    await enter('source-system-id', 'synthetic-source');
    await enter('source-record-key', 'record-1');
    await enter('source-name', 'Synthetic Person');
    await enter('source-registration-country', 'L1');
    const countryBeforeSubmit = container!.querySelector<HTMLInputElement>('#source-registration-country')!;
    expect(countryBeforeSubmit.value).toBe('L1');
    expect(countryBeforeSubmit.validity.patternMismatch).toBe(true);

    await act(async () => {
      container!.querySelector('form')?.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
    });

    const country = container!.querySelector<HTMLInputElement>('#source-registration-country')!;
    const error = container!.querySelector<HTMLElement>('#source-registration-country-error');
    expect(container!.textContent).toContain('Fix the following fields before continuing.');
    expect(country.getAttribute('aria-invalid'), `country=${country.outerHTML}; error=${error?.outerHTML}; summary=${container!.querySelector('.form-error-summary')?.textContent}`).toBe('true');
    expect(country.getAttribute('aria-describedby')?.split(' ')).toEqual([
      'source-registration-country-help',
      'source-registration-country-error',
    ]);
    expect(error?.textContent).toBe('Enter a two-letter country or region code, such as LA.');

    await enter('source-registration-country', 'LA');
    expect(country.hasAttribute('aria-invalid')).toBe(false);
    expect(container!.querySelector('#source-registration-country-error')).toBeNull();
  });

  it('offers published identifier types as suggestions without selecting them or changing the registration payload', async () => {
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      if (url.endsWith('/country-profiles/LA')) return new Response(JSON.stringify({ country_code: 'LA', identifier_types: ['national-id', 'passport'], version: 3 }), { status: 200 });
      if (init?.method === 'POST') return new Response(JSON.stringify({ data: { outcome: 'accepted', source_record_id: 'source-1', person_public_id: 'person-1' } }), { status: 202 });
      throw new Error('unexpected request ' + url);
    });
    vi.stubGlobal('fetch', fetchMock);
    await render();
    await enter('source-registration-country', 'LA');
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });

    const identifierType = container!.querySelector<HTMLInputElement>('#identifier-type')!;
    expect(identifierType.value).toBe('');
    expect(Array.from(container!.querySelectorAll('#registration-identifier-type-options option')).map((item) => item.getAttribute('value'))).toEqual(['national-id', 'passport']);

    await enter('source-system-id', 'source');
    await enter('source-record-key', 'record-1');
    await enter('source-name', 'Synthetic Person');
    await enter('identifier-type', 'national-id');
    await enter('identifier-value', 'synthetic-id');
    await act(async () => {
      container!.querySelector('form')?.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    const post = fetchMock.mock.calls.find(([, init]) => init?.method === 'POST');
    const payload = JSON.parse(String(post?.[1]?.body));
    expect(payload.identifiers).toEqual([{ authority: '', identifier_type: 'national-id', value: 'synthetic-id', authoritative: false }]);
    expect(payload).not.toHaveProperty('source_country');
  });

  it('ignores a late response from the previous registry scope', async () => {
    let resolveFirst!: (response: Response) => void;
    const first = new Promise<Response>((resolve) => { resolveFirst = resolve; });
    const fetchMock = vi.fn((input: RequestInfo | URL) => String(input).includes('/registry-a/') ? first : Promise.resolve(new Response(JSON.stringify({ country_code: 'LA', identifier_types: ['thai-id'] }), { status: 200 })));
    vi.stubGlobal('fetch', fetchMock);
    await render();
    await enter('source-registration-country', 'LA');
    expect(fetchMock).toHaveBeenCalledTimes(1);

    await act(async () => root?.render(<MemoryRouter><RegisterScreen scope={{ registryId: 'registry-b', organizationId: 'org-b' }} labels={createUiTranslator({})} locale="lo-LA" /></MemoryRouter>));
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });
    resolveFirst(new Response(JSON.stringify({ country_code: 'LA', identifier_types: ['stale-id'] }), { status: 200 }));
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });

    const options = Array.from(container!.querySelectorAll('#registration-identifier-type-options option')).map((item) => item.getAttribute('value'));
    expect(options).toEqual(['thai-id']);
    expect(fetchMock.mock.calls.map(([input]) => String(input))).toEqual([
      '/api/v1/config/registry-a/org-a/country-profiles/LA',
      '/api/v1/config/registry-b/org-b/country-profiles/LA',
    ]);
  });

  it('keeps manual registration available when profile loading fails', async () => {
    const fetchMock = vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => init?.method === 'POST'
      ? new Response(JSON.stringify({ data: { outcome: 'accepted', source_record_id: 'source-2', person_public_id: 'person-2' } }), { status: 202 })
      : new Response('', { status: 503 }));
    vi.stubGlobal('fetch', fetchMock);
    await render();
    await enter('source-registration-country', 'VN');
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });
    expect(container!.textContent).toContain('Country profile suggestions are unavailable. You can continue with manual entry.');

    await enter('source-system-id', 'source');
    await enter('source-record-key', 'record-2');
    await enter('source-name', 'Synthetic Person');
    await act(async () => {
      container!.querySelector('form')?.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(fetchMock.mock.calls.some(([, init]) => init?.method === 'POST')).toBe(true);
    expect(container!.textContent).toContain('person-2');
  });
});
