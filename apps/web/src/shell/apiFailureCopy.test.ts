import { describe, expect, it } from 'vitest';
import { englishUiMessages } from '../../../../packages/localization/src/webCatalogPreview';
import { loadBuiltInDraftCatalog } from '../features/localization/builtinDraftCatalogs';
import { ApiFailure } from './api';
import { apiFailureCopy } from './apiFailureCopy';

const labels = (key: string, fallback?: string) => `localized:${key}:${fallback ?? ''}`;

describe('API failure localization', () => {
  it('maps an allowlisted code only when its fixed server message matches', () => {
    const failure = new ApiFailure('This action is not available in the current access scope.', 403, 'ACCESS_DENIED', false, 'The requested operation is not permitted.');
    expect(apiFailureCopy(failure, labels)).toBe('localized:common.error.scope:This action is not available in the current access scope.');
    expect(failure).toMatchObject({ status: 403, code: 'ACCESS_DENIED', rawMessage: 'The requested operation is not permitted.' });
  });

  it('uses localized generic copy for unknown codes and changed 400/422 messages', () => {
    const unknown = new ApiFailure('Safe fallback text', 422, 'FUTURE_VALIDATION', false, 'field `name` is invalid');
    expect(apiFailureCopy(unknown, labels)).toBe('localized:common.error.invalidRequest:The request could not be completed. Check the submitted values and try again.');
    expect(unknown.rawMessage).toBe('field `name` is invalid');

    const changed = new ApiFailure('Safe fallback text', 422, 'ACCESS_DENIED', false, 'Permission was denied for field: organization_id');
    expect(apiFailureCopy(changed, labels)).toBe('localized:common.error.invalidRequest:The request could not be completed. Check the submitted values and try again.');
    expect(changed.rawMessage).toContain('organization_id');
  });

  it('maps stable admin validation errors to actionable localized copy', () => {
    for (const message of ['Country profile request could not be completed.', 'Appearance request could not be completed.', 'Catalog request could not be completed.']) {
      const failure = new ApiFailure(message, 400, 'INVALID_CONFIG', false, message);
      expect(apiFailureCopy(failure, labels)).toBe('localized:common.error.invalidSettings:Some settings could not be saved. Check required fields and formats, then try again.');
    }
    const changed = new ApiFailure('Unsafe server detail', 400, 'INVALID_CONFIG', false, 'Invalid value for secret setting database_url');
    expect(apiFailureCopy(changed, labels)).toBe('localized:common.error.invalidRequest:The request could not be completed. Check the submitted values and try again.');
  });

  it('keeps the generic validation key in the English source and all four editable draft catalogs', async () => {
    const key = 'common.error.invalidRequest';
    expect(englishUiMessages[key]).toBeTruthy();
    for (const locale of ['lo', 'th', 'vi', 'hi-IN']) {
      const catalog = await loadBuiltInDraftCatalog(locale);
      expect(catalog?.[key], `${locale} draft entry`).toBeTruthy();
      expect(catalog?.[key]).not.toBe(englishUiMessages[key]);
    }
  });
});
