type Labeler = (key: string, sourceFallback?: string) => string;

type EnumLabel = readonly [key: string, fallback: string];
const enumLabels = {
  caseState: {
    open: ['review.state.open', 'Open'],
    assigned: ['review.state.assigned', 'Assigned'],
    decided: ['review.state.decided', 'Decided'],
    closed: ['review.state.closed', 'Closed'],
  },
  candidateOutcome: {
    review: ['review.outcome.review', 'Review'],
    insufficient_evidence: ['review.outcome.insufficientEvidence', 'Insufficient evidence'],
    conflict_review: ['review.outcome.conflictReview', 'Conflict review'],
  },
  evidenceState: {
    agreement: ['review.evidenceState.agreement', 'Agreement'],
    partial_agreement: ['review.evidenceState.partialAgreement', 'Partial agreement'],
    conflict: ['review.evidenceState.conflict', 'Conflict'],
    unknown: ['review.evidenceState.unknown', 'Unknown'],
    correlated_weak: ['review.evidenceState.correlatedWeak', 'Correlated weak evidence'],
  },
  field: {
    name: ['review.fieldLabel.name', 'Name'],
    birth_date: ['review.fieldLabel.birthDate', 'Birth date'],
    authoritative_identifier: ['review.fieldLabel.authoritativeIdentifier', 'Authoritative identifier'],
    contact: ['review.fieldLabel.contact', 'Contact'],
    address: ['review.fieldLabel.address', 'Address'],
    identity_disambiguation: ['review.fieldLabel.identityDisambiguation', 'Person-specific evidence'],
  },
  decision: {
    confirmed_non_match: ['review.decision.nonMatch', 'Records belong to different people'],
    link: ['review.decision.link', 'Link identities'],
    merge: ['review.decision.merge', 'Merge identities'],
    reverse_merge: ['review.decision.reverseMerge', 'Reverse a prior merge'],
  },
  decisionOutcome: {
    accepted: ['review.decisionOutcome.accepted', 'Accepted'],
    stale: ['review.decisionOutcome.stale', 'Stale'],
    conflict: ['review.decisionOutcome.conflict', 'Conflict'],
    requires_review: ['review.decisionOutcome.requiresReview', 'Requires review'],
  },
  rationale: {
    NAME_NORMALIZED_EXACT_TAGGED_VARIANT: ['review.rationale.nameNormalizedExactTaggedVariant', 'Exact name match using a tagged source variant'],
    NAME_NORMALIZED_EXACT: ['review.rationale.nameNormalizedExact', 'Exact normalized name match'],
    NAME_NO_EXACT_BLOCK: ['review.rationale.nameNoExactBlock', 'Names differ; no exact-name retrieval block'],
    NAME_MISSING: ['review.rationale.nameMissing', 'Name evidence unavailable'],
    NAME_VARIANT_RETRIEVAL_ONLY: ['review.rationale.nameVariantRetrievalOnly', 'Similar spelling retrieved this candidate; it does not confirm identity or count as an exact name match.'],
    NAME_INCOMPLETE_RETRIEVAL_ONLY: ['review.rationale.nameIncompleteRetrievalOnly', 'Partial or reordered name parts retrieved this candidate. Check the full original name and person-specific evidence.'],
    DOB_SAME_PRECISION_VALUE: ['review.rationale.birthDateSamePrecision', 'Birth dates agree at supplied precision'],
    DOB_PARTIAL_YEAR_AGREEMENT: ['review.rationale.birthYearPartialAgreement', 'Birth years agree at partial precision'],
    DOB_YEAR_CONFLICT: ['review.rationale.birthYearConflict', 'Birth years conflict'],
    DOB_KNOWN_COMPONENT_CONFLICT: ['review.rationale.birthDateComponentConflict', 'Birth dates disagree on a known month or day; verify the source dates.'],
    DOB_UNKNOWN_OR_UNPARSEABLE: ['review.rationale.birthDateUnknown', 'Birth-date evidence is missing or unparseable'],
    AUTHORITY_ID_EXPLICIT_COLLISION: ['review.rationale.authorityIdentifierCollision', 'Source reports an authoritative identifier collision'],
    AUTHORITY_TYPE_VALUE_EXACT: ['review.rationale.authorityTypeValueExact', 'Same assigning authority, identifier type, and value'],
    NO_SHARED_AUTHORITY_ID: ['review.rationale.noSharedAuthorityIdentifier', 'No shared authoritative identifier'],
    SHARED_CONTACT_NOT_INDEPENDENT: ['review.rationale.sharedContactWeak', 'Shared contact is weak evidence; it may be reused'],
    SHARED_ADDRESS_NOT_INDEPENDENT: ['review.rationale.sharedAddressWeak', 'Shared address is weak evidence; it may be shared'],
    DEMOGRAPHICS_NOT_PERSON_SPECIFIC: ['review.rationale.demographicsNotPersonSpecific', 'Names and birth dates can be shared by relatives or twins. Check person-specific evidence before deciding.'],
  },
  conflictSignal: {
    DOB_KNOWN_COMPONENT_CONFLICT: ['review.rationale.birthDateComponentConflict', 'Birth dates disagree on a known month or day; verify the source dates.'],
    DOB_YEAR_CONFLICT: ['review.conflictSignal.birthYearConflict', 'Birth years conflict'],
    AUTHORITATIVE_IDENTIFIER_COLLISION: ['review.conflictSignal.authoritativeIdentifierCollision', 'Conflicting authoritative identifier'],
  },
} satisfies Record<string, Record<string, EnumLabel>>;

export type ReviewEnumGroup = keyof typeof enumLabels;

/** Translate only known API/domain enum values; unknown values remain visible verbatim. */
export function reviewEnumLabel(labels: Labeler, group: ReviewEnumGroup, value: string): string {
  const groupLabels: Record<string, EnumLabel> = enumLabels[group];
  const entry = groupLabels[value];
  return entry ? labels(entry[0], entry[1]) : value;
}
