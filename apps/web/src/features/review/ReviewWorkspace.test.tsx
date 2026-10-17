import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { ReviewWorkspace } from './ReviewWorkspace';
import { createUiTranslator } from '../../../../../packages/localization/src/webCatalogPreview';
import laoCatalog from '../../../../../packages/localization/catalogs/lo.json';

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

describe('review queue pagination', () => {
  it('fetches the continuation cursor and exposes newer cases through a focusable native button', async () => {
    const firstCase = row('case-first-page');
    const newerCase = row('case-next-page');
    const resolvedCase = row('case-resolved', 'decided');
    const requests: URL[] = [];
    vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL) => {
      const url = new URL(String(input), 'http://localhost');
      requests.push(url);
      if (url.pathname !== '/api/v1/review-cases') return new Response(JSON.stringify({ error: { code: 'NOT_FOUND' } }), { status: 404 });
      if (url.searchParams.get('queue_filter') === 'resolved') {
        return new Response(JSON.stringify({ data: [resolvedCase], meta: { has_more: false, next_cursor: null } }), { status: 200 });
      }
      if (url.searchParams.has('after_created_at')) {
        return new Response(JSON.stringify({ data: [newerCase], meta: { has_more: false, next_cursor: null } }), { status: 200 });
      }
      return new Response(JSON.stringify({
        data: [firstCase],
        meta: { has_more: true, next_cursor: { queue_filter: 'active', after_created_at: '2026-10-09T12:34:56.123456Z', after_case_id: '123e4567-e89b-12d3-a456-426614174000' } },
      }), { status: 200 });
    }));
    container = document.createElement('div'); document.body.append(container); root = createRoot(container);

    await act(async () => {
      root?.render(createElement(ReviewWorkspace, {
        scope: { registryId: 'registry', organizationId: 'organization' },
        labels: createUiTranslator(undefined),
      }));
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    const continuation = container.querySelector<HTMLButtonElement>('.review-load-more');
    expect(continuation?.textContent).toContain('Load more cases');
    continuation?.focus();
    expect(document.activeElement).toBe(continuation);
    expect(continuation?.tabIndex).toBe(0);

    await act(async () => {
      continuation?.click();
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(container.textContent).toContain('case-next-page');
    expect(container.querySelector('.review-load-more')).toBeNull();
    const continuationRequest = requests.find((url) => url.searchParams.has('after_created_at'));
    expect(continuationRequest?.searchParams.get('after_created_at')).toBe('2026-10-09T12:34:56.123456Z');
    expect(continuationRequest?.searchParams.get('after_case_id')).toBe('123e4567-e89b-12d3-a456-426614174000');
    const newlyVisibleCase = Array.from(container.querySelectorAll<HTMLButtonElement>('.case-row')).find((button) => button.textContent?.includes('case-next-page'));
    expect(newlyVisibleCase?.tagName).toBe('BUTTON');
    expect(document.activeElement).toBe(newlyVisibleCase);
    expect(newlyVisibleCase?.tabIndex).toBe(0);

    await act(async () => {
      container?.querySelector<HTMLButtonElement>('[aria-pressed="false"]')?.click();
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(container.textContent).toContain('case-resolved');
    expect(requests.some((url) => url.searchParams.get('queue_filter') === 'resolved')).toBe(true);
  });

  it('keeps visually colliding long case and source IDs distinguishable in the queue', async () => {
    const first = {
      ...row('case-id-0000-middle-ALPHA-98765432'),
      query_source_record_id: 'source-id-00-name-ALPHA-87654321',
      candidate_source_record_id: 'source-id-00-target-ALPHA-12345678',
    };
    const second = {
      ...row('case-id-0000-middle-BRAVO-98765432'),
      query_source_record_id: 'source-id-00-name-BRAVO-87654321',
      candidate_source_record_id: 'source-id-00-target-BRAVO-12345678',
    };
    vi.stubGlobal('fetch', vi.fn(async () => new Response(JSON.stringify({ data: [first, second] }), { status: 200 })));
    container = document.createElement('div'); document.body.append(container); root = createRoot(container);

    await act(async () => {
      root?.render(createElement(ReviewWorkspace, {
        scope: { registryId: 'registry', organizationId: 'organization' },
        labels: createUiTranslator(undefined),
      }));
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    const queueRows = Array.from(container.querySelectorAll<HTMLButtonElement>('.case-row'));
    expect(queueRows).toHaveLength(2);
    expect(queueRows[0].textContent).toContain(first.case_id);
    expect(queueRows[0].textContent).toContain(first.query_source_record_id);
    expect(queueRows[0].textContent).toContain(first.candidate_source_record_id);
    expect(queueRows[1].textContent).toContain(second.case_id);
    expect(queueRows[1].textContent).toContain(second.query_source_record_id);
    expect(queueRows[1].textContent).toContain(second.candidate_source_record_id);
    expect(queueRows[0].textContent).not.toContain(second.case_id);
    expect(queueRows[1].textContent).not.toContain(first.case_id);
  });
});

describe('review case selection focus', () => {
  it('moves focus to the selected case after its details load', async () => {
    const reviewCase = row('case-selected');
    const source = (sourceRecordId: string, publicId: string) => ({
      public_id: publicId, resource_version: 1, source_record_ids: [sourceRecordId], aliases: [], assertions: [],
    });
    vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL) => {
      const url = new URL(String(input), 'http://localhost');
      if (url.pathname === '/api/v1/review-cases') return new Response(JSON.stringify({ data: [reviewCase] }), { status: 200 });
      if (url.pathname === '/api/v1/review-cases/case-selected') {
        return new Response(JSON.stringify({ data: { case: reviewCase, history: [] } }), { status: 200 });
      }
      if (url.pathname.endsWith('/source-records/query-record')) {
        return new Response(JSON.stringify({ data: source('query-record', 'person-query') }), { status: 200 });
      }
      if (url.pathname.endsWith('/source-records/candidate-record')) {
        return new Response(JSON.stringify({ data: source('candidate-record', 'person-candidate') }), { status: 200 });
      }
      return new Response(JSON.stringify({ error: { code: 'NOT_FOUND' } }), { status: 404 });
    }));
    container = document.createElement('div'); document.body.append(container); root = createRoot(container);

    await act(async () => {
      root?.render(createElement(ReviewWorkspace, {
        scope: { registryId: 'registry', organizationId: 'organization' },
        labels: createUiTranslator(undefined),
      }));
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    const queueRow = container.querySelector<HTMLButtonElement>('.case-row')!;
    queueRow.focus();
    await act(async () => {
      queueRow.click();
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    const caseHeading = container.querySelector<HTMLElement>('#case-heading');
    expect(caseHeading?.textContent).toContain('case-selected');
    expect(caseHeading?.tabIndex).toBe(-1);
    expect(document.activeElement).toBe(caseHeading);
  });

  it('shows the exact pair and versions before a merge can be submitted', async () => {
    const reviewCase = row('case-merge');
    let committed = false;
    const decisions: Array<Record<string, unknown>> = [];
    vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = new URL(String(input), 'http://localhost');
      if (url.pathname === '/api/v1/review-cases' && init?.method !== 'POST') return new Response(JSON.stringify({ data: [reviewCase] }), { status: 200 });
      if (url.pathname === '/api/v1/review-cases/case-merge') {
        return new Response(JSON.stringify({ data: { case: committed ? { ...reviewCase, state: 'decided' } : reviewCase, history: [] } }), { status: 200 });
      }
      if (url.pathname.endsWith('/source-records/query-record')) return new Response(JSON.stringify({ data: { public_id: 'person-query', resource_version: 3, source_record_ids: ['query-record'], aliases: [], assertions: [] } }), { status: 200 });
      if (url.pathname.endsWith('/source-records/candidate-record')) return new Response(JSON.stringify({ data: { public_id: 'person-candidate', resource_version: 7, source_record_ids: ['candidate-record'], aliases: [], assertions: [] } }), { status: 200 });
      if (url.pathname.endsWith('/decisions') && init?.method === 'POST') {
        decisions.push(JSON.parse(String(init.body)) as Record<string, unknown>);
        committed = true;
        return new Response(JSON.stringify({ data: { case: { ...reviewCase, state: 'decided' }, decision_id: 'decision-merge', outcome: 'merged', merge_id: 'merge-1' } }), { status: 200 });
      }
      return new Response(JSON.stringify({ error: { code: 'NOT_FOUND' } }), { status: 404 });
    }));
    container = document.createElement('div'); document.body.append(container); root = createRoot(container);

    await act(async () => {
      root?.render(createElement(ReviewWorkspace, { scope: { registryId: 'registry', organizationId: 'organization' }, labels: createUiTranslator(undefined) }));
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    await act(async () => { container?.querySelector<HTMLButtonElement>('.case-row')?.click(); await new Promise((resolve) => setTimeout(resolve, 0)); });
    await act(async () => container?.querySelector<HTMLInputElement>('input[name="decision"][value="merge"]')?.click());
    const reasonCode = container.querySelector<HTMLInputElement>('#reason-code')!;
    const reason = container.querySelector<HTMLTextAreaElement>('#decision-reason')!;
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set?.call(reasonCode, 'reviewer_adjudication');
      reasonCode.dispatchEvent(new Event('input', { bubbles: true }));
      Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')?.set?.call(reason, 'Compared the current source assertions');
      reason.dispatchEvent(new Event('input', { bubbles: true }));
    });
    const submit = container.querySelector<HTMLButtonElement>('.decision-actions .button-primary')!;
    submit.focus();
    await act(async () => container?.querySelector<HTMLFormElement>('.decision-form')?.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })));

    const confirmation = container.querySelector<HTMLElement>('[aria-labelledby="merge-confirmation-title"]')!;
    const confirmButton = confirmation.querySelector<HTMLButtonElement>('.button-primary')!;
    expect(confirmation.textContent).toContain('person-query');
    expect(confirmation.textContent).toContain('person-candidate');
    expect(confirmation.textContent).toContain('case version 1');
    expect(confirmation.textContent).toContain('identity versions 1/1');
    expect(confirmation.textContent).toContain('Compared the current source assertions');
    expect(decisions).toHaveLength(0);
    expect(document.activeElement).toBe(confirmButton);

    await act(async () => { confirmation.querySelector<HTMLButtonElement>('.button-subtle')?.click(); });
    expect(decisions).toHaveLength(0);
    expect(document.activeElement).toBe(container.querySelector('.decision-actions .button-primary'));

    await act(async () => container?.querySelector<HTMLFormElement>('.decision-form')?.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })));
    const reopenedConfirmation = container.querySelector<HTMLElement>('[aria-labelledby="merge-confirmation-title"]')!;
    await act(async () => { reopenedConfirmation.querySelector<HTMLButtonElement>('.button-primary')?.click(); await new Promise((resolve) => setTimeout(resolve, 0)); });
    expect(decisions).toHaveLength(1);
    expect(decisions[0]).toMatchObject({ decision: 'merge', reason: 'Compared the current source assertions', reason_code: 'reviewer_adjudication' });
  });

  it('starts without a decision and clears the decision and reason when another case is selected', async () => {
    const firstCase = row('case-first');
    const secondCase = row('case-second');
    const posted: Array<{ path: string; body: Record<string, unknown> }> = [];
    vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = new URL(String(input), 'http://localhost');
      if (url.pathname === '/api/v1/review-cases' && init?.method !== 'POST') return new Response(JSON.stringify({ data: [firstCase, secondCase] }), { status: 200 });
      if (url.pathname === '/api/v1/review-cases/case-first' || url.pathname === '/api/v1/review-cases/case-second') {
        const reviewCase = url.pathname.endsWith('case-first') ? firstCase : secondCase;
        return new Response(JSON.stringify({ data: { case: reviewCase, history: [] } }), { status: 200 });
      }
      if (url.pathname.endsWith('/source-records/query-record')) return new Response(JSON.stringify({ data: { public_id: 'person-query', resource_version: 1, source_record_ids: ['query-record'], aliases: [], assertions: [] } }), { status: 200 });
      if (url.pathname.endsWith('/source-records/candidate-record')) return new Response(JSON.stringify({ data: { public_id: 'person-candidate', resource_version: 1, source_record_ids: ['candidate-record'], aliases: [], assertions: [] } }), { status: 200 });
      if (url.pathname.endsWith('/decisions') && init?.method === 'POST') {
        posted.push({ path: url.pathname, body: JSON.parse(String(init.body)) as Record<string, unknown> });
        const decidedCase = { ...secondCase, state: 'decided' };
        return new Response(JSON.stringify({ data: { case: decidedCase, decision_id: 'decision-second', outcome: 'link', merge_id: null } }), { status: 200 });
      }
      return new Response(JSON.stringify({ error: { code: 'NOT_FOUND' } }), { status: 404 });
    }));
    container = document.createElement('div'); document.body.append(container); root = createRoot(container);

    await act(async () => {
      root?.render(createElement(ReviewWorkspace, { scope: { registryId: 'registry', organizationId: 'organization' }, labels: createUiTranslator(undefined) }));
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    const rows = container.querySelectorAll<HTMLButtonElement>('.case-row');
    await act(async () => { rows[0].click(); await new Promise((resolve) => setTimeout(resolve, 0)); });
    expect(container.querySelector<HTMLInputElement>('input[name="decision"]:checked')).toBeNull();
    expect(container.querySelector<HTMLButtonElement>('.decision-actions .button-primary')?.disabled).toBe(true);

    const firstDecision = container.querySelector<HTMLInputElement>('input[name="decision"][value="confirmed_non_match"]')!;
    await act(async () => { firstDecision.click(); });
    const code = container.querySelector<HTMLInputElement>('#reason-code')!;
    const reason = container.querySelector<HTMLTextAreaElement>('#decision-reason')!;
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set?.call(code, 'reviewer_adjudication');
      code.dispatchEvent(new Event('input', { bubbles: true }));
      Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')?.set?.call(reason, 'Checked the source details');
      reason.dispatchEvent(new Event('input', { bubbles: true }));
    });

    const secondRow = Array.from(container.querySelectorAll<HTMLButtonElement>('.case-row')).find((button) => button.textContent?.includes('case-second'))!;
    await act(async () => { secondRow.click(); await new Promise((resolve) => setTimeout(resolve, 0)); });
    expect(container.querySelector<HTMLInputElement>('input[name="decision"]:checked')).toBeNull();
    expect(container.querySelector<HTMLInputElement>('#reason-code')?.value).toBe('');
    expect(container.querySelector<HTMLTextAreaElement>('#decision-reason')?.value).toBe('');
    expect(container.querySelector<HTMLButtonElement>('.decision-actions .button-primary')?.disabled).toBe(true);

    await act(async () => {
      container?.querySelector<HTMLInputElement>('input[name="decision"][value="link"]')?.click();
      const activeCode = container?.querySelector<HTMLInputElement>('#reason-code')!;
      const activeReason = container?.querySelector<HTMLTextAreaElement>('#decision-reason')!;
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set?.call(activeCode, 'reviewer_adjudication');
      activeCode.dispatchEvent(new Event('input', { bubbles: true }));
      Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')?.set?.call(activeReason, 'Reviewed this case independently');
      activeReason.dispatchEvent(new Event('input', { bubbles: true }));
      container?.querySelector<HTMLFormElement>('.decision-form')?.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(posted).toHaveLength(1);
    expect(posted[0].path).toBe('/api/v1/review-cases/case-second/decisions');
    expect(posted[0].body.decision).toBe('link');
    expect(posted[0].body.reason).toBe('Reviewed this case independently');
  });

  it('keeps a decision retry bound to its case and prevents navigation until the outcome is confirmed', async () => {
    const firstCase = row('case-original');
    const otherCase = row('case-other');
    const postedPaths: string[] = [];
    let decisionAttempts = 0;
    vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = new URL(String(input), 'http://localhost');
      if (url.pathname === '/api/v1/review-cases' && init?.method !== 'POST') {
        return new Response(JSON.stringify({ data: [firstCase, otherCase] }), { status: 200 });
      }
      if (url.pathname === '/api/v1/review-cases/case-original' || url.pathname === '/api/v1/review-cases/case-other') {
        const reviewCase = url.pathname.endsWith('case-original') ? firstCase : otherCase;
        return new Response(JSON.stringify({ data: { case: reviewCase, history: [] } }), { status: 200 });
      }
      if (url.pathname.endsWith('/source-records/query-record')) {
        return new Response(JSON.stringify({ data: { public_id: 'person-query', resource_version: 1, source_record_ids: ['query-record'], aliases: [], assertions: [] } }), { status: 200 });
      }
      if (url.pathname.endsWith('/source-records/candidate-record')) {
        return new Response(JSON.stringify({ data: { public_id: 'person-candidate', resource_version: 1, source_record_ids: ['candidate-record'], aliases: [], assertions: [] } }), { status: 200 });
      }
      if (url.pathname.endsWith('/decisions') && init?.method === 'POST') {
        postedPaths.push(url.pathname);
        decisionAttempts += 1;
        if (decisionAttempts === 1) return new Response(JSON.stringify({ error: { code: 'TEMPORARY_UNAVAILABLE' } }), { status: 503 });
        const decidedCase = { ...firstCase, state: 'decided' };
        Object.assign(firstCase, decidedCase);
        return new Response(JSON.stringify({ data: { case: decidedCase, decision_id: 'decision-1', outcome: 'confirmed_non_match', merge_id: null } }), { status: 200 });
      }
      return new Response(JSON.stringify({ error: { code: 'NOT_FOUND' } }), { status: 404 });
    }));
    container = document.createElement('div'); document.body.append(container); root = createRoot(container);

    await act(async () => {
      root?.render(createElement(ReviewWorkspace, {
        scope: { registryId: 'registry', organizationId: 'organization' },
        labels: createUiTranslator(undefined),
      }));
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    const queueRows = container.querySelectorAll<HTMLButtonElement>('.case-row');
    await act(async () => {
      queueRows[0].click();
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    await act(async () => container?.querySelector<HTMLInputElement>('input[name="decision"][value="confirmed_non_match"]')?.click());
    const reasonCode = container.querySelector<HTMLInputElement>('#reason-code')!;
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set?.call(reasonCode, 'reviewer_adjudication');
      reasonCode.dispatchEvent(new Event('input', { bubbles: true }));
    });
    const reason = container.querySelector<HTMLTextAreaElement>('#decision-reason')!;
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')?.set?.call(reason, 'Reviewed source evidence');
      reason.dispatchEvent(new Event('input', { bubbles: true }));
    });
    await act(async () => {
      container?.querySelector<HTMLFormElement>('.decision-form')?.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(postedPaths).toEqual(['/api/v1/review-cases/case-original/decisions']);
    expect(container.textContent).toContain('Retry same request safely');
    const decisionError = container.querySelector<HTMLDivElement>('.decision-error');
    expect(decisionError?.getAttribute('role')).toBe('alert');
    expect(document.activeElement).toBe(decisionError);
    const otherQueueRow = Array.from(container.querySelectorAll<HTMLButtonElement>('.case-row')).find((button) => button.textContent?.includes('case-other'))!;
    expect(otherQueueRow.disabled).toBe(true);
    await act(async () => otherQueueRow.click());
    expect(container.querySelector('#case-heading')?.textContent).toContain('case-original');

    await act(async () => {
      container?.querySelector<HTMLButtonElement>('.decision-actions button[type="button"]')?.click();
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(postedPaths).toEqual([
      '/api/v1/review-cases/case-original/decisions',
      '/api/v1/review-cases/case-original/decisions',
    ]);
    expect(container.textContent).not.toContain('Retry same request safely');
    expect(container.textContent).toContain('History is read-only');
  });
});

describe('candidate actions', () => {
  it('gives each localized Open case action a distinct accessible name containing its candidate ID', async () => {
    vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL) => {
      const url = new URL(String(input), 'http://localhost');
      if (url.pathname === '/api/v1/review-cases') {
        return new Response(JSON.stringify({ data: [], meta: { has_more: false, next_cursor: null } }), { status: 200 });
      }
      if (url.pathname === '/api/v1/match') {
        return new Response(JSON.stringify({ data: {
          evidence_run_id: 'run-1', query_record_id: 'query-1', query_receive_version: 1,
          candidates: [
            { candidate_record_id: 'candidate-1', candidate_receive_version: 1, score_points: 18, score_type: 'raw_heuristic_points_not_probability', outcome: 'possible_duplicate', fields: [], hard_conflicts: [] },
            { candidate_record_id: 'candidate-2', candidate_receive_version: 1, score_points: 12, score_type: 'raw_heuristic_points_not_probability', outcome: 'possible_duplicate', fields: [], hard_conflicts: [] },
          ],
          retrieval: { per_pass_candidates: {}, union_before_limit: 2, returned_candidates: 2, truncated: false, limit: 50, complete: true },
          mutation_authority: 'registry-only', snapshot_complete: true,
        } }), { status: 200 });
      }
      return new Response(JSON.stringify({ error: { code: 'NOT_FOUND' } }), { status: 404 });
    }));
    container = document.createElement('div'); document.body.append(container); root = createRoot(container);

    await act(async () => {
      root?.render(createElement(ReviewWorkspace, {
        scope: { registryId: 'registry', organizationId: 'organization' },
        labels: createUiTranslator(laoCatalog),
      }));
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    const input = container.querySelector<HTMLInputElement>('#review-query-source')!;
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set?.call(input, 'query-1');
      input.dispatchEvent(new Event('input', { bubbles: true }));
    });
    await act(async () => {
      container?.querySelector<HTMLFormElement>('.review-open-form')?.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    const actions = Array.from(container.querySelectorAll<HTMLButtonElement>('.candidate-list button'));
    expect(actions.map((button) => button.getAttribute('aria-label'))).toEqual([
      'ເປີດກໍລະນີ: candidate-1',
      'ເປີດກໍລະນີ: candidate-2',
    ]);
    expect(actions.map((button) => button.textContent)).toEqual(['ເປີດກໍລະນີ', 'ເປີດກໍລະນີ']);
    expect(container.querySelector('.candidate-list')?.textContent).toContain('ຄະແນນຈັດລຳດັບການກວດ: 18 ຄະແນນ');
    expect(container.querySelector('.candidate-list')?.textContent).not.toContain('raw_heuristic_points_not_probability');
    expect(container.querySelector('.candidate-list')?.textContent).not.toContain('registry-only');
  });

  it.each([false, true])('explains scores and demographic ambiguity for historical evidence=%s', async (historical) => {
    const reviewCase = { ...row('case-score-copy'), evidence_run_id: 'evidence-score-copy' };
    const source = (sourceRecordId: string, publicId: string) => ({
      public_id: publicId, resource_version: 1, source_record_ids: [sourceRecordId], aliases: [],
      assertions: [{
        source_record_id: sourceRecordId, source_system_id: 'synthetic', source_version: 'v1',
        receive_version: 1, field_path: 'name', original_value: 'Synthetic Source Name',
        language_tag: sourceRecordId === 'query-record' ? 'lo-LA' : 'invalid_locale', script: 'Lao',
      }],
    });
    vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL) => {
      const url = new URL(String(input), 'http://localhost');
      if (url.pathname === '/api/v1/review-cases') return new Response(JSON.stringify({ data: [reviewCase] }), { status: 200 });
      if (url.pathname === '/api/v1/review-cases/case-score-copy') return new Response(JSON.stringify({ data: { case: reviewCase, history: [] } }), { status: 200 });
      if (url.pathname.endsWith('/source-records/query-record')) return new Response(JSON.stringify({ data: source('query-record', 'person-query') }), { status: 200 });
      if (url.pathname.endsWith('/source-records/candidate-record')) return new Response(JSON.stringify({ data: source('candidate-record', 'person-candidate') }), { status: 200 });
      if (url.pathname === '/api/v1/match/evidence/evidence-score-copy') return new Response(JSON.stringify({ data: {
        evidence_run_id: 'evidence-score-copy', query_record_id: 'query-record', query_receive_version: 1,
        candidates: [{
          candidate_record_id: 'candidate-record', candidate_receive_version: 1, score_points: 65,
          score_type: 'raw_heuristic_points_not_probability', outcome: historical ? 'review' : 'insufficient_evidence', fields: historical ? [
            { field: 'name', state: 'agreement', points: 35, rationale_code: 'NAME_NORMALIZED_EXACT' },
            { field: 'birth_date', state: 'agreement', points: 30, rationale_code: 'DOB_SAME_PRECISION_VALUE' },
            { field: 'authoritative_identifier', state: 'unknown', points: 0, rationale_code: 'NO_SHARED_AUTHORITY_ID' },
          ] : [{ field: 'identity_disambiguation', state: 'unknown', points: 0, rationale_code: 'DEMOGRAPHICS_NOT_PERSON_SPECIFIC' }], hard_conflicts: [],
        }],
        retrieval: { per_pass_candidates: {}, union_before_limit: 1, returned_candidates: 1, truncated: false, limit: 50, complete: true },
        mutation_authority: 'registry-only', snapshot_complete: true,
      } }), { status: 200 });
      return new Response(JSON.stringify({ error: { code: 'NOT_FOUND' } }), { status: 404 });
    }));
    container = document.createElement('div'); document.body.append(container); root = createRoot(container);

    await act(async () => {
      root?.render(createElement(ReviewWorkspace, {
        scope: { registryId: 'registry', organizationId: 'organization' },
        labels: createUiTranslator(undefined),
      }));
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    await act(async () => {
      container?.querySelector<HTMLButtonElement>('.case-row')?.click();
      await new Promise((resolve) => setTimeout(resolve, 0));
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(container.querySelector('.score-disclaimer')?.textContent).toBe(
      'This score only ranks cases for review. It does not estimate probability or decide identity; only an authorized reviewer can submit an identity decision.',
    );
    expect(container.textContent).not.toContain('raw_heuristic_points_not_probability');
    expect(container.textContent).not.toContain('registry-only');
    expect(container.textContent).toContain('Points');
    expect(container.querySelector('[role="status"].alert-warning')?.textContent).toBe('Names and birth dates can be shared by relatives or twins. Check person-specific evidence before deciding.');
    if (!historical) expect(container.textContent).toContain('Insufficient evidence');
    expect(container.textContent).not.toContain('DEMOGRAPHICS_NOT_PERSON_SPECIFIC');
    const originalNames = container.querySelectorAll<HTMLElement>('.assertion-item dd bdi');
    expect(originalNames[0]?.getAttribute('lang')).toBe('lo-LA');
    expect(originalNames[1]?.hasAttribute('lang')).toBe(false);
  });
});

function row(caseId: string, state = 'open') {
  return {
    case_id: caseId, registry_id: 'registry', organization_id: 'organization',
    query_source_record_id: 'query-record', candidate_source_record_id: 'candidate-record',
    evidence_run_id: null, query_receive_version: 1, candidate_receive_version: 1,
    query_person_version: 1, candidate_person_version: 1, case_version: 1,
    state, assigned_actor_id: null,
  };
}
