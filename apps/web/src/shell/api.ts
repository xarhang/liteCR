import { csrfToken } from './config';

export type ApiError = { error?: { code?: string; message?: string; retryable?: boolean } };
export class ApiFailure extends Error {
  constructor(
    message: string,
    readonly status: number,
    readonly code?: string,
    readonly retryable = false,
    /** Original server-provided text, retained for diagnostics; UI copy must use the allowlist. */
    readonly rawMessage?: string,
  ) {
    super(message);
  }
}

export async function api<T>(path: string, init: RequestInit = {}): Promise<T> {
  const headers = new Headers(init.headers);
  if (init.body && !headers.has('content-type')) headers.set('content-type', 'application/json');
  if (['POST', 'PUT', 'PATCH', 'DELETE'].includes((init.method ?? 'GET').toUpperCase())) {
    const token = csrfToken();
    // The server issues URL-safe opaque values. Pass a malformed cookie through
    // for controlled server rejection rather than throwing a browser URIError.
    if (token) headers.set('x-csrf-token', token);
  }
  let response: Response;
  try {
    response = await fetch(path, {
      ...init,
      headers,
      credentials: 'same-origin',
      cache: 'no-store',
      referrerPolicy: 'no-referrer',
    });
  } catch {
    throw new ApiFailure('The service could not be reached. Check the connection and retry.', 0, 'NETWORK_ERROR', true);
  }
  if (!response.ok) {
    let body: ApiError = {};
    try { body = await response.json() as ApiError; } catch { /* Do not expose raw server text. */ }
    const code = body.error?.code ?? (response.status === 401 ? 'UNAUTHENTICATED' : undefined);
    const message = safeMessage(response.status);
    throw new ApiFailure(message, response.status, code, body.error?.retryable ?? response.status >= 500, body.error?.message);
  }
  if (response.status === 204 || response.status === 205) return undefined as T;
  const body = await response.text();
  if (!body.trim()) return undefined as T;
  try {
    return JSON.parse(body) as T;
  } catch {
    throw new ApiFailure('The service returned an invalid response.', response.status, 'INVALID_RESPONSE');
  }
}

function safeMessage(status: number): string {
  if (status === 401) return 'Your session has expired. Sign in again to continue.';
  if (status === 403 || status === 404) return 'This action is not available in the current access scope.';
  if (status === 409) return 'The record changed or conflicts with another request. Refresh and review before retrying.';
  if (status === 422 || status === 400) return 'The request could not be completed. Check the submitted values and try again.';
  if (status >= 500) return 'The service is temporarily unavailable. Your changes were not confirmed.';
  return 'The request could not be completed.';
}

export type ApiEnvelope<T> = { data: T; meta?: { api_version?: string; replayed?: boolean } };
export type Person = {
  public_id: string;
  resource_version: number;
  source_record_ids: string[];
  aliases: string[];
  assertions: Array<{
    source_record_id: string;
    source_system_id: string;
    source_version?: string | null;
    receive_version: number;
    field_path: string;
    original_value: string;
    language_tag?: string | null;
    script?: string | null;
  }>;
};
