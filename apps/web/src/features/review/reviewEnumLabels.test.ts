import { describe, expect, it } from 'vitest';
import { createUiTranslator } from '../../../../../packages/localization/src/webCatalogPreview';
import { reviewEnumLabel } from './reviewEnumLabels';
import laoCatalog from '../../../../../packages/localization/catalogs/lo.json';

describe('review enum labels', () => {
  it('explains demographic ambiguity in the Lao draft without exposing internal codes', () => {
    const labels = createUiTranslator(laoCatalog);
    expect(reviewEnumLabel(labels, 'field', 'identity_disambiguation')).toBe('ຫຼັກຖານສະເພາະບຸກຄົນ');
    expect(reviewEnumLabel(labels, 'rationale', 'DEMOGRAPHICS_NOT_PERSON_SPECIFIC')).toContain('ລູກແຝດ');
    expect(reviewEnumLabel(labels, 'rationale', 'NAME_VARIANT_RETRIEVAL_ONLY')).toContain('ຍັງບໍ່ຢືນຢັນ');
    expect(reviewEnumLabel(labels, 'rationale', 'NAME_INCOMPLETE_RETRIEVAL_ONLY')).toContain('ຊື່ບາງສ່ວນ');
  });
  it('uses published catalog text for allowlisted reviewer and matcher values', () => {
    const labels = createUiTranslator({
      'review.state.open': 'Localized open',
      'review.outcome.conflictReview': 'Localized conflict review',
      'review.evidenceState.partialAgreement': 'Localized partial agreement',
      'review.fieldLabel.birthDate': 'Localized birth date',
      'review.decision.reverseMerge': 'Localized reversal',
      'review.decisionOutcome.accepted': 'Localized accepted',
      'review.rationale.birthYearConflict': 'Localized year conflict rationale',
      'review.conflictSignal.authoritativeIdentifierCollision': 'Localized identifier conflict',
    });

    expect(reviewEnumLabel(labels, 'caseState', 'open')).toBe('Localized open');
    expect(reviewEnumLabel(labels, 'candidateOutcome', 'conflict_review')).toBe('Localized conflict review');
    expect(reviewEnumLabel(labels, 'evidenceState', 'partial_agreement')).toBe('Localized partial agreement');
    expect(reviewEnumLabel(labels, 'field', 'birth_date')).toBe('Localized birth date');
    expect(reviewEnumLabel(labels, 'decision', 'reverse_merge')).toBe('Localized reversal');
    expect(reviewEnumLabel(labels, 'decisionOutcome', 'accepted')).toBe('Localized accepted');
    expect(reviewEnumLabel(labels, 'rationale', 'DOB_YEAR_CONFLICT')).toBe('Localized year conflict rationale');
    expect(reviewEnumLabel(labels, 'conflictSignal', 'AUTHORITATIVE_IDENTIFIER_COLLISION')).toBe('Localized identifier conflict');
  });

  it('leaves unknown API enum values visible verbatim', () => {
    const labels = createUiTranslator(undefined);
    expect(reviewEnumLabel(labels, 'caseState', 'future_case_state')).toBe('future_case_state');
    expect(reviewEnumLabel(labels, 'candidateOutcome', 'new_outcome')).toBe('new_outcome');
    expect(reviewEnumLabel(labels, 'evidenceState', 'new_evidence_state')).toBe('new_evidence_state');
    expect(reviewEnumLabel(labels, 'field', 'new_field')).toBe('new_field');
    expect(reviewEnumLabel(labels, 'decision', 'new_decision')).toBe('new_decision');
    expect(reviewEnumLabel(labels, 'decisionOutcome', 'new_decision_outcome')).toBe('new_decision_outcome');
    expect(reviewEnumLabel(labels, 'rationale', 'NEW_RATIONALE_CODE')).toBe('NEW_RATIONALE_CODE');
    expect(reviewEnumLabel(labels, 'conflictSignal', 'NEW_CONFLICT_SIGNAL')).toBe('NEW_CONFLICT_SIGNAL');
  });
});
