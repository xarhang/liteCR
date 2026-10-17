import { ApiFailure } from './api';

type Labeler = (key: string, sourceFallback?: string) => string;
type CopyRule = { key: string; fallback: string; serverMessages: readonly string[] };

// These exact phrases are currently emitted beside stable codes by crates/api.
// A changed or dynamic phrase is never rendered; 400/422 use generic localized copy.
const rules: Readonly<Record<string, CopyRule>> = {
  NETWORK_ERROR: { key: 'common.error.network', fallback: 'The service could not be reached. Check the connection and retry.', serverMessages: ['The service could not be reached. Check the connection and retry.'] },
  ACCESS_DENIED: { key: 'common.error.scope', fallback: 'This action is not available in the current access scope.', serverMessages: ['The requested operation is not permitted.'] },
  UNAUTHENTICATED: { key: 'review.error.unauthenticated', fallback: 'Your session has expired. Sign in again to continue.', serverMessages: ['Authentication is required.'] },
  PERSON_NOT_FOUND: { key: 'common.error.scope', fallback: 'This action is not available in the current access scope.', serverMessages: ['The requested person was not found.'] },
  SOURCE_RECORD_NOT_FOUND: { key: 'common.error.scope', fallback: 'This action is not available in the current access scope.', serverMessages: ['The requested source record was not found.'] },
  REVIEW_CASE_NOT_FOUND: { key: 'common.error.scope', fallback: 'This action is not available in the current access scope.', serverMessages: ['The review case was not found.'] },
  NOT_FOUND: { key: 'common.error.scope', fallback: 'This action is not available in the current access scope.', serverMessages: ['Country profile request could not be completed.', 'Appearance request could not be completed.', 'Catalog request could not be completed.'] },
  REVIEW_CONFLICT: { key: 'review.error.conflict', fallback: 'The case or identity changed since it was loaded. Refresh to see current versions before deciding.', serverMessages: ['The review state changed; refresh before deciding.'] },
  VERSION_CONFLICT: { key: 'common.error.conflict', fallback: 'The record changed or conflicts with another request. Refresh and review before retrying.', serverMessages: ['Country profile request could not be completed.', 'Appearance request could not be completed.', 'Catalog request could not be completed.'] },
  IDEMPOTENCY_KEY_REUSED: { key: 'common.error.conflict', fallback: 'The record changed or conflicts with another request. Refresh and review before retrying.', serverMessages: ['The idempotency key was used for a different command.'] },
  MATCH_SNAPSHOT_STALE: { key: 'common.error.conflict', fallback: 'The record changed or conflicts with another request. Refresh and review before retrying.', serverMessages: ['A source record changed during matching. Request matching again.'] },
  REGISTRY_UNAVAILABLE: { key: 'common.error.unavailable', fallback: 'The service is temporarily unavailable. Your changes were not confirmed.', serverMessages: ['The registry is temporarily unavailable.'] },
  REVIEW_UNAVAILABLE: { key: 'common.error.unavailable', fallback: 'The service is temporarily unavailable. Your changes were not confirmed.', serverMessages: ['The review service is temporarily unavailable.'] },
  MATCHING_UNAVAILABLE: { key: 'common.error.unavailable', fallback: 'The service is temporarily unavailable. Your changes were not confirmed.', serverMessages: ['Matching is temporarily unavailable.'] },
  ACCESS_UNAVAILABLE: { key: 'common.error.unavailable', fallback: 'The service is temporarily unavailable. Your changes were not confirmed.', serverMessages: ['Access service is temporarily unavailable.'] },
  CONFIG_UNAVAILABLE: { key: 'common.error.unavailable', fallback: 'The service is temporarily unavailable. Your changes were not confirmed.', serverMessages: ['Country profile request could not be completed.', 'Appearance request could not be completed.', 'Catalog request could not be completed.'] },
  INVALID_CONFIG: { key: 'common.error.invalidSettings', fallback: 'Some settings could not be saved. Check required fields and formats, then try again.', serverMessages: ['Country profile request could not be completed.', 'Appearance request could not be completed.', 'Catalog request could not be completed.'] },
};

/** Translate only a known code paired with its known fixed server message. */
export function apiFailureCopy(failure: ApiFailure, labels: Labeler): string | undefined {
  const rule = failure.code ? rules[failure.code] : undefined;
  const rawMessage = failure.rawMessage;
  if (rule && rawMessage && rule.serverMessages.includes(rawMessage)) {
    return labels(rule.key, rule.fallback);
  }
  if (failure.status === 400 || failure.status === 422) {
    return labels('common.error.invalidRequest', 'The request could not be completed. Check the submitted values and try again.');
  }
  return undefined;
}
