import { act, createElement, type ReactNode } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { MemoryRouter, Route, Routes } from 'react-router-dom';
import { CountryProfileAdmin } from '../country_profiles/CountryProfileAdmin';
import { TranslationManager } from './TranslationManager';
import { PersonDetailScreen, RegisterScreen, SearchScreen } from '../registry/RegistryScreens';
import { ReviewWorkspace } from '../review/ReviewWorkspace';
import { AppearanceAdmin } from '../appearance/AppearanceAdmin';
import { App } from '../../shell/App';
import { loadBuiltInDraftCatalog } from './builtinDraftCatalogs';
import { createUiTranslator, englishUiMessages, UI_NAMESPACE } from '../../../../../packages/localization/src/webCatalogPreview';

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

let root: Root | undefined;
let container: HTMLDivElement | undefined;

afterEach(async () => {
  if (root) await act(async () => root?.unmount());
  vi.unstubAllGlobals();
  root = undefined;
  container?.remove();
  container = undefined;
});

describe('published UI namespace integration', () => {
  it('announces an empty country-profile filter result to assistive technology', async () => {
    vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL) => {
      const path = new URL(String(input), 'http://localhost').pathname;
      if (path.endsWith('/country-profiles')) return new Response('[]', { status: 200 });
      return new Response('null', { status: 200 });
    }));
    const labels = createUiTranslator({});
    container = document.createElement('div'); document.body.append(container); root = createRoot(container);
    await act(async () => { root?.render(createElement(CountryProfileAdmin, { registryId: 'r', organizationId: 'o', labels })); await new Promise((resolve) => setTimeout(resolve, 0)); });

    const filter = [...container.querySelectorAll<HTMLInputElement>('input')]
      .find((input) => input.getAttribute('aria-describedby') === null && input.parentElement?.textContent?.includes('Filter regions'));
    expect(filter).toBeDefined();
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set?.call(filter, 'no-region-can-match-this');
      filter?.dispatchEvent(new Event('input', { bubbles: true }));
    });

    const emptyResult = container.querySelector('[role="status"]');
    expect(emptyResult?.textContent).toBe('No matching regions.');
    expect(emptyResult?.closest('[role="list"]')).toBeNull();
  });

  it('associates an invalid country code message with its field', async () => {
    vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL) => {
      const path = new URL(String(input), 'http://localhost').pathname;
      if (path.endsWith('/country-profiles')) return new Response('[]', { status: 200 });
      return new Response('null', { status: 200 });
    }));
    const labels = createUiTranslator({});
    container = document.createElement('div'); document.body.append(container); root = createRoot(container);
    await act(async () => { root?.render(createElement(CountryProfileAdmin, { registryId: 'r', organizationId: 'o', labels })); await new Promise((resolve) => setTimeout(resolve, 0)); });
    expect(container.textContent).toContain('Set the default address fields, identifier labels, locale, calendar and time zone for this country profile.');
    expect(container.textContent).not.toContain('Standard CLDR regions');

    const countryCode = container.querySelectorAll<HTMLInputElement>('input')[0];
    expect(countryCode).not.toBeNull();
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set?.call(countryCode, 'I');
      countryCode?.dispatchEvent(new Event('input', { bubbles: true }));
      countryCode?.dispatchEvent(new FocusEvent('focusout', { bubbles: true }));
    });

    expect(countryCode?.getAttribute('aria-invalid')).toBe('true');
    const describedBy = countryCode?.getAttribute('aria-describedby')?.split(/\s+/) ?? [];
    expect(describedBy).toContain('country-code-help');
    expect(describedBy).toContain('country-code-error');
    expect(container.querySelector('#country-code-error')?.textContent).toContain('two-letter uppercase country code');

    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set?.call(countryCode, 'IN');
      countryCode?.dispatchEvent(new Event('input', { bubbles: true }));
    });
    expect(countryCode?.hasAttribute('aria-invalid')).toBe(false);
    expect(container.querySelector('#country-code-error')).toBeNull();
  });

  it('clears prior-country defaults and binds edits to a successfully loaded country', async () => {
    const requests = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = new URL(String(input), 'http://localhost');
      if (url.pathname.endsWith('/country-profiles') && !init?.method) return new Response('[]', { status: 200 });
      if (url.pathname.endsWith('/LA') && !init?.method) return new Response(JSON.stringify({ country_code: 'LA', default_locale: 'lo-LA', calendar: 'buddhist', timezone: 'Asia/Vientiane', address_fields: ['province'], identifier_types: ['national-id'], phone_region: 'LA', version: 4 }), { status: 200 });
      if (url.pathname.endsWith('/IN') && !init?.method) return new Response('null', { status: 200 });
      if (url.pathname.endsWith('/IN') && init?.method === 'PUT') return new Response(null, { status: 204 });
      return new Response(JSON.stringify({ error: { code: 'NOT_FOUND', message: 'Not found' } }), { status: 404 });
    });
    vi.stubGlobal('fetch', requests);
    const labels = createUiTranslator({});
    container = document.createElement('div'); document.body.append(container); root = createRoot(container);
    await act(async () => { root?.render(createElement(CountryProfileAdmin, { registryId: 'r', organizationId: 'o', labels })); await new Promise((resolve) => setTimeout(resolve, 0)); });
    const inputs = () => container!.querySelectorAll<HTMLInputElement>('input');
    const buttons = () => container!.querySelectorAll<HTMLButtonElement>('button');
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set?.call(inputs()[0], 'IN');
      inputs()[0].dispatchEvent(new Event('input', { bubbles: true }));
    });
    expect(inputs()[1].disabled).toBe(true);
    expect(buttons()[1].disabled).toBe(true);
    await act(async () => { buttons()[0].click(); await new Promise((resolve) => setTimeout(resolve, 0)); });
    expect(inputs()[1].value).toBe('');
    expect(inputs()[2].value).toBe('');
    expect(inputs()[3].value).toBe('');
    expect(inputs()[4].value).toBe('');
    expect(container.querySelectorAll('textarea')[0].value).toBe('');
    expect(container.querySelectorAll('textarea')[1].value).toBe('');
    expect(buttons()[1].disabled).toBe(true);

    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set?.call(inputs()[1], 'hi-IN');
      inputs()[1].dispatchEvent(new Event('input', { bubbles: true }));
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set?.call(inputs()[3], 'Asia/Kolkata');
      inputs()[3].dispatchEvent(new Event('input', { bubbles: true }));
    });
    expect(buttons()[1].disabled).toBe(false);
    await act(async () => { buttons()[1].click(); await new Promise((resolve) => setTimeout(resolve, 0)); });
    const save = requests.mock.calls.find(([input, init]) => new URL(String(input), 'http://localhost').pathname.endsWith('/IN') && init?.method === 'PUT');
    expect(save).toBeDefined();
    expect(JSON.parse(String(save?.[1]?.body))).toMatchObject({ country_code: 'IN', default_locale: 'hi-IN', timezone: 'Asia/Kolkata', address_fields: [], identifier_types: [] });
  });

  it('keeps profile editing disabled after a failed load until retry succeeds', async () => {
    const requests = vi.fn(async (input: RequestInfo | URL) => {
      const path = new URL(String(input), 'http://localhost').pathname;
      if (path.endsWith('/country-profiles')) return new Response('[]', { status: 200 });
      if (requests.mock.calls.filter(([requested]) => new URL(String(requested), 'http://localhost').pathname.endsWith('/LA')).length === 1) return new Response(JSON.stringify({ error: { code: 'TEMPORARY', message: 'Private detail' } }), { status: 503 });
      return new Response(JSON.stringify({ country_code: 'LA', default_locale: 'lo-LA', calendar: 'gregory', timezone: 'Asia/Vientiane', address_fields: ['province'], identifier_types: ['national-id'], phone_region: 'LA', version: 2 }), { status: 200 });
    });
    vi.stubGlobal('fetch', requests);
    const labels = createUiTranslator({});
    container = document.createElement('div'); document.body.append(container); root = createRoot(container);
    await act(async () => { root?.render(createElement(CountryProfileAdmin, { registryId: 'r', organizationId: 'o', labels })); await new Promise((resolve) => setTimeout(resolve, 0)); });
    expect(container.querySelectorAll<HTMLInputElement>('input')[1].disabled).toBe(true);
    expect(container.querySelectorAll<HTMLButtonElement>('button')[1].disabled).toBe(true);
    expect(container.textContent).not.toContain('Private detail');
    await act(async () => { container?.querySelectorAll<HTMLButtonElement>('button')[0].click(); await new Promise((resolve) => setTimeout(resolve, 0)); });
    expect(container.querySelectorAll<HTMLInputElement>('input')[1].disabled).toBe(false);
    expect(container.querySelectorAll<HTMLButtonElement>('button')[1].disabled).toBe(false);
  });

  it('ignores an earlier country response after the selected country changes', async () => {
    let resolveFirst!: (response: Response) => void;
    const firstLoad = new Promise<Response>((resolve) => { resolveFirst = resolve; });
    const requests = vi.fn((input: RequestInfo | URL) => {
      const path = new URL(String(input), 'http://localhost').pathname;
      if (path.endsWith('/country-profiles')) return Promise.resolve(new Response('[]', { status: 200 }));
      if (path.endsWith('/LA')) return firstLoad;
      return Promise.resolve(new Response('null', { status: 200 }));
    });
    vi.stubGlobal('fetch', requests);
    const labels = createUiTranslator({});
    container = document.createElement('div'); document.body.append(container); root = createRoot(container);
    await act(async () => { root?.render(createElement(CountryProfileAdmin, { registryId: 'r', organizationId: 'o', labels })); await Promise.resolve(); });
    expect(container.querySelector('.admin-grid')?.getAttribute('aria-busy')).toBe('true');
    const waitingStatus = [...container.querySelectorAll('[role="status"]')]
      .find((status) => status.textContent?.includes('Waiting for server'));
    expect(waitingStatus).toBeDefined();
    expect(waitingStatus?.closest('[aria-busy="true"]')).toBeNull();
    const inputs = () => container!.querySelectorAll<HTMLInputElement>('input');
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set?.call(inputs()[0], 'IN');
      inputs()[0].dispatchEvent(new Event('input', { bubbles: true }));
      resolveFirst(new Response(JSON.stringify({ country_code: 'LA', default_locale: 'lo-LA', calendar: 'buddhist', timezone: 'Asia/Vientiane', address_fields: ['province'], identifier_types: ['national-id'], phone_region: 'LA', version: 5 }), { status: 200 }));
      await firstLoad;
    });
    expect(inputs()[1].value).toBe('');
    expect(inputs()[1].disabled).toBe(true);
    expect(container.querySelectorAll<HTMLButtonElement>('button')[0].disabled).toBe(false);
    await act(async () => { container?.querySelectorAll<HTMLButtonElement>('button')[0].click(); await new Promise((resolve) => setTimeout(resolve, 0)); });
    expect(inputs()[1].disabled).toBe(false);
    expect(inputs()[1].value).toBe('');
  });

  it('uses the published catalog for the person resource version label', async () => {
    const labels = createUiTranslator({
      'registry.person.title': 'Published person heading',
      'registry.person.version': 'Localized revision',
      'registry.person.history': 'Published source history',
    });
    vi.stubGlobal('fetch', vi.fn(async () => new Response(JSON.stringify({ data: {
      public_id: 'person-42', resource_version: 7, source_record_ids: [], aliases: [], assertions: [],
    } }), { status: 200 })));
    container = document.createElement('div'); document.body.append(container); root = createRoot(container);
    await act(async () => {
      root?.render(createElement(MemoryRouter, { initialEntries: ['/persons/person-42'] },
        createElement(Routes, null, createElement(Route, { path: '/persons/:publicId', element: createElement(PersonDetailScreen, { scope: { registryId: 'r', organizationId: 'o' }, labels }) }))));
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(container.textContent).toContain('Localized revision 7');
    expect(container.textContent).not.toContain('version 7');
  });

  it.each([
    {
      name: 'warns reviewers when retrieval is capped',
      candidates: [
        { candidate_record_id: 'candidate-1', candidate_receive_version: 1, score_points: 10, score_type: 'raw_points', outcome: 'review', fields: [], hard_conflicts: [] },
        { candidate_record_id: 'candidate-2', candidate_receive_version: 1, score_points: 8, score_type: 'raw_points', outcome: 'review', fields: [], hard_conflicts: [] },
      ],
      retrieval: { per_pass_candidates: { exact_name: 5, partial_birth_year: 2, authoritative_identifier: 1, name_variant: 3, incomplete_name: 2 }, union_before_limit: 8, returned_candidates: 2, truncated: true, limit: 2, complete: true },
      expected: 'Showing 2 of 8 possible matches (limit: 2). Some records were not shown or reviewed. This list is not enough to tell whether the people are the same.',
      warning: true,
    },
    {
      name: 'shows counts when retrieval is not capped',
      candidates: [{ candidate_record_id: 'candidate-1', candidate_receive_version: 1, score_points: 10, score_type: 'raw_points', outcome: 'review', fields: [], hard_conflicts: [] }],
      retrieval: { per_pass_candidates: { exact_name: 1, partial_birth_year: 0, authoritative_identifier: 0 }, union_before_limit: 1, returned_candidates: 1, truncated: false, limit: 50, complete: true },
      expected: 'Showing 1 of 1 candidate (limit: 50).',
      warning: false,
    },
    {
      name: 'preserves the no-candidate caution for an empty retrieval',
      candidates: [],
      retrieval: { per_pass_candidates: { exact_name: 0, partial_birth_year: 0, authoritative_identifier: 0 }, union_before_limit: 0, returned_candidates: 0, truncated: false, limit: 50, complete: true },
      expected: 'Showing 0 of 0 candidates (limit: 50).',
      warning: false,
    },
    {
      name: 'does not present reconstructed legacy counts as exact',
      candidates: [{ candidate_record_id: 'candidate-1', candidate_receive_version: 1, score_points: 10, score_type: 'raw_points', outcome: 'review', fields: [], hard_conflicts: [] }],
      retrieval: { per_pass_candidates: { exact_name: 1, partial_birth_year: 0, authoritative_identifier: 0 }, union_before_limit: 1, returned_candidates: 1, truncated: false, limit: 1, complete: false },
      expected: 'Exact candidate counts were not saved. Candidates shown: 1. Counts from each search are minimums; omitted candidates cannot be recovered from this saved case.',
      warning: true,
    },
  ])('$name', async ({ candidates, retrieval, expected, warning }) => {
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const path = new URL(String(input), 'http://localhost').pathname;
      if (path === '/api/v1/review-cases' && (!init?.method || init.method === 'GET')) return new Response(JSON.stringify({ data: [] }), { status: 200 });
      if (path === '/api/v1/match' && init?.method === 'POST') return new Response(JSON.stringify({ data: {
        evidence_run_id: 'synthetic-run', query_record_id: 'query-record-1', query_receive_version: 1,
        candidates, retrieval, mutation_authority: 'advisory_only',
      } }), { status: 200 });
      return new Response(JSON.stringify({ error: { code: 'NOT_FOUND', message: 'Not found' } }), { status: 404 });
    });
    vi.stubGlobal('fetch', fetchMock);
    container = document.createElement('div'); document.body.append(container); root = createRoot(container);
    await act(async () => { root?.render(createElement(ReviewWorkspace, { scope: { registryId: 'registry', organizationId: 'org' }, labels: createUiTranslator(undefined) })); await new Promise((resolve) => setTimeout(resolve, 0)); });
    const input = container.querySelector<HTMLInputElement>('#review-query-source')!;
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set?.call(input, 'query-record-1');
      input.dispatchEvent(new Event('input', { bubbles: true }));
    });
    await act(async () => { container?.querySelector('.review-open-form')?.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })); await new Promise((resolve) => setTimeout(resolve, 0)); });

    const summary = container.querySelector('.retrieval-summary');
    expect(container.querySelectorAll('.retrieval-summary')).toHaveLength(1);
    expect(summary?.getAttribute('role')).toBe('status');
    expect(summary?.getAttribute('aria-live')).toBe('polite');
    expect(summary?.textContent).toContain(expected);
    expect(summary?.textContent).toContain('Exact normalized name');
    expect(summary?.textContent).toContain(String(retrieval.per_pass_candidates.exact_name));
    const passCounts = Object.fromEntries(Array.from(summary?.querySelectorAll('dl > div') ?? []).map((row) => [row.querySelector('dt')?.textContent, row.querySelector('dd')?.textContent]));
    expect(passCounts['Similar name spelling']).toBe(String(retrieval.per_pass_candidates.name_variant ?? 0));
    expect(passCounts['Partial or reordered name']).toBe(String(retrieval.per_pass_candidates.incomplete_name ?? 0));
    expect(summary?.classList.contains('alert-warning')).toBe(warning);
    if (retrieval.union_before_limit === 0) expect(container?.textContent).toContain('No candidate was returned for this source record');
    expect(fetchMock.mock.calls.some((call) => new URL(String(call[0]), 'http://localhost').pathname === '/api/v1/match')).toBe(true);
  });

  it('loads the exact saved evidence run for a review case without recomputing matches', async () => {
    const caseRow = {
      case_id: 'case-1', registry_id: 'registry', organization_id: 'org',
      query_source_record_id: 'query-record-1', candidate_source_record_id: 'candidate-1',
      evidence_run_id: 'run-original', query_receive_version: 1, candidate_receive_version: 1,
      query_person_version: 1, candidate_person_version: 1, case_version: 1, state: 'open', assigned_actor_id: null,
    };
    const source = (sourceRecordId: string, publicId: string) => ({
      public_id: publicId, resource_version: 1, source_record_ids: [sourceRecordId], aliases: [],
      assertions: [
        { source_record_id: sourceRecordId, source_system_id: 'fixture', source_version: 'v1', receive_version: 1, field_path: 'name', original_value: 'Synthetic Person', language_tag: 'en', script: 'Latn' },
        { source_record_id: sourceRecordId, source_system_id: 'fixture', source_version: 'v2', receive_version: 2, field_path: 'name', original_value: 'Later Changed Name', language_tag: 'en', script: 'Latn' },
      ],
    });
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = new URL(String(input), 'http://localhost');
      if (url.pathname === '/api/v1/review-cases' && (!init?.method || init.method === 'GET')) return new Response(JSON.stringify({ data: [caseRow] }), { status: 200 });
      if (url.pathname === '/api/v1/review-cases/case-1') return new Response(JSON.stringify({ data: { case: caseRow, history: [] } }), { status: 200 });
      if (url.pathname === '/api/v1/registries/registry/organizations/org/source-records/query-record-1') return new Response(JSON.stringify({ data: source('query-record-1', 'public-query') }), { status: 200 });
      if (url.pathname === '/api/v1/registries/registry/organizations/org/source-records/candidate-1') return new Response(JSON.stringify({ data: source('candidate-1', 'public-candidate') }), { status: 200 });
      if (url.pathname === '/api/v1/match/evidence/run-original') return new Response(JSON.stringify({ data: {
        evidence_run_id: 'run-original', snapshot_complete: true, query_record_id: 'query-record-1', query_receive_version: 1,
        candidates: [{ candidate_record_id: 'candidate-1', candidate_receive_version: 1, score_points: 42, score_type: 'raw_points', outcome: 'review', fields: [{ field: 'name', state: 'agreement', points: 42, rationale_code: 'EXACT_NAME' }], hard_conflicts: [] }],
        retrieval: { per_pass_candidates: { exact_name: 1 }, union_before_limit: 1, returned_candidates: 1, truncated: false, limit: 50, complete: true }, mutation_authority: 'registry_only',
      } }), { status: 200 });
      return new Response(JSON.stringify({ error: { code: 'NOT_FOUND', message: 'Not found' } }), { status: 404 });
    });
    vi.stubGlobal('fetch', fetchMock);
    container = document.createElement('div'); document.body.append(container); root = createRoot(container);
    await act(async () => { root?.render(createElement(ReviewWorkspace, { scope: { registryId: 'registry', organizationId: 'org' }, labels: createUiTranslator(undefined) })); await new Promise((resolve) => setTimeout(resolve, 0)); });
    await act(async () => { container?.querySelector<HTMLButtonElement>('.case-row')?.click(); await new Promise((resolve) => setTimeout(resolve, 0)); await new Promise((resolve) => setTimeout(resolve, 0)); });

    expect(container.textContent).toContain('Evidence run run-original');
    expect(container.textContent).toContain('42');
    expect(container.textContent).toContain('Synthetic Person');
    expect(container.textContent).not.toContain('Later Changed Name');
    expect(fetchMock.mock.calls.some((call) => new URL(String(call[0]), 'http://localhost').pathname === '/api/v1/match/evidence/run-original' && (!call[1]?.method || call[1].method === 'GET'))).toBe(true);
    expect(fetchMock.mock.calls.some((call) => new URL(String(call[0]), 'http://localhost').pathname === '/api/v1/match' && call[1]?.method === 'POST')).toBe(false);
  });

  it('uses the same published ui catalog across search, review and administration screens', async () => {
    const publishedCatalog = {
      locale: 'xx', namespace: UI_NAMESPACE, revision: 7,
      entries: {
        'registry.search.title': 'Published Search Label',
        'registry.search.submit': 'Published Search Action',
        'registry.search.scopeRequired': 'Published Search Scope Notice',
        'registry.register.title': 'Published Register Label',
        'registry.register.submit': 'Published Register Action',
        'registry.register.scopeRequired': 'Published Register Scope Notice',
        'review.title': 'Published Review Label',
        'review.scopeRequired': 'Published Review Scope Notice',
        'review.emptyQueue': 'Published Empty Queue Notice',
        'translation.title': 'Published Catalog Editor Label',
        'translation.loadKeys': 'Published Load Keys Action',
        'countryProfiles.title': 'Published Country Profiles Label',
        'countryProfiles.profileScope': 'Published Profile Scope Label',
        'countryProfiles.load': 'Published Load Profile Action',
        'shell.nav.search': 'Published Shell Search',
        'shell.signOut': 'Published Sign Out',
        'shell.scope.apply': 'Published Apply Context',
        'shell.footer.coverage': 'Published Coverage Note:',
        'appearanceAdmin.title': 'Published Appearance Editor',
        'appearanceAdmin.brandIdentity': 'Published Brand Identity',
        'appearanceAdmin.token.canvas': 'Published canvas token',
        'appearanceAdmin.error.unavailable': 'Published theme service error',
      }, fallback_locale: 'en',
    };
    const labels = createUiTranslator(publishedCatalog.entries);
    container = document.createElement('div'); document.body.append(container);
    root = createRoot(container);

    const screens: Array<[ReactNode, string[]]> = [
      [createElement(MemoryRouter, null, createElement(SearchScreen, { scope: { registryId: '', organizationId: '' }, labels })), ['Published Search Label', 'Published Search Action', 'Published Search Scope Notice']],
      [createElement(MemoryRouter, null, createElement(RegisterScreen, { scope: { registryId: '', organizationId: '' }, labels })), ['Published Register Label', 'Published Register Action', 'Published Register Scope Notice']],
      [createElement(ReviewWorkspace, { scope: { registryId: '', organizationId: '' }, labels }), ['Published Review Label', 'Published Review Scope Notice', 'Published Empty Queue Notice']],
      [createElement(TranslationManager, { registryId: '', organizationId: '', labels }), ['Published Catalog Editor Label', 'Published Load Keys Action']],
      [createElement(CountryProfileAdmin, { registryId: '', organizationId: '', labels }), ['Published Country Profiles Label', 'Published Profile Scope Label', 'Published Load Profile Action']],
      [createElement(AppearanceAdmin, { registryId: '', organizationId: '', labels }), ['Published Appearance Editor', 'Published Brand Identity', 'Published canvas token']],
    ];
    for (const [screen, expected] of screens) {
      await act(async () => root?.render(screen));
      for (const value of expected) expect(container.textContent).toContain(value);
      if (expected.includes('Published Profile Scope Label')) {
        const suggestions = container.querySelectorAll('#country-profile-region-codes option');
        expect(suggestions).toHaveLength(257);
        expect(container.querySelector('#country-profile-region-codes option[value="LA"]')).not.toBeNull();
      }
      if (expected.includes('Published Catalog Editor Label')) {
        expect(Array.from(container.querySelectorAll('input')).some((input) => input.value === 'ui')).toBe(true);
      }
    }

    expect(publishedCatalog.namespace).toBe('ui');
  });

  it('confirms text-group publication before sending the publish request', async () => {
    const approved = { locale: 'lo', namespace: UI_NAMESPACE, key: 'shell.brand', value: 'ຊື່ອົງກອນ', plural_forms: null, source_revision: 1, state: 'approved', stale: false };
    const entries = [{ key: approved.key, source_text: 'Organization name', source_revision: 1, placeholders: [], plural: false, draft: approved }];
    const publishRequests: Array<{ path: string; body: unknown }> = [];
    vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const path = new URL(String(input), 'http://localhost').pathname;
      if (path.endsWith('/locales') && (!init?.method || init.method === 'GET')) return new Response(JSON.stringify([{ tag: 'lo', direction: 'ltr', enabled: true }]), { status: 200 });
      if (path.endsWith(`/locales/lo/${UI_NAMESPACE}/entries`)) return new Response(JSON.stringify(entries), { status: 200 });
      if (path.endsWith(`/locales/lo/${UI_NAMESPACE}/shell.brand`)) return new Response(JSON.stringify(approved), { status: 200 });
      if (path.endsWith(`/lo/${UI_NAMESPACE}`) && path.startsWith('/api/v1/bundles/')) return new Response(JSON.stringify({ locale: 'lo', namespace: UI_NAMESPACE, revision: 5, entries: {}, fallback_locale: 'en' }), { status: 200 });
      if (path.endsWith(`/locales/lo/${UI_NAMESPACE}/publish`) && init?.method === 'POST') {
        publishRequests.push({ path, body: JSON.parse(String(init.body)) });
        return new Response(JSON.stringify({ locale: 'lo', namespace: UI_NAMESPACE, revision: 6, entries: {}, fallback_locale: 'en' }), { status: 200 });
      }
      return new Response(JSON.stringify({ error: { code: 'NOT_FOUND', message: 'Not found' } }), { status: 404 });
    }));
    container = document.createElement('div'); document.body.append(container); root = createRoot(container);
    await act(async () => { root?.render(createElement(TranslationManager, { registryId: 'registry', organizationId: 'org', labels: createUiTranslator(undefined) })); await new Promise((resolve) => setTimeout(resolve, 0)); });
    await act(async () => {
      const keyInput = container!.querySelector('input[maxlength="128"]') as HTMLInputElement;
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set?.call(keyInput, 'shell.brand');
      keyInput.dispatchEvent(new Event('input', { bubbles: true }));
      Array.from(container!.querySelectorAll('button')).find((button) => button.textContent?.includes('Load messages'))?.click();
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    await act(async () => { container!.querySelector('.catalog-entry')?.dispatchEvent(new MouseEvent('click', { bubbles: true })); });
    const publishTrigger = Array.from(container!.querySelectorAll('button')).find((button) => button.textContent?.includes('Publish translations')) as HTMLButtonElement;
    publishTrigger.focus();
    await act(async () => { publishTrigger.click(); });
    expect(publishRequests).toHaveLength(0);
    expect(container.textContent).toContain('all messages in the lo text group ui, not just the message open now');
    expect(container.textContent).toContain('Current published revision: 5');
    expect(document.activeElement).toBe(container.querySelector('.publish-confirmation .button-primary'));

    await act(async () => { container!.querySelector<HTMLButtonElement>('.publish-confirmation .button-subtle')?.click(); });
    expect(document.activeElement).toBe(publishTrigger);

    await act(async () => { publishTrigger.click(); });
    expect(document.activeElement).toBe(container.querySelector('.publish-confirmation .button-primary'));

    await act(async () => { container!.querySelector<HTMLButtonElement>('.publish-confirmation .button-primary')?.click(); await new Promise((resolve) => setTimeout(resolve, 0)); });
    expect(publishRequests).toEqual([{ path: `/api/v1/config/registry/org/locales/lo/ui/publish`, body: { expected_revision: 5 } }]);
    expect(container.textContent).toContain('Published as revision 6.');
  });

  it('registers missing built-in UI source messages without overwriting existing source text', async () => {
    const existing = [{ key: 'shell.brand', source_text: 'Organization-approved brand', source_revision: 4, placeholders: [], plural: false, draft: null }];
    const writes: Array<{ key: string; body: { text: string; revision: number; placeholders: string[]; plural: boolean } }> = [];
    vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = new URL(String(input), 'http://localhost').pathname;
      if (url.endsWith('/locales') && (!init?.method || init.method === 'GET')) return new Response('[]', { status: 200 });
      if (url.endsWith(`/locales/lo/${UI_NAMESPACE}/entries`)) return new Response(JSON.stringify(existing), { status: 200 });
      if (init?.method === 'PUT' && url.includes(`/sources/${UI_NAMESPACE}/`)) {
        const key = decodeURIComponent(url.split('/').at(-1) ?? '');
        writes.push({ key, body: JSON.parse(String(init.body)) });
        return new Response(null, { status: 204 });
      }
      return new Response(JSON.stringify({ error: { code: 'NOT_FOUND', message: 'Not found' } }), { status: 404 });
    }));
    container = document.createElement('div'); document.body.append(container); root = createRoot(container);
    await act(async () => { root?.render(createElement(TranslationManager, { registryId: 'registry', organizationId: 'org', labels: createUiTranslator(undefined) })); await new Promise((resolve) => setTimeout(resolve, 0)); });
    await act(async () => {
      const button = Array.from(container!.querySelectorAll('button')).find((item) => item.textContent?.includes('Register missing built-in UI source keys'));
      button?.click();
      await new Promise((resolve) => setTimeout(resolve, 25));
    });
    expect(writes).toHaveLength(Object.keys(englishUiMessages).length - 1);
    expect(writes.some((item) => item.key === 'shell.brand')).toBe(false);
    expect(writes.find((item) => item.key === 'review.caseCount')?.body.placeholders).toEqual(['count']);
    expect(writes.every((item) => item.body.revision === 1 && item.body.plural === false)).toBe(true);
    expect(container.textContent).toContain(`Registered ${writes.length} missing built-in UI source keys`);
  });

  it('blocks a draft that adds an unknown placeholder even when all source placeholders remain', async () => {
    const entry = { key: 'sample.greeting', source_text: 'Hello {name}', source_revision: 1, placeholders: ['name'], plural: false, draft: null };
    const draftWrites: string[] = [];
    const requests = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const path = new URL(String(input), 'http://localhost').pathname;
      if (path.endsWith('/locales') && (!init?.method || init.method === 'GET')) return new Response(JSON.stringify([{ tag: 'lo', direction: 'ltr', enabled: true }]), { status: 200 });
      if (path.endsWith('/locales/lo/ui/entries')) return new Response(JSON.stringify([entry]), { status: 200 });
      if (path.endsWith('/locales/lo/ui/sample.greeting') && (!init?.method || init.method === 'GET')) return new Response('null', { status: 200 });
      if (path.endsWith('/lo/ui') && path.startsWith('/api/v1/bundles/')) return new Response('null', { status: 200 });
      if (init?.method === 'PUT' && path.endsWith('/locales/lo/ui/sample.greeting')) {
        draftWrites.push(String(init.body));
        return new Response(null, { status: 204 });
      }
      return new Response(JSON.stringify({ error: { code: 'NOT_FOUND', message: 'Not found' } }), { status: 404 });
    });
    vi.stubGlobal('fetch', requests);
    container = document.createElement('div'); document.body.append(container); root = createRoot(container);
    await act(async () => { root?.render(createElement(TranslationManager, { registryId: 'registry', organizationId: 'org', labels: createUiTranslator(undefined) })); await new Promise((resolve) => setTimeout(resolve, 0)); });

    const keyInput = container.querySelector<HTMLInputElement>('input[maxlength="128"]')!;
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set?.call(keyInput, 'sample.greeting');
      keyInput.dispatchEvent(new Event('input', { bubbles: true }));
      Array.from(container!.querySelectorAll('button')).find((button) => button.textContent?.trim() === 'Load message')?.click();
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    const draftInput = container.querySelector<HTMLTextAreaElement>('textarea[required]')!;
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')?.set?.call(draftInput, 'ສະບາຍດີ {name} {unexpected}');
      draftInput.dispatchEvent(new Event('input', { bubbles: true }));
    });
    const saveButton = Array.from(container.querySelectorAll<HTMLButtonElement>('button')).find((button) => button.textContent?.includes('Save draft'));
    expect(container.textContent).toContain('Every translation form must use exactly the same placeholders as the source message.');
    expect(saveButton?.disabled).toBe(true);
    expect(draftWrites).toHaveLength(0);
  });

  it.each(['vi', 'hi-IN'])('imports built-in %s suggestions as drafts without replacing prior drafts or published values', async (locale) => {
    const catalog = await loadBuiltInDraftCatalog(locale);
    expect(catalog).not.toBeNull();
    const existingDraft = { locale, namespace: UI_NAMESPACE, key: 'shell.brand', value: 'Reviewed brand text', plural_forms: null, source_revision: 1, state: 'approved', stale: false };
    const drafts = new Map([[existingDraft.key, existingDraft]]);
    const writes: Array<{ key: string; body: { value: string; source_revision: number; plural_forms: unknown } }> = [];
    const locales = [{ tag: locale, direction: 'ltr', enabled: true }];
    const sources = Object.entries(englishUiMessages).map(([key, source_text]) => ({ key, source_text, source_revision: 1, placeholders: [...source_text.matchAll(/\{([A-Za-z0-9_.-]+)\}/g)].map((match) => match[1]), plural: false, draft: drafts.get(key) ?? null }));
    const published = { locale, namespace: UI_NAMESPACE, revision: 1, entries: { 'shell.signIn': 'Organization-approved published text' }, fallback_locale: 'en' };
    const importFetch = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const path = new URL(String(input), 'http://localhost').pathname;
      if (path.endsWith('/locales') && (!init?.method || init.method === 'GET')) return new Response(JSON.stringify(locales), { status: 200 });
      if (path.endsWith(`/locales/${locale}/${UI_NAMESPACE}/entries`) && (!init?.method || init.method === 'GET')) return new Response(JSON.stringify(sources.map((entry) => ({ ...entry, draft: drafts.get(entry.key) ?? null }))), { status: 200 });
      if (path.endsWith(`/${locale}/${UI_NAMESPACE}`) && path.startsWith('/api/v1/bundles/')) return new Response(JSON.stringify(published), { status: 200 });
      if (init?.method === 'PUT' && path.includes(`/locales/${locale}/${UI_NAMESPACE}/`)) {
        const key = decodeURIComponent(path.split('/').at(-1) ?? '');
        const body = JSON.parse(String(init.body));
        writes.push({ key, body }); drafts.set(key, { locale, namespace: UI_NAMESPACE, key, ...body, state: 'draft', stale: false });
        return new Response(null, { status: 204 });
      }
      return new Response(JSON.stringify({ error: { code: 'NOT_FOUND', message: 'Not found' } }), { status: 404 });
    });
    vi.stubGlobal('fetch', importFetch);
    container = document.createElement('div'); document.body.append(container); root = createRoot(container);
    await act(async () => { root?.render(createElement(TranslationManager, { registryId: 'registry', organizationId: 'org', labels: createUiTranslator(undefined) })); await new Promise((resolve) => setTimeout(resolve, 0)); });
    await act(async () => {
      Array.from(container!.querySelectorAll('button')).find((button) => button.textContent?.includes('Import built-in draft translations'))?.click();
      await new Promise((resolve) => setTimeout(resolve, 250));
    });
    expect(writes).toHaveLength(Object.keys(catalog!).length - 2);
    expect(writes.some((entry) => entry.key === 'shell.brand' || entry.key === 'shell.signIn')).toBe(false);
    expect(writes.find((entry) => entry.key === 'review.caseCount')?.body.value).toBe(catalog?.['review.caseCount']);
    expect(writes.find((entry) => entry.key === 'review.retrieval.truncated')?.body.value).toBe(catalog?.['review.retrieval.truncated']);
    expect(importFetch.mock.calls.some(([input]) => new URL(String(input), 'http://localhost').pathname.endsWith('/publish'))).toBe(false);
    expect(Object.keys(englishUiMessages).filter((messageKey) => !Object.hasOwn(catalog!, messageKey))).toHaveLength(0);
    expect(container.textContent).toContain(`Imported ${writes.length} draft suggestions; existing drafts and published entries were left unchanged. Review every translation before approval or publication.`);
  });

  it('suggests direction from a newly entered locale script while keeping manual override available', async () => {
    const registered: Array<{ tag: string; direction: string }> = [];
    const locales: Array<{ tag: string; direction: string; enabled: boolean }> = [{ tag: 'en', direction: 'ltr', enabled: true }];
    const requests = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const path = new URL(String(input), 'http://localhost').pathname;
      if (path.endsWith('/locales') && (!init?.method || init.method === 'GET')) return new Response(JSON.stringify(locales), { status: 200 });
      if (path.endsWith('/locales') && init?.method === 'POST') {
        const body = JSON.parse(String(init.body)) as { tag: string; direction: string };
        registered.push(body);
        locales.push({ ...body, enabled: true });
        return new Response(null, { status: 204 });
      }
      return new Response(JSON.stringify({ error: { code: 'NOT_FOUND', message: 'Not found' } }), { status: 404 });
    });
    vi.stubGlobal('fetch', requests);
    container = document.createElement('div'); document.body.append(container); root = createRoot(container);
    await act(async () => { root?.render(createElement(TranslationManager, { registryId: 'registry', organizationId: 'org', labels: createUiTranslator(undefined) })); await new Promise((resolve) => setTimeout(resolve, 0)); });

    const localeInput = container.querySelector<HTMLInputElement>('input[placeholder="vi-VN"]')!;
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set?.call(localeInput, 'pa-Arab');
      localeInput.dispatchEvent(new Event('input', { bubbles: true }));
    });
    const directionSelect = container.querySelectorAll<HTMLSelectElement>('select')[1]!;
    expect(directionSelect.value).toBe('rtl');

    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, 'value')?.set?.call(directionSelect, 'ltr');
      directionSelect.dispatchEvent(new Event('change', { bubbles: true }));
    });
    await act(async () => {
      container!.querySelector('form')?.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(registered).toEqual([{ tag: 'pa-Arab', direction: 'ltr' }]);

    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set?.call(localeInput, 'wo-Gara');
      localeInput.dispatchEvent(new Event('input', { bubbles: true }));
    });
    expect(directionSelect.value).toBe('rtl');
    await act(async () => {
      container!.querySelector('form')?.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(registered).toEqual([{ tag: 'pa-Arab', direction: 'ltr' }, { tag: 'wo-Gara', direction: 'rtl' }]);
  });

  it('uses the configured writing direction when the first locale is selected on load', async () => {
    const requests = vi.fn(async (input: RequestInfo | URL) => {
      const path = new URL(String(input), 'http://localhost').pathname;
      if (path.endsWith('/locales')) return new Response(JSON.stringify([{ tag: 'ar', direction: 'rtl', enabled: true }]), { status: 200 });
      return new Response(JSON.stringify({ error: { code: 'NOT_FOUND', message: 'Not found' } }), { status: 404 });
    });
    vi.stubGlobal('fetch', requests);
    container = document.createElement('div'); document.body.append(container); root = createRoot(container);
    await act(async () => { root?.render(createElement(TranslationManager, { registryId: 'registry', organizationId: 'org', labels: createUiTranslator(undefined) })); await new Promise((resolve) => setTimeout(resolve, 0)); });

    const localeSelect = container.querySelector('select')!;
    expect(localeSelect.value).toBe('ar');
    expect(container.querySelector('.preview-pair p')?.getAttribute('dir')).toBe('rtl');
    expect(container.querySelector('.preview-pair p')?.getAttribute('lang')).toBe('ar');
  });

  it('filters a loaded translation namespace by key with case-insensitive matching and visible counts', async () => {
    const entries = [
      { key: 'person.name', source_text: 'ຊື່ຜູ້ປ່ວຍ', source_revision: 1, placeholders: [], plural: false, draft: null },
      { key: 'person.address', source_text: 'ທີ່ຢູ່', source_revision: 1, placeholders: [], plural: false, draft: null },
      { key: 'review.case', source_text: 'ກໍລະນີກວດສອບ', source_revision: 1, placeholders: [], plural: false, draft: null },
    ];
    vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL) => {
      const path = new URL(String(input), 'http://localhost').pathname;
      if (path.endsWith('/locales')) return new Response(JSON.stringify([{ tag: 'lo', direction: 'ltr', enabled: true }]), { status: 200 });
      if (path.endsWith('/locales/lo/ui/entries')) return new Response(JSON.stringify(entries), { status: 200 });
      if (path.endsWith('/lo/ui') && path.startsWith('/api/v1/bundles/')) return new Response('null', { status: 200 });
      return new Response(JSON.stringify({ error: { code: 'NOT_FOUND', message: 'Not found' } }), { status: 404 });
    }));
    container = document.createElement('div'); document.body.append(container); root = createRoot(container);
    await act(async () => { root?.render(createElement(TranslationManager, { registryId: 'registry', organizationId: 'org', labels: createUiTranslator(undefined) })); await new Promise((resolve) => setTimeout(resolve, 0)); });
    await act(async () => {
      Array.from(container!.querySelectorAll('button')).find((button) => button.textContent?.includes('Load messages'))?.click();
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(container.querySelectorAll('.catalog-entry')).toHaveLength(3);

    const filter = container.querySelector<HTMLInputElement>('input[type="search"]')!;
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set?.call(filter, 'person.addr');
      filter.dispatchEvent(new Event('input', { bubbles: true }));
    });
    expect(Array.from(container.querySelectorAll('.catalog-entry code')).map((item) => item.textContent)).toEqual(['person.address']);
    expect(container.querySelector('.catalog-filter-count')?.textContent).toBe('1 / 3');

    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set?.call(filter, 'REVIEW.');
      filter.dispatchEvent(new Event('input', { bubbles: true }));
    });
    expect(Array.from(container.querySelectorAll('.catalog-entry code')).map((item) => item.textContent)).toEqual(['review.case']);
    expect(container.querySelector('.catalog-filter-count')?.textContent).toBe('1 / 3');
  });

  it('blocks saving a previous key draft after the selected key changes', async () => {
    const draftWrites: string[] = [];
    const requests = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const path = new URL(String(input), 'http://localhost').pathname;
      if (path.endsWith('/locales') && (!init?.method || init.method === 'GET')) return new Response(JSON.stringify([{ tag: 'lo', direction: 'ltr', enabled: true }]), { status: 200 });
      if (path.endsWith('/locales/lo/ui/entries')) return new Response(JSON.stringify([{ key: 'shell.brand', source_text: 'Organization name', source_revision: 1, placeholders: [], plural: false, draft: null }]), { status: 200 });
      if (path.endsWith('/locales/lo/ui/shell.brand') && (!init?.method || init.method === 'GET')) return new Response(JSON.stringify({ locale: 'lo', namespace: 'ui', key: 'shell.brand', value: 'ຊື່ອົງກອນ', plural_forms: null, source_revision: 1, state: 'draft', stale: false }), { status: 200 });
      if (path.endsWith('/lo/ui') && path.startsWith('/api/v1/bundles/')) return new Response(JSON.stringify({ locale: 'lo', namespace: 'ui', revision: 1, entries: {}, fallback_locale: 'en' }), { status: 200 });
      if (path.endsWith('/locales/lo/ui/shell.brand') && init?.method === 'PUT') { draftWrites.push(path); return new Response(null, { status: 204 }); }
      return new Response(JSON.stringify({ error: { code: 'NOT_FOUND', message: 'Not found' } }), { status: 404 });
    });
    vi.stubGlobal('fetch', requests);
    container = document.createElement('div'); document.body.append(container); root = createRoot(container);
    await act(async () => { root?.render(createElement(TranslationManager, { registryId: 'registry', organizationId: 'org', labels: createUiTranslator(undefined) })); await new Promise((resolve) => setTimeout(resolve, 0)); });

    await act(async () => {
      const keyInput = container!.querySelector('input[maxlength="128"]') as HTMLInputElement;
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set?.call(keyInput, 'shell.brand');
      keyInput.dispatchEvent(new Event('input', { bubbles: true }));
      Array.from(container!.querySelectorAll('button')).find((button) => button.textContent?.trim() === 'Load message')?.click();
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(container.textContent).toContain('ຊື່ອົງກອນ');
    const draftForm = () => Array.from(container!.querySelectorAll('form')).find((form) => form.textContent?.includes('Save draft'));
    expect((draftForm()?.querySelector('button') as HTMLButtonElement | null)?.disabled).toBe(false);

    await act(async () => {
      const keyInput = container!.querySelector('input[maxlength="128"]') as HTMLInputElement;
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set?.call(keyInput, 'shell.other');
      keyInput.dispatchEvent(new Event('input', { bubbles: true }));
    });
    expect(container.textContent).toContain('Load message');
    expect((draftForm()?.querySelector('button') as HTMLButtonElement | null)?.disabled).toBe(true);
    await act(async () => {
      draftForm()?.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
      await Promise.resolve();
    });
    expect(draftWrites).toEqual([]);
  });

  it('summarizes namespace coverage with the same precedence as per-key states', async () => {
    const entries = [
      { key: 'missing.key', source_text: 'Missing', source_revision: 1, placeholders: [], plural: false, draft: null },
      { key: 'stale.key', source_text: 'Stale', source_revision: 2, placeholders: [], plural: false, draft: { locale: 'lo', namespace: 'ui', key: 'stale.key', value: 'Old', plural_forms: null, source_revision: 1, state: 'draft' as const, stale: true } },
      { key: 'draft.key', source_text: 'Draft', source_revision: 1, placeholders: [], plural: false, draft: { locale: 'lo', namespace: 'ui', key: 'draft.key', value: 'Draft', plural_forms: null, source_revision: 1, state: 'draft' as const, stale: false } },
      { key: 'review.key', source_text: 'Review', source_revision: 1, placeholders: [], plural: false, draft: { locale: 'lo', namespace: 'ui', key: 'review.key', value: 'Review', plural_forms: null, source_revision: 1, state: 'in_review' as const, stale: false } },
      { key: 'approved.key', source_text: 'Approved', source_revision: 1, placeholders: [], plural: false, draft: { locale: 'lo', namespace: 'ui', key: 'approved.key', value: 'Approved', plural_forms: null, source_revision: 1, state: 'approved' as const, stale: false } },
      { key: 'published.key', source_text: 'Published', source_revision: 1, placeholders: [], plural: false, draft: null },
      { key: 'plural.key', source_text: 'Plural', source_revision: 1, placeholders: ['count'], plural: true, draft: null },
      { key: 'prefix.key', source_text: 'Prefix collision', source_revision: 1, placeholders: [], plural: false, draft: null },
    ];
    const requests = vi.fn(async (input: RequestInfo | URL) => {
      const path = new URL(String(input), 'http://localhost').pathname;
      if (path.endsWith('/locales')) return new Response(JSON.stringify([{ tag: 'lo', direction: 'ltr', enabled: true }]), { status: 200 });
      if (path.endsWith('/locales/lo/ui/entries')) return new Response(JSON.stringify(entries), { status: 200 });
      if (path.endsWith('/lo/ui') && path.startsWith('/api/v1/bundles/')) return new Response(JSON.stringify({
        locale: 'lo', namespace: 'ui', revision: 3,
        entries: { 'draft.key': 'Published too, but draft status wins', 'published.key': 'Published', 'plural.key_one': 'One plural', 'plural.key_other': 'Other plural', 'prefix.key_variant': 'Not a plural form' },
        fallback_locale: 'en',
      }), { status: 200 });
      return new Response(JSON.stringify({ error: { code: 'NOT_FOUND', message: 'Not found' } }), { status: 404 });
    });
    vi.stubGlobal('fetch', requests);
    container = document.createElement('div'); document.body.append(container); root = createRoot(container);
    await act(async () => { root?.render(createElement(TranslationManager, { registryId: 'registry', organizationId: 'org', labels: createUiTranslator(undefined) })); await new Promise((resolve) => setTimeout(resolve, 0)); });
    await act(async () => {
      Array.from(container!.querySelectorAll('button')).find((button) => button.textContent?.includes('Load messages'))?.click();
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    const summary = container.querySelector('.coverage-summary')!;
    expect(summary.getAttribute('lang')).toBe('lo');
    expect(summary.getAttribute('dir')).toBe('ltr');
    expect(summary.textContent).toContain('ui');
    expect(Array.from(summary.querySelectorAll('dd')).map((item) => item.textContent)).toEqual(['8', '2', '1', '1', '1', '1', '2']);
    expect(summary.textContent).toContain('These counts show server workflow states only. They do not certify translation quality');
    expect(summary.textContent).toContain('qualified language or accessibility review');
    expect(Array.from(container.querySelectorAll('.catalog-entry')).map((item) => item.textContent)).toEqual([
      'missing.keyMissingMissing', 'stale.keyStaleStale', 'draft.keyDraftDraft', 'review.keyReviewIn review',
      'approved.keyApprovedApproved', 'published.keyPublishedPublished', 'plural.keyPluralPublished', 'prefix.keyPrefix collisionMissing',
    ]);

    await act(async () => {
      const namespaceInput = Array.from(container!.querySelectorAll('input[maxlength="64"]')).find((input) => (input as HTMLInputElement).value === 'ui') as HTMLInputElement;
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set?.call(namespaceInput, 'other');
      namespaceInput.dispatchEvent(new Event('input', { bubbles: true }));
    });
    expect(container.querySelector('.coverage-summary')).toBeNull();
  });

  it('registers hi-IN before importing its built-in catalog as drafts without publishing', async () => {
    const catalog = await loadBuiltInDraftCatalog('hi-IN');
    expect(catalog).not.toBeNull();

    const locales: Array<{ tag: string; direction: string; enabled: boolean }> = [];
    const registeredLocaleRequests: Array<{ tag: string; direction: string }> = [];
    const drafts = new Map<string, { locale: string; namespace: string; key: string; value: string; plural_forms: null; source_revision: number; state: string; stale: boolean }>();
    const existingDraft = { locale: 'hi-IN', namespace: UI_NAMESPACE, key: 'shell.brand', value: 'पूर्व-समीक्षित संगठन नाम', plural_forms: null, source_revision: 1, state: 'in_review', stale: false };
    drafts.set(existingDraft.key, existingDraft);
    const sourceKeys = Object.entries(englishUiMessages).map(([key, source_text]) => ({
      key, source_text, source_revision: 1,
      placeholders: [...source_text.matchAll(/\{([A-Za-z0-9_.-]+)\}/g)].map((match) => match[1]),
      plural: false,
    }));
    const published = { locale: 'hi-IN', namespace: UI_NAMESPACE, revision: 2, entries: { 'shell.signIn': 'प्रकाशित संगठन साइन-इन पाठ' }, fallback_locale: 'en' };
    const draftWrites: Array<{ key: string; body: { value: string; source_revision: number; plural_forms: unknown } }> = [];
    const requests = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const path = new URL(String(input), 'http://localhost').pathname;
      if (path.endsWith('/locales') && (!init?.method || init.method === 'GET')) return new Response(JSON.stringify(locales), { status: 200 });
      if (path.endsWith('/locales') && init?.method === 'POST') {
        const body = JSON.parse(String(init.body)) as { tag: string; direction: string };
        registeredLocaleRequests.push(body);
        locales.push({ ...body, enabled: true });
        return new Response(null, { status: 204 });
      }
      if (path.endsWith(`/locales/hi-IN/${UI_NAMESPACE}/entries`) && (!init?.method || init.method === 'GET')) {
        return new Response(JSON.stringify(sourceKeys.map((entry) => ({ ...entry, draft: drafts.get(entry.key) ?? null }))), { status: 200 });
      }
      if (path.endsWith('/hi-IN/ui') && path.startsWith('/api/v1/bundles/')) return new Response(JSON.stringify(published), { status: 200 });
      if (init?.method === 'PUT' && path.includes(`/locales/hi-IN/${UI_NAMESPACE}/`)) {
        const key = decodeURIComponent(path.split('/').at(-1) ?? '');
        const body = JSON.parse(String(init.body)) as { value: string; source_revision: number; plural_forms: null };
        draftWrites.push({ key, body });
        drafts.set(key, { locale: 'hi-IN', namespace: UI_NAMESPACE, key, ...body, state: 'draft', stale: false });
        return new Response(null, { status: 204 });
      }
      return new Response(JSON.stringify({ error: { code: 'NOT_FOUND', message: 'Not found' } }), { status: 404 });
    });
    vi.stubGlobal('fetch', requests);
    container = document.createElement('div'); document.body.append(container); root = createRoot(container);
    await act(async () => { root?.render(createElement(TranslationManager, { registryId: 'registry', organizationId: 'org', labels: createUiTranslator(undefined) })); await new Promise((resolve) => setTimeout(resolve, 0)); });

    await act(async () => {
      const localeInput = container!.querySelector('input[placeholder="vi-VN"]') as HTMLInputElement;
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set?.call(localeInput, 'hi-IN');
      localeInput.dispatchEvent(new Event('input', { bubbles: true }));
      container!.querySelector('form')?.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
      await new Promise((resolve) => setTimeout(resolve, 25));
    });
    expect(registeredLocaleRequests).toEqual([{ tag: 'hi-IN', direction: 'ltr' }]);
    expect(container.textContent).toContain('hi-IN · LTR');

    await act(async () => {
      Array.from(container!.querySelectorAll('button')).find((button) => button.textContent?.includes('Import built-in draft translations'))?.click();
      await new Promise((resolve) => setTimeout(resolve, 250));
    });

    expect(draftWrites).toHaveLength(Object.keys(catalog!).length - 2);
    expect(draftWrites.some((entry) => entry.key === 'shell.brand' || entry.key === 'shell.signIn')).toBe(false);
    expect(drafts.get('shell.brand')?.value).toBe(existingDraft.value);
    expect(published.entries['shell.signIn']).toBe('प्रकाशित संगठन साइन-इन पाठ');
    expect(draftWrites.some((entry) => entry.key in published.entries)).toBe(false);
    expect(drafts.get('review.caseCount')?.value).toBe(catalog?.['review.caseCount']);
    expect([...drafts.values()].filter((entry) => entry.state === 'draft')).toHaveLength(draftWrites.length);
    expect(requests.mock.calls.some(([input]) => new URL(String(input), 'http://localhost').pathname.endsWith('/publish'))).toBe(false);
    expect(container.textContent).toContain(`Imported ${draftWrites.length} draft suggestions`);
    expect(container.textContent).toContain('Review every translation before approval or publication');
  });

  it('loads published shell labels into the app from the ui namespace', async () => {
    localStorage.setItem('litecr.registry-context.v1', JSON.stringify({ registryId: 'synthetic-registry', organizationId: 'synthetic-org' }));
    Object.defineProperty(window, 'matchMedia', { configurable: true, value: () => ({ matches: false, addEventListener: vi.fn(), removeEventListener: vi.fn() }) });
    const shellCatalog = {
      locale: 'en', namespace: UI_NAMESPACE, revision: 11, fallback_locale: 'en',
      entries: { 'shell.nav.search': 'Catalog Search Navigation', 'shell.signOut': 'Catalog Sign Out', 'shell.scope.apply': 'Catalog Apply Context', 'shell.footer.coverage': 'Catalog coverage note:' },
    };
    vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL) => {
      const path = new URL(String(input), 'http://localhost').pathname;
      if (path === '/api/v1/access/session') return new Response(JSON.stringify({ actor_kind: 'human', actor_id: 'synthetic', request_id: 'synthetic-request' }), { status: 200 });
      if (path.endsWith('/appearance/preference')) return new Response(JSON.stringify({ color_mode: 'system', density: 'comfortable', high_contrast: false, locale_tag: 'en' }), { status: 200 });
      if (path.endsWith('/appearance')) return new Response('null', { status: 200 });
      if (path.endsWith('/en/ui')) return new Response(JSON.stringify(shellCatalog), { status: 200 });
      return new Response(JSON.stringify({ error: { code: 'NOT_FOUND', message: 'Not found' } }), { status: 404 });
    }));
    container = document.createElement('div'); document.body.append(container);
    root = createRoot(container);
    await act(async () => { root?.render(createElement(MemoryRouter, null, createElement(App))); await new Promise((resolve) => setTimeout(resolve, 0)); });
    expect(container.textContent).toContain('Catalog Search Navigation');
    expect(container.textContent).toContain('Catalog Sign Out');
    expect(container.textContent).toContain('Catalog coverage note:');
    expect(container.textContent).not.toContain('shell.signOut');
    const preferences = container.querySelector<HTMLDetailsElement>('.workspace-preferences');
    expect(preferences?.open).toBe(false);
    expect(preferences?.querySelector('summary')?.textContent).toContain('Language and appearance controls');
    await act(async () => { preferences?.querySelector('summary')?.click(); });
    expect(preferences?.open).toBe(true);
  });

  it('treats a missing published bundle as a normal English-fallback state', async () => {
    localStorage.setItem('litecr.registry-context.v1', JSON.stringify({ registryId: 'synthetic-registry', organizationId: 'synthetic-org' }));
    Object.defineProperty(window, 'matchMedia', { configurable: true, value: () => ({ matches: false, addEventListener: vi.fn(), removeEventListener: vi.fn() }) });
    const requests = vi.fn(async (input: RequestInfo | URL) => {
      const path = new URL(String(input), 'http://localhost').pathname;
      if (path === '/api/v1/access/session') return new Response(JSON.stringify({ actor_kind: 'human', actor_id: 'synthetic', request_id: 'missing-bundle-test' }), { status: 200 });
      if (path.endsWith('/appearance/preference')) return new Response(JSON.stringify({ color_mode: 'system', density: 'comfortable', high_contrast: false, locale_tag: 'en' }), { status: 200 });
      if (path.endsWith('/appearance')) return new Response('null', { status: 200 });
      if (path.endsWith('/en/ui')) return new Response('null', { status: 200 });
      return new Response(JSON.stringify({ error: { code: 'NOT_FOUND', message: 'Not found' } }), { status: 404 });
    });
    vi.stubGlobal('fetch', requests);
    container = document.createElement('div'); document.body.append(container); root = createRoot(container);
    await act(async () => { root?.render(createElement(MemoryRouter, null, createElement(App))); await new Promise((resolve) => setTimeout(resolve, 0)); });
    expect(requests.mock.calls.some(([input]) => new URL(String(input), 'http://localhost').pathname.endsWith('/en/ui'))).toBe(true);
    expect(container.textContent).toContain('No published UI catalog was found. English source labels are shown until a reviewed catalog is published.');
    expect(container.textContent).toContain('Find a registry record');
    expect(container.textContent).toContain('Search by a liteCR public ID or known alias. This search does not check demographic details.');
    expect(container.textContent).not.toContain('matching is not available in this search endpoint');
  });

  it('marks the current navigation destination for assistive technology', async () => {
    Object.defineProperty(window, 'matchMedia', { configurable: true, value: () => ({ matches: false, addEventListener: vi.fn(), removeEventListener: vi.fn() }) });
    vi.stubGlobal('fetch', vi.fn(async () => new Response(JSON.stringify({ actor_kind: 'human', actor_id: 'synthetic-reviewer', request_id: 'accessibility-test' }), { status: 200 })));
    container = document.createElement('div'); document.body.append(container); root = createRoot(container);
    await act(async () => {
      root?.render(createElement(MemoryRouter, { initialEntries: ['/admin/translations'] }, createElement(App)));
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(container.querySelector('a[href="/admin/translations"]')?.getAttribute('aria-current')).toBe('page');
    expect(container.querySelector('a[href="/review"]')?.hasAttribute('aria-current')).toBe(false);
  });

  it('falls back to readable English source copy instead of exposing an internal key', () => {
    const labels = createUiTranslator({});
    expect(labels('registry.search.title')).toBe('Find a registry record');
    expect(labels('registry.search.help')).toBe('Search by a liteCR public ID or known alias. This search does not check demographic details.');
    expect(labels('unknown.example-label')).toBe('Unknown example label');
    expect(labels('unknown.exampleLabel')).toBe('Unknown example label');
    expect(labels('unknown.example-label')).not.toContain('unknown.example-label');
    expect(labels('known.count', 'There are {{count}} items.', { count: 3 })).toBe('There are 3 items.');
  });

  it.each([400, 422])('renders localized generic copy, not server details, for unknown HTTP %i validation errors', async (status) => {
    const privateDetail = 'database column patient_secret failed: value 12345';
    const labels = createUiTranslator({ 'common.error.invalidRequest': 'Localized safe request error.' });
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify({
      error: { code: 'FUTURE_VALIDATION', message: privateDetail },
    }), { status })));
    container = document.createElement('div'); document.body.append(container); root = createRoot(container);
    await act(async () => root?.render(createElement(MemoryRouter, null,
      createElement(SearchScreen, { scope: { registryId: 'r', organizationId: 'o' }, labels }))));

    const input = container.querySelector<HTMLInputElement>('#search-id')!;
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set?.call(input, 'public-id');
      input.dispatchEvent(new Event('input', { bubbles: true }));
      container?.querySelector('form')?.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(container.textContent).toContain('Localized safe request error.');
    expect(container.textContent).not.toContain(privateDetail);
  });

  it('uses a published empty-search message and cataloged service errors on registry and admin screens', async () => {
    const labels = createUiTranslator({
      'registry.search.noResult': 'Published no-result message',
      'common.error.unavailable': 'Published service unavailable message',
      'review.error.unavailable': 'Published review service error',
      'translation.error.unavailable': 'Published catalog service error',
      'countryProfiles.error.unavailable': 'Published profile service error',
      'appearanceAdmin.error.unavailable': 'Published theme service error',
    });
    container = document.createElement('div'); document.body.append(container);
    root = createRoot(container);

    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify({ data: [] }), { status: 200 })));
    await act(async () => root?.render(createElement(MemoryRouter, null, createElement(SearchScreen, { scope: { registryId: 'synthetic-registry', organizationId: 'synthetic-org' }, labels }))));
    await act(async () => {
      const form = container?.querySelector('form');
      form?.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
      await Promise.resolve();
    });
    expect(container.textContent).toContain('Published no-result message');

    const failingFetch = vi.fn().mockResolvedValue(new Response(JSON.stringify({ error: { code: 'TEMPORARY', message: 'Private server detail' } }), { status: 503 }));
    vi.stubGlobal('fetch', failingFetch);
    const failingScreens: Array<[ReactNode, string, boolean]> = [
      [createElement(MemoryRouter, null, createElement(SearchScreen, { scope: { registryId: 'r', organizationId: 'o' }, labels })), 'Published service unavailable message', true],
      [createElement(MemoryRouter, null, createElement(RegisterScreen, { scope: { registryId: 'r', organizationId: 'o' }, labels })), 'Published service unavailable message', true],
      [createElement(ReviewWorkspace, { scope: { registryId: 'r', organizationId: 'o' }, labels }), 'Published review service error', false],
      [createElement(TranslationManager, { registryId: 'r', organizationId: 'o', labels }), 'Published catalog service error', false],
      [createElement(CountryProfileAdmin, { registryId: 'r', organizationId: 'o', labels }), 'Published profile service error', false],
      [createElement(AppearanceAdmin, { registryId: 'r', organizationId: 'o', labels }), 'Published theme service error', false],
    ];
    for (const [screen, expected, submit] of failingScreens) {
      await act(async () => { root?.render(screen); await new Promise((resolve) => setTimeout(resolve, 0)); });
      if (submit) await act(async () => {
        for (const [id, value] of [['source-system-id', 'system-1'], ['source-record-key', 'source-1'], ['source-name', 'Synthetic name']]) {
          const input = container?.querySelector<HTMLInputElement>('#' + id);
          if (input) {
            Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set?.call(input, value);
            input.dispatchEvent(new Event('input', { bubbles: true }));
          }
        }
        container?.querySelector('form')?.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
        await new Promise((resolve) => setTimeout(resolve, 0));
      });
      expect(container.textContent).toContain(expected);
      expect(container.textContent).not.toContain('Private server detail');
    }
  });

  it('uses cataloged review copy and a readable shell fallback without exposing server text', async () => {
    const labels = createUiTranslator({
      'review.error.unauthenticated': 'Published review sign-in message',
      'common.error.unavailable': 'Published session service message',
    });
    container = document.createElement('div'); document.body.append(container);
    root = createRoot(container);

    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify({ error: { code: 'UNAUTHENTICATED', message: 'Private server detail' } }), { status: 401 })));
    await act(async () => { root?.render(createElement(ReviewWorkspace, { scope: { registryId: 'r', organizationId: 'o' }, labels })); await new Promise((resolve) => setTimeout(resolve, 0)); });
    expect(container.textContent).toContain('Published review sign-in message');
    expect(container.textContent).not.toContain('Private server detail');

    localStorage.setItem('litecr.registry-context.v1', JSON.stringify({ registryId: '', organizationId: '' }));
    Object.defineProperty(window, 'matchMedia', { configurable: true, value: () => ({ matches: false, addEventListener: vi.fn(), removeEventListener: vi.fn() }) });
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify({ error: { code: 'TEMPORARY', message: 'Private server detail' } }), { status: 503 })));
    await act(async () => { root?.render(createElement(MemoryRouter, null, createElement(App))); await new Promise((resolve) => setTimeout(resolve, 0)); });
    expect(container.textContent).toContain('Sign-in status could not be checked. Try again.');
    expect(container.textContent).not.toContain('Your changes were not confirmed.');
    expect(container.textContent).not.toContain('Private server detail');
  });
});
