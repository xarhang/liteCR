import { afterEach, describe, expect, it, vi } from 'vitest';
import { api } from './api';

afterEach(() => vi.unstubAllGlobals());

describe('same-origin API client', () => {
  it('handles malformed CSRF cookie encoding as a controlled API failure', async () => {
    vi.stubGlobal('document', { cookie: 'litecr_csrf=%malformed' });
    const fetchMock = vi.fn().mockResolvedValue(new Response('{}', { status: 403 }));
    vi.stubGlobal('fetch', fetchMock);
    await expect(api('/api/v1/person-search', { method: 'POST', body: '{}' })).rejects.toMatchObject({ status: 403 });
    expect(fetchMock.mock.calls[0][1].headers.get('x-csrf-token')).toBe('%malformed');
  });
  it('uses cookies but avoids browser HTTP caching and referrer disclosure', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify({ ok: true }), { status: 200 }));
    vi.stubGlobal('fetch', fetchMock);
    await expect(api<{ ok: boolean }>('/api/v1/person-search')).resolves.toEqual({ ok: true });
    expect(fetchMock.mock.calls[0][1]).toMatchObject({
      credentials: 'same-origin', cache: 'no-store', referrerPolicy: 'no-referrer',
    });
  });

  it('conceals scoped 403/404 details behind a generic message', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify({
      error: { code: 'PERSON_NOT_FOUND', message: 'The requested person was not found.' },
    }), { status: 404 })));
    await expect(api('/api/v1/registries/r/organizations/o/persons/p')).rejects.toMatchObject({
      status: 404, message: 'This action is not available in the current access scope.',
      code: 'PERSON_NOT_FOUND', rawMessage: 'The requested person was not found.',
    });
  });

  it.each([400, 422])('keeps unknown validation details out of ApiFailure.message for HTTP %i', async (status) => {
    const privateDetail = 'database column patient_secret failed: value 12345';
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify({
      error: { code: 'FUTURE_VALIDATION', message: privateDetail },
    }), { status })));

    await expect(api('/api/v1/source-records')).rejects.toMatchObject({
      status,
      message: 'The request could not be completed. Check the submitted values and try again.',
      code: 'FUTURE_VALIDATION',
      rawMessage: privateDetail,
    });
  });

  it('does not expose server text for an unrecognized client-error status', async () => {
    const privateDetail = 'internal rate-limit bucket patient-search';
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify({
      error: { code: 'RATE_LIMITED', message: privateDetail },
    }), { status: 429 })));

    await expect(api('/api/v1/person-search')).rejects.toMatchObject({
      status: 429,
      message: 'The request could not be completed.',
      rawMessage: privateDetail,
    });
  });
});
