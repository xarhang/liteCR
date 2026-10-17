import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { FormEvent } from 'react';
import { api, ApiFailure } from '../../shell/api';
import { apiFailureCopy } from '../../shell/apiFailureCopy';
import type { ApiEnvelope } from '../../shell/api';
import { idempotencyKey } from '../../shell/config';
import type { RegistryScope } from '../../shell/config';
import { reviewEnumLabel } from './reviewEnumLabels';
import { canonicalLocaleTag } from '../localization/validation';
import './review.css';

type Props = { scope: RegistryScope; labels: (key: string, sourceFallback?: string, values?: Record<string, string | number>) => string };
type ReviewCase = {
  case_id: string; registry_id: string; organization_id: string;
  query_source_record_id: string; candidate_source_record_id: string;
  evidence_run_id: string | null;
  query_receive_version: number; candidate_receive_version: number;
  query_person_version: number; candidate_person_version: number;
  case_version: number; state: string; assigned_actor_id: string | null;
};
type QueueFilter = 'active' | 'resolved';
type QueueCursor = { queue_filter: 'all' | QueueFilter; after_created_at: string; after_case_id: string };
type QueueEnvelope = ApiEnvelope<ReviewCase[]> & { meta?: ApiEnvelope<ReviewCase[]>['meta'] & { has_more?: boolean; next_cursor?: QueueCursor | null } };
type MatchField = { field: string; state: string; points: number; rationale_code: string };
type Candidate = { candidate_record_id: string; candidate_receive_version: number | null; score_points: number; score_type: string; outcome: string; fields: MatchField[]; hard_conflicts: string[] };
type MatchRetrieval = { per_pass_candidates: Record<string, number>; union_before_limit: number; returned_candidates: number; truncated: boolean; limit: number; complete: boolean };
type MatchResult = { evidence_run_id: string | null; query_record_id: string; query_receive_version: number | null; candidates: Candidate[]; retrieval: MatchRetrieval; mutation_authority: string; snapshot_complete: boolean };
type SourceAssertion = { source_record_id: string; source_system_id: string; source_version: string | null; receive_version: number; field_path: string; original_value: string; language_tag: string | null; script: string | null };
type SourceProjection = { public_id: string; resource_version: number; source_record_ids: string[]; aliases: string[]; assertions: SourceAssertion[] };
type SourceView = SourceProjection & { source_system_id: string; source_version: string | null; receive_version: number; assertions: SourceAssertion[] };
type Decision = { decision_id: string; decision: string; outcome: string; reason_code: string; reason: string; actor_id: string; actor_issuer_url: string; occurred_at: string; merge_id: string | null };
type CaseDetail = { case: ReviewCase; history: Decision[] };
type DecisionKind = 'confirmed_non_match' | 'link' | 'merge' | 'reverse_merge';
type DecisionDraft = { decision: DecisionKind | null; reason_code: string; reason: string; merge_id: string };
type PendingDecision = { caseId: string; key: string; body: Record<string, unknown> };

const emptyDraft: DecisionDraft = { decision: null, reason_code: '', reason: '', merge_id: '' };
const ui = (labels: Props['labels'], key: string, fallback: string, values?: Record<string, string | number>) => labels('review.' + key, fallback, values);

export function ReviewWorkspace({ scope, labels }: Props) {
  const [cases, setCases] = useState<ReviewCase[] | null>(null);
  const [queueFilter, setQueueFilter] = useState<QueueFilter>('active');
  const [hasMoreCases, setHasMoreCases] = useState(false);
  const [nextCaseCursor, setNextCaseCursor] = useState<QueueCursor | null>(null);
  const [loadingMoreCases, setLoadingMoreCases] = useState(false);
  const [selectedId, setSelectedId] = useState('');
  const [detail, setDetail] = useState<CaseDetail | null>(null);
  const [sources, setSources] = useState<[SourceView | null, SourceView | null]>([null, null]);
  const [match, setMatch] = useState<MatchResult | null>(null);
  const [querySourceId, setQuerySourceId] = useState('');
  const [candidates, setCandidates] = useState<Candidate[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [decisionError, setDecisionError] = useState('');
  const [notice, setNotice] = useState('');
  const [draft, setDraft] = useState<DecisionDraft>(emptyDraft);
  const [mergeConfirmationOpen, setMergeConfirmationOpen] = useState(false);
  const [assigneeId, setAssigneeId] = useState('');
  const [pending, setPending] = useState<PendingDecision | null>(null);
  const liveRequest = useRef(0);
  const queueGeneration = useRef(0);
  const caseHeadingRef = useRef<HTMLHeadingElement>(null);
  const queueHeadingRef = useRef<HTMLHeadingElement>(null);
  const decisionErrorRef = useRef<HTMLDivElement>(null);
  const mergeSubmitRef = useRef<HTMLButtonElement>(null);
  const mergeConfirmRef = useRef<HTMLButtonElement>(null);
  const focusMergeConfirmation = useRef(false);
  const returnFocusToMergeSubmit = useRef(false);
  const queueCaseButtons = useRef(new Map<string, HTMLButtonElement>());
  const pendingQueueFocus = useRef<{ caseId: string | null } | null>(null);
  const pendingCaseFocus = useRef<string | null>(null);
  const casesRef = useRef<ReviewCase[] | null>(cases);
  casesRef.current = cases;

  useEffect(() => {
    if (!selectedId || pendingCaseFocus.current !== selectedId || detail?.case.case_id !== selectedId) return;
    pendingCaseFocus.current = null;
    caseHeadingRef.current?.focus();
  }, [selectedId, detail?.case.case_id]);

  useEffect(() => {
    const pendingFocus = pendingQueueFocus.current;
    if (!pendingFocus || cases === null || hasMoreCases) return;
    pendingQueueFocus.current = null;
    if (pendingFocus.caseId) queueCaseButtons.current.get(pendingFocus.caseId)?.focus();
    else queueHeadingRef.current?.focus();
  }, [cases, hasMoreCases]);

  useEffect(() => {
    if (decisionError) decisionErrorRef.current?.focus();
  }, [decisionError]);

  useEffect(() => {
    if (mergeConfirmationOpen && focusMergeConfirmation.current) {
      focusMergeConfirmation.current = false;
      mergeConfirmRef.current?.focus();
    } else if (!mergeConfirmationOpen && returnFocusToMergeSubmit.current) {
      returnFocusToMergeSubmit.current = false;
      mergeSubmitRef.current?.focus();
    }
  }, [mergeConfirmationOpen]);

  const scopeReady = Boolean(scope.registryId && scope.organizationId);
  const base = useMemo(() => '/api/v1/review-cases?registry_id=' + encodeURIComponent(scope.registryId) + '&organization_id=' + encodeURIComponent(scope.organizationId) + '&queue_filter=' + queueFilter + '&limit=50', [scope.registryId, scope.organizationId, queueFilter]);
  const queueUrl = (cursor?: QueueCursor | null) => cursor
    ? base + '&after_created_at=' + encodeURIComponent(cursor.after_created_at) + '&after_case_id=' + encodeURIComponent(cursor.after_case_id)
    : base;

  function storeQueuePage(result: QueueEnvelope) {
    setHasMoreCases(result.meta?.has_more === true);
    const cursor = result.meta?.next_cursor ?? null;
    setNextCaseCursor(cursor?.queue_filter === queueFilter ? cursor : null);
  }

  function changeQueueFilter(filter: QueueFilter) {
    if (filter === queueFilter) return;
    queueGeneration.current += 1;
    setQueueFilter(filter);
    setCases(null);
    setHasMoreCases(false);
    setNextCaseCursor(null);
    setLoadingMoreCases(false);
    setSelectedId('');
    setDetail(null);
    setDraft(emptyDraft);
    setError('');
    setDecisionError('');
    setNotice('');
  }

  const refreshQueue = useCallback(async (preserve = true) => {
    if (!scopeReady) { setCases([]); setHasMoreCases(false); setNextCaseCursor(null); setDetail(null); return; }
    const generation = queueGeneration.current;
    const prior = preserve ? selectedId : '';
    const result = await api<QueueEnvelope>(base);
    if (generation !== queueGeneration.current) return;
    storeQueuePage(result);
    const selectedCase = prior ? casesRef.current?.find((item) => item.case_id === prior) : undefined;
    const nextCases = selectedCase && !result.data.some((item) => item.case_id === prior) ? [...result.data, selectedCase] : result.data;
    setCases(nextCases);
    setSelectedId(prior && (result.data.some((item) => item.case_id === prior) || selectedCase) ? prior : '');
    if (!prior || (!result.data.some((item) => item.case_id === prior) && !selectedCase)) setDetail(null);
  }, [base, scopeReady, selectedId]);

  useEffect(() => {
    let active = true;
    const generation = ++queueGeneration.current;
    if (!scopeReady) { setCases([]); setHasMoreCases(false); setNextCaseCursor(null); setError(''); return; }
    setCases(null); setError('');
    void api<QueueEnvelope>(base).then((result) => { if (active && generation === queueGeneration.current) { setCases(result.data); storeQueuePage(result); } })
      .catch((cause) => { if (active) setError(messageFor(cause, labels)); });
    return () => { active = false; };
  }, [base, scopeReady]);

  async function loadMoreCases() {
    if (!hasMoreCases || !nextCaseCursor || loadingMoreCases) return;
    const generation = queueGeneration.current;
    setLoadingMoreCases(true); setError('');
    try {
      const result = await api<QueueEnvelope>(queueUrl(nextCaseCursor));
      if (generation !== queueGeneration.current) return;
      if (result.meta?.has_more !== true) {
        const existingIds = new Set((casesRef.current ?? []).map((item) => item.case_id));
        pendingQueueFocus.current = { caseId: result.data.find((item) => !existingIds.has(item.case_id))?.case_id ?? null };
      }
      setCases((current) => {
        const byId = new Map((current ?? []).map((item) => [item.case_id, item]));
        for (const item of result.data) byId.set(item.case_id, item);
        return [...byId.values()];
      });
      storeQueuePage(result);
    } catch (cause) { if (generation === queueGeneration.current) setError(messageFor(cause, labels)); }
    finally { if (generation === queueGeneration.current) setLoadingMoreCases(false); }
  }

  const loadCase = useCallback(async (caseId: string) => {
    const current = ++liveRequest.current;
    setSelectedId(caseId); setDetail(null); setSources([null, null]); setMatch(null); setError(''); setNotice('');
    setDecisionError('');
    setCandidates([]); setDraft(emptyDraft);
    setBusy(true);
    try {
      const query = '?registry_id=' + encodeURIComponent(scope.registryId) + '&organization_id=' + encodeURIComponent(scope.organizationId);
      const [caseResult, left, right] = await Promise.all([
        api<ApiEnvelope<CaseDetail>>('/api/v1/review-cases/' + encodeURIComponent(caseId) + query),
        api<ApiEnvelope<SourceProjection>>('/api/v1/registries/' + encodeURIComponent(scope.registryId) + '/organizations/' + encodeURIComponent(scope.organizationId) + '/source-records/' + encodeURIComponent(casesRef.current?.find((item) => item.case_id === caseId)?.query_source_record_id ?? '')),
        api<ApiEnvelope<SourceProjection>>('/api/v1/registries/' + encodeURIComponent(scope.registryId) + '/organizations/' + encodeURIComponent(scope.organizationId) + '/source-records/' + encodeURIComponent(casesRef.current?.find((item) => item.case_id === caseId)?.candidate_source_record_id ?? '')),
      ]);
      if (current !== liveRequest.current) return;
      setDetail(caseResult.data);
      setSources([
        sourceForRecord(left.data, caseResult.data.case.query_source_record_id, caseResult.data.case.query_receive_version),
        sourceForRecord(right.data, caseResult.data.case.candidate_source_record_id, caseResult.data.case.candidate_receive_version),
      ]);
      setAssigneeId(caseResult.data.case.assigned_actor_id ?? '');
      const { evidence_run_id: evidenceRunId } = caseResult.data.case;
      if (evidenceRunId) {
        const evidence = await api<ApiEnvelope<MatchResult>>('/api/v1/match/evidence/' + encodeURIComponent(evidenceRunId) + '?registry_id=' + encodeURIComponent(scope.registryId) + '&organization_id=' + encodeURIComponent(scope.organizationId));
        if (current === liveRequest.current) setMatch(evidence.data);
      }
    } catch (cause) {
      if (current === liveRequest.current) setError(messageFor(cause, labels));
    } finally { if (current === liveRequest.current) setBusy(false); }
  }, [scope.registryId, scope.organizationId]);

  useEffect(() => {
    if (selectedId && casesRef.current?.some((item) => item.case_id === selectedId)) void loadCase(selectedId);
  }, [selectedId, loadCase]);

  async function runMatch(event: FormEvent) {
    event.preventDefault(); setBusy(true); setError(''); setNotice(''); setMatch(null); setCandidates([]);
    try {
      const response = await api<ApiEnvelope<MatchResult>>('/api/v1/match', { method: 'POST', body: JSON.stringify({ registry_id: scope.registryId, organization_id: scope.organizationId, query_source_record_id: querySourceId.trim(), candidate_limit: 50 }) });
      setMatch(response.data); setCandidates(response.data.candidates);
      if (!response.data.candidates.length) setNotice(ui(labels, 'noCandidate', 'No candidate was returned for this source record. This is matching evidence only; it does not prove a person has no duplicate.'));
    } catch (cause) { setError(messageFor(cause, labels)); }
    finally { setBusy(false); }
  }

  async function openCase(candidate: Candidate) {
    if (!match?.query_receive_version || !candidate.candidate_receive_version || !match.evidence_run_id) { setError(ui(labels, 'missingVersion', 'The current source version or evidence run is missing. Refresh the evidence before opening a review case.')); return; }
    setBusy(true); setError(''); setNotice('');
    try {
      const response = await api<ApiEnvelope<ReviewCase>>('/api/v1/review-cases', { method: 'POST', body: JSON.stringify({ registry_id: scope.registryId, organization_id: scope.organizationId, query_source_record_id: match.query_record_id, candidate_source_record_id: candidate.candidate_record_id, expected_query_receive_version: match.query_receive_version, expected_candidate_receive_version: candidate.candidate_receive_version, evidence_run_id: match.evidence_run_id }) });
      await refreshQueue(false);
      setCases((existing) => existing ? [response.data, ...existing.filter((item) => item.case_id !== response.data.case_id)] : [response.data]);
      pendingCaseFocus.current = response.data.case_id;
      setDraft(emptyDraft);
      setSelectedId(response.data.case_id);
      setNotice(ui(labels, 'caseOpened', 'Review case opened. Matching remains advisory; no identity was changed.'));
    } catch (cause) { setError(messageFor(cause, labels)); }
    finally { setBusy(false); }
  }

  async function assignCase(event: FormEvent) {
    event.preventDefault(); if (!detail) return;
    setBusy(true); setError(''); setNotice('');
    try {
      const response = await api<ApiEnvelope<ReviewCase>>('/api/v1/review-cases/' + encodeURIComponent(detail.case.case_id) + '/assignment', { method: 'POST', body: JSON.stringify({ registry_id: scope.registryId, organization_id: scope.organizationId, expected_case_version: detail.case.case_version, assignee_id: assigneeId.trim() }) });
      setDetail((current) => current ? { ...current, case: response.data } : current);
      setNotice(ui(labels, 'assignmentSaved', 'Assignment saved after the server confirmed the update.'));
      await refreshQueue(true);
    } catch (cause) { setError(messageFor(cause, labels)); if (isConflict(cause)) setNotice(ui(labels, 'caseChangedDuringEdit', 'The case changed while you were editing. Refresh the queue and compare the latest versions.')); }
    finally { setBusy(false); }
  }

  async function submitDecision(event: FormEvent) {
    event.preventDefault();
    if (!detail) return;
    if (pending) { await sendDecision(pending); return; }
    if (!draft.decision) { setDecisionError(ui(labels, 'decisionRequired', 'Choose a decision for this case before submitting.')); return; }
    if (draft.reason.length > 2000 || draft.reason.trim().length === 0 || draft.reason_code.trim().length === 0) { setDecisionError(ui(labels, 'reasonRequired', 'Add a reason code and a clear reason before deciding.')); return; }
    if (draft.decision === 'reverse_merge' && !draft.merge_id.trim()) { setDecisionError(ui(labels, 'mergeIdRequired', 'Select or enter the merge ID from the recorded decision history before reversing.')); return; }
    if (draft.decision === 'merge' && !mergeConfirmationOpen) {
      focusMergeConfirmation.current = true;
      setMergeConfirmationOpen(true);
      return;
    }
    setMergeConfirmationOpen(false);
    const body: Record<string, unknown> = {
      registry_id: scope.registryId, organization_id: scope.organizationId,
      expected_case_version: detail.case.case_version,
      expected_query_receive_version: detail.case.query_receive_version,
      expected_candidate_receive_version: detail.case.candidate_receive_version,
      expected_query_person_version: detail.case.query_person_version,
      expected_candidate_person_version: detail.case.candidate_person_version,
      decision: draft.decision, reason_code: draft.reason_code.trim(), reason: draft.reason.trim(),
      idempotency_key: idempotencyKey(), merge_id: draft.decision === 'reverse_merge' ? draft.merge_id.trim() : undefined,
    };
    const request = { caseId: detail.case.case_id, key: body.idempotency_key as string, body };
    setPending(request);
    await sendDecision(request);
  }

  async function sendDecision(request: PendingDecision) {
    if (!detail) return;
    setBusy(true); setError(''); setDecisionError(''); setNotice('');
    try {
      const response = await api<ApiEnvelope<{ case: ReviewCase; decision_id: string | null; outcome: string; merge_id: string | null }>>('/api/v1/review-cases/' + encodeURIComponent(request.caseId) + '/decisions', { method: 'POST', body: JSON.stringify(request.body) });
      setPending(null);
      setNotice(ui(labels, 'decisionCommitted', 'Decision committed') + ': ' + reviewEnumLabel(labels, 'decisionOutcome', response.data.outcome) + (response.meta?.replayed ? ' (' + ui(labels, 'safeReplay', 'safe replay') + ')' : '') + '. ' + ui(labels, 'refreshingQueueHistory', 'The queue and history are being refreshed.'));
      await Promise.all([loadCase(request.caseId), refreshQueue(true)]);
    } catch (cause) {
      setDecisionError(messageFor(cause, labels));
      if (isConflict(cause)) { setPending(null); setNotice(ui(labels, 'decisionConflict', 'A version or identity constraint changed. No success is shown; refresh the case and review current evidence.')); }
      else if (cause instanceof ApiFailure && cause.status === 403) setPending(null);
      else setNotice(ui(labels, 'decisionNotConfirmed', 'The server did not confirm the commit. Retry this exact request to reuse its idempotency key, or refresh the case.'));
    } finally { setBusy(false); }
  }

  const selected = detail?.case;
  const currentCandidate = match?.candidates.find((item) => item.candidate_record_id === selected?.candidate_source_record_id);
  // Historical snapshots keep their original outcome. The display warning also
  // covers older evidence with the same demographic-only agreement pattern.
  const needsPersonSpecificEvidence = currentCandidate && (
    currentCandidate.fields.some((field) => field.rationale_code === 'DEMOGRAPHICS_NOT_PERSON_SPECIFIC') || (
      currentCandidate.fields.some((field) => field.field === 'name' && field.state === 'agreement') &&
      currentCandidate.fields.some((field) => field.field === 'birth_date' && field.state === 'agreement') &&
      !currentCandidate.fields.some((field) => field.field === 'authoritative_identifier' && field.state === 'agreement')
    )
  );
  const completeHistory = detail?.history ?? [];
  const latestMerge = [...completeHistory].reverse().find((item) => item.decision === 'merge' && item.merge_id && !completeHistory.some((next) => next.decision === 'reverse_merge' && next.merge_id === item.merge_id));
  const canDecide = Boolean(selected && (selected.state !== 'decided' && selected.state !== 'closed' || latestMerge));

  function selectCase(caseId: string) {
    pendingCaseFocus.current = caseId;
    setMergeConfirmationOpen(false);
    setDraft(emptyDraft);
    if (selectedId === caseId) void loadCase(caseId);
    else setSelectedId(caseId);
  }

  return <section className="page review-page">
    <div className="page-heading"><div><p className="eyebrow">{ui(labels, 'eyebrow', 'Identity adjudication')}</p><h1>{ui(labels, 'title', 'Review workspace')}</h1><p className="lead">{ui(labels, 'intro', 'Compare original source assertions, inspect versioned match evidence, and record an explicit decision. Matching never changes identity by itself.')}</p></div>
      <button className="button" type="button" onClick={() => { setError(''); void refreshQueue(true).catch((cause) => setError(messageFor(cause, labels))); }} disabled={!scopeReady || busy}>{ui(labels, 'refreshQueue', 'Refresh queue')}</button></div>
    {!scopeReady && <p className="notice" role="status">{ui(labels, 'scopeRequired', 'Set registry and organization context in the header before reviewing cases.')}</p>}
    {error && <div className="alert alert-error" role="alert"><strong>{ui(labels, 'actionNotConfirmed', 'Action not confirmed.')}</strong> {error}</div>}
    {notice && <p className="inline-status" role="status" aria-live="polite">{notice}</p>}

    <div className="review-layout">
      <aside className="panel review-queue" aria-label={ui(labels, 'queueAccessibleName', 'Review case queue')}>
        <div className="review-section-heading"><div><p className="eyebrow">{ui(labels, 'scopedQueue', 'Scoped queue')}</p><h2 ref={queueHeadingRef} tabIndex={-1}>{ui(labels, 'cases', 'Cases')}</h2></div><span className="review-count" aria-label={labels('review.caseCount', '{{count}} cases', { count: cases?.length ?? 0 })}>{cases?.length ?? '—'}</span></div>
        <div className="review-section-heading" role="group" aria-label={ui(labels, 'queueFilter', 'Review queue filter')}>
          <button type="button" className="button" aria-pressed={queueFilter === 'active'} disabled={Boolean(pending)} onClick={() => changeQueueFilter('active')}>{ui(labels, 'activeCases', 'Active cases')}</button>
          <button type="button" className="button" aria-pressed={queueFilter === 'resolved'} disabled={Boolean(pending)} onClick={() => changeQueueFilter('resolved')}>{ui(labels, 'resolvedCases', 'Resolved cases')}</button>
        </div>
        {cases === null ? <p role="status">{ui(labels, 'loadingCases', 'Loading scoped cases…')}</p> : cases.length === 0 ? <p className="help">{ui(labels, 'emptyQueue', 'No cases are currently visible in this scope.')}</p> : <ul className="case-list">
          {cases.map((item) => <li key={item.case_id}><button ref={(element) => { if (element) queueCaseButtons.current.set(item.case_id, element); else queueCaseButtons.current.delete(item.case_id); }} type="button" className={'case-row' + (selectedId === item.case_id ? ' is-selected' : '')} aria-current={selectedId === item.case_id ? 'true' : undefined} disabled={Boolean(pending)} onClick={() => selectCase(item.case_id)}>
            <span className="case-row-top"><strong><bdi dir="ltr">{item.case_id}</bdi></strong><span className={'case-state state-' + safeClass(item.state)}>{reviewEnumLabel(labels, 'caseState', item.state)}</span></span>
            <span className="case-row-pair"><bdi dir="ltr">{item.query_source_record_id}</bdi><span aria-hidden="true">↔</span><bdi dir="ltr">{item.candidate_source_record_id}</bdi></span>
            <span className="case-row-meta">{ui(labels, 'caseVersion', 'Case')} v{item.case_version} · {ui(labels, 'receive', 'receive')} {item.query_receive_version}/{item.candidate_receive_version}</span>
          </button></li>)}
        </ul>}
        {hasMoreCases && <button type="button" className="button review-load-more" onClick={() => void loadMoreCases()} disabled={loadingMoreCases || !nextCaseCursor} aria-describedby="review-queue-continuation-note">
          {loadingMoreCases ? ui(labels, 'loadingMoreCases', 'Loading more cases…') : ui(labels, 'loadMoreCases', 'Load more cases')}
        </button>}
        {hasMoreCases && <p id="review-queue-continuation-note" className="help" role="status">
          {ui(labels, 'moreCasesAvailable', 'More cases are available. New cases may appear after refreshing the queue.')}
        </p>}
        <form className="review-open-form" onSubmit={runMatch}>
          <h3>{ui(labels, 'findCandidates', 'Find records to review')}</h3><p className="help">{ui(labels, 'findCandidatesHelp', 'Enter a source record ID you are allowed to review. Results are suggestions only; liteCR does not change identity records here.')}</p>
          <label htmlFor="review-query-source">{ui(labels, 'querySourceId', 'Source record ID to check')}</label>
          <input id="review-query-source" value={querySourceId} onChange={(event) => setQuerySourceId(event.target.value)} required maxLength={128} dir="ltr" autoComplete="off" />
          <button className="button button-primary" disabled={!scopeReady || busy || Boolean(pending) || !querySourceId.trim()}>{busy ? ui(labels, 'loading', 'Loading…') : ui(labels, 'findCandidatesAction', 'Find candidates')}</button>
          {!selected && match?.retrieval && <RetrievalSummary labels={labels} retrieval={match.retrieval} />}
          {candidates.length > 0 && <ul className="candidate-list" aria-label={ui(labels, 'matchingCandidates', 'Matching candidates')}>{candidates.map((candidate) => <li key={candidate.candidate_record_id}>
            <div><bdi dir="ltr">{candidate.candidate_record_id}</bdi><small>{reviewEnumLabel(labels, 'candidateOutcome', candidate.outcome)} · {ui(labels, 'candidateScore', 'Review order score: {{score}}', { score: candidate.score_points })}</small></div>
            <button type="button" className="button" aria-label={ui(labels, 'openCase', 'Open case') + ': ' + candidate.candidate_record_id} disabled={busy || Boolean(pending)} onClick={() => void openCase(candidate)}>{ui(labels, 'openCase', 'Open case')}</button>
          </li>)}</ul>}
        </form>
      </aside>

      <div className="review-main" aria-busy={busy}>
        {!selected && <div className="panel review-empty"><h2>{ui(labels, 'selectCase', 'Select a review case')}</h2><p>{ui(labels, 'selectCaseHelp', 'Choose a case from this registry’s queue, or find records to open a new review case.')}</p></div>}
        {selected && <>
          <section className="panel review-case-header" aria-labelledby="case-heading"><div><p className="eyebrow">{reviewEnumLabel(labels, 'caseState', selected.state)} · {ui(labels, 'caseVersion', 'case version')} {selected.case_version}</p><h2 id="case-heading" ref={caseHeadingRef} tabIndex={-1}>{ui(labels, 'case', 'Case')} <bdi dir="ltr">{selected.case_id}</bdi></h2><p className="help">{ui(labels, 'registry', 'Registry')} <bdi dir="ltr">{selected.registry_id}</bdi> · {ui(labels, 'organization', 'organization')} <bdi dir="ltr">{selected.organization_id}</bdi></p></div>
            <form className="assignment-form" onSubmit={assignCase}><label htmlFor="review-assignee">{ui(labels, 'assignActorIdSameIssuer', 'Assignee ID (same identity provider)')}</label><div><input id="review-assignee" value={assigneeId} onChange={(event) => setAssigneeId(event.target.value)} required maxLength={256} dir="ltr" /><button className="button" disabled={busy || Boolean(pending) || !assigneeId.trim()}>{ui(labels, 'saveAssignment', 'Save assignment')}</button></div><small>{ui(labels, 'assignmentIssuerHelp', 'Enter a human subject ID from the identity provider used for your sign-in.')}</small><small>{ui(labels, 'assignmentVersionCheck', 'Assignment is version checked; the server confirms before it appears as saved.')}</small></form>
          </section>

          <section className="review-comparison" aria-label={ui(labels, 'comparison', 'Source record comparison')}>
            <SourcePanel labels={labels} title={ui(labels, 'querySource', 'Query source')} sourceId={selected.query_source_record_id} expectedReceive={selected.query_receive_version} personVersion={selected.query_person_version} source={sources[0]} />
            <SourcePanel labels={labels} title={ui(labels, 'candidateSource', 'Candidate source')} sourceId={selected.candidate_source_record_id} expectedReceive={selected.candidate_receive_version} personVersion={selected.candidate_person_version} source={sources[1]} />
          </section>

          <section className="panel evidence-panel"><div className="review-section-heading"><div><p className="eyebrow">{ui(labels, 'advisoryOnly', 'Advisory only')}</p><h2>{ui(labels, 'matchEvidence', 'Match evidence')}</h2></div><span>{currentCandidate ? reviewEnumLabel(labels, 'candidateOutcome', currentCandidate.outcome) : ui(labels, 'noCurrentEvidence', 'No saved evidence for this case')}</span></div>
            {selected.evidence_run_id ? <p className="help">{ui(labels, 'evidenceRun', 'Evidence run')} <bdi dir="ltr">{selected.evidence_run_id}</bdi></p> : <p className="notice" role="status">{ui(labels, 'noSavedEvidence', 'This case has no saved match evidence. Running a new match will not replace the evidence this case was based on.')}</p>}
            {match && (!match.snapshot_complete || !match.retrieval.complete) && <p className="alert alert-warning" role="status">{ui(labels, 'legacyEvidenceIncomplete', 'This older evidence snapshot preserves candidate evidence, but some retrieval details were not stored.')}</p>}
            {match?.retrieval && <RetrievalSummary labels={labels} retrieval={match.retrieval} />}
            {currentCandidate ? <><p className="help score-disclaimer">{ui(labels, 'scoreDisclaimer', 'This score only ranks cases for review. It does not estimate probability or decide identity; only an authorized reviewer can submit an identity decision.')}</p>
              {needsPersonSpecificEvidence && <div className="alert alert-warning" role="status">{reviewEnumLabel(labels, 'rationale', 'DEMOGRAPHICS_NOT_PERSON_SPECIFIC')}</div>}
              {currentCandidate.hard_conflicts.length > 0 && <div className="alert alert-warning"><strong>{ui(labels, 'conflictSignals', 'Conflict signals')}:</strong> {currentCandidate.hard_conflicts.map((code) => reviewEnumLabel(labels, 'conflictSignal', code)).join(', ')}</div>}
              <div className="evidence-table-wrap" role="region" tabIndex={0} aria-label={ui(labels, 'evidenceCaption', 'Field-level evidence for the selected source pair')}><table className="evidence-table"><caption>{ui(labels, 'evidenceCaption', 'Field-level evidence for the selected source pair')}</caption><thead><tr><th scope="col">{ui(labels, 'field', 'Field')}</th><th scope="col">{ui(labels, 'evidenceState', 'Evidence state')}</th><th scope="col">{ui(labels, 'scorePoints', 'Points')}</th><th scope="col">{ui(labels, 'rationaleCode', 'Rationale code')}</th></tr></thead><tbody>
                {currentCandidate.fields.map((field, index) => <tr key={field.field + '-' + index}><th scope="row">{reviewEnumLabel(labels, 'field', field.field)}</th><td><span className={'evidence-state evidence-' + safeClass(field.state)}>{reviewEnumLabel(labels, 'evidenceState', field.state)}</span></td><td>{field.points}</td><td>{reviewEnumLabel(labels, 'rationale', field.rationale_code)}</td></tr>)}
              </tbody></table></div>
            </> : <p className="help">{ui(labels, 'candidateEvidenceMissing', 'The current matching response did not include this candidate. Refresh source data and use an authorized correction workflow before deciding.')}</p>}
          </section>

          {completeHistory.length > 0 && <section className="panel decision-history"><p className="eyebrow">{ui(labels, 'immutableDecisionRecord', 'Immutable decision record')}</p><h2>{ui(labels, 'decisionHistory', 'Decision history')}</h2><ol>{completeHistory.map((item) => <li key={item.decision_id || item.merge_id || item.occurred_at || item.decision}>
            <div className="history-meta"><strong>{reviewEnumLabel(labels, 'decision', item.decision)}</strong><span>{reviewEnumLabel(labels, 'decisionOutcome', item.outcome)}</span><time dateTime={item.occurred_at}>{item.occurred_at}</time></div>
            <p>{ui(labels, 'actor', 'Actor')}: <bdi dir="ltr">{item.actor_id}</bdi></p>
            <p>{item.reason_code}: <bdi dir="auto">{item.reason}</bdi></p>
            {item.merge_id && <p>{ui(labels, 'mergeId', 'Merge ID')}: <code dir="ltr">{item.merge_id}</code></p>}
            {item.decision === 'merge' && item.merge_id && !completeHistory.some((next) => next.decision === 'reverse_merge' && next.merge_id === item.merge_id) && <button type="button" className="button" disabled={!canDecide || busy} onClick={() => setDraft({ decision: 'reverse_merge', reason_code: 'compensating_correction', reason: '', merge_id: item.merge_id ?? '' })}>{ui(labels, 'prepareReversal', 'Prepare reversal of this merge')}</button>}
          </li>)}</ol></section>}

          {canDecide && <form className="panel decision-form" onSubmit={submitDecision}>
            <p className="eyebrow">{ui(labels, 'explicitAdjudication', 'Explicit adjudication')}</p><h2>{ui(labels, 'recordDecision', 'Record a decision')}</h2>
            {decisionError && <div className="alert alert-error decision-error" role="alert" tabIndex={-1} ref={decisionErrorRef}><strong>{ui(labels, 'actionNotConfirmed', 'Action not confirmed.')}</strong> {decisionError}</div>}
            <fieldset disabled={busy || Boolean(pending)}><legend>{ui(labels, 'decisionType', 'Decision type')}</legend><div className="decision-options">
              {decisionOptions.map((option) => <label key={option.value}><input type="radio" name="decision" value={option.value} checked={draft.decision === option.value} disabled={selected.state === 'decided' && option.value !== 'reverse_merge'} onChange={() => { setMergeConfirmationOpen(false); setDraft((value) => ({ ...value, decision: option.value })); }} /><span><strong>{ui(labels, option.labelKey, option.label)}</strong><small>{ui(labels, option.descriptionKey, option.description)}</small></span></label>)}
            </div></fieldset>
            {draft.decision === 'merge' && <div className="merge-consequence" role="note"><strong>{ui(labels, 'mergeConsequence', 'What happens when you merge')}:</strong> {ui(labels, 'mergeConsequenceHelp', 'liteCR keeps a redirect for the old public ID, retains the original source details, and records the change. The server checks the current record versions and may stop the merge if identity links have changed.')}</div>}
            {draft.decision === 'reverse_merge' && <div className="merge-consequence" role="note"><strong>{ui(labels, 'compensatingReversal', 'Reverse this merge')}:</strong> {ui(labels, 'compensatingReversalHelp', 'This records a reversal; it does not erase the earlier merge. Later source edits or related merges may prevent reversal, so the server can require a new review.')}</div>}
            {draft.decision === 'reverse_merge' && <div className="review-field"><label htmlFor="reverse-merge-id">{ui(labels, 'recordedMergeId', 'Recorded merge ID')}</label><input id="reverse-merge-id" dir="ltr" value={draft.merge_id} onChange={(event) => setDraft((value) => ({ ...value, merge_id: event.target.value }))} required maxLength={128} /></div>}
            <div className="review-fields-grid"><div className="review-field"><label htmlFor="reason-code">{ui(labels, 'reasonCode', 'Reason code')}</label><input id="reason-code" value={draft.reason_code} onChange={(event) => { setMergeConfirmationOpen(false); setDraft((value) => ({ ...value, reason_code: event.target.value })); }} required maxLength={128} /></div><div className="review-field"><label htmlFor="decision-reason">{ui(labels, 'decisionReason', 'Decision reason')}</label><textarea id="decision-reason" value={draft.reason} onChange={(event) => { setMergeConfirmationOpen(false); setDraft((value) => ({ ...value, reason: event.target.value })); }} required maxLength={2000} rows={4} /></div></div>
            <p className="help">{ui(labels, 'expectedVersionsHelp', 'This decision will be checked against case version')} {selected.case_version}; {ui(labels, 'receives', 'source record versions')} {selected.query_receive_version}/{selected.candidate_receive_version}; {ui(labels, 'people', 'identity versions')} {selected.query_person_version}/{selected.candidate_person_version}. {ui(labels, 'staleVersionHelp', 'If any version has changed, refresh the case and review it again.')}</p>
            <div className="decision-actions">{pending && <button type="button" className="button" disabled={busy} onClick={() => void sendDecision(pending)}>{ui(labels, 'retrySameRequest', 'Retry same request safely')}</button>}
              {!mergeConfirmationOpen && <button ref={mergeSubmitRef} className="button button-primary" disabled={busy || Boolean(pending) || !draft.decision || !draft.reason.trim()}>{busy ? ui(labels, 'waitingForServer', 'Waiting for server…') : ui(labels, 'submitDecision', 'Submit decision')}</button>}
            </div>
            {mergeConfirmationOpen && draft.decision === 'merge' && <section className="merge-consequence" role="group" aria-labelledby="merge-confirmation-title">
              <h3 id="merge-confirmation-title">{ui(labels, 'mergeConsequence', 'Review merge consequence')}</h3>
              <p>{ui(labels, 'mergeConsequenceHelp', 'liteCR keeps a redirect for the old public ID, retains the original source details, and records the change. The server checks the current record versions and may stop the merge if identity links have changed.')}</p>
              <dl><div><dt>{ui(labels, 'querySource', 'Query source')}</dt><dd><bdi dir="ltr">{sources[0]?.public_id ?? '—'}</bdi></dd></div><div><dt>{ui(labels, 'candidateSource', 'Candidate source')}</dt><dd><bdi dir="ltr">{sources[1]?.public_id ?? '—'}</bdi></dd></div></dl>
              <p>{ui(labels, 'expectedVersionsHelp', 'This decision will be checked against case version')} {selected.case_version}; {ui(labels, 'receives', 'source record versions')} {selected.query_receive_version}/{selected.candidate_receive_version}; {ui(labels, 'people', 'identity versions')} {selected.query_person_version}/{selected.candidate_person_version}.</p>
              <p>{ui(labels, 'reasonCode', 'Reason code')}: <bdi dir="auto">{draft.reason_code}</bdi> · {ui(labels, 'decisionReason', 'Decision reason')}: <bdi dir="auto">{draft.reason}</bdi></p>
              <div className="button-row"><button ref={mergeConfirmRef} className="button button-primary">{ui(labels, 'decision.merge', 'Merge identities')}</button><button type="button" className="button button-subtle" onClick={() => { returnFocusToMergeSubmit.current = true; setMergeConfirmationOpen(false); }}>{labels('translation.cancelPublish', 'Cancel')}</button></div>
            </section>}
          </form>}
          {!canDecide && <p className="notice" role="status">{ui(labels, 'caseReadOnly', 'This case is')} {reviewEnumLabel(labels, 'caseState', selected.state)}. {ui(labels, 'historyReadOnly', 'History is read-only; open a new review case for a correction.')}</p>}
        </>}
      </div>
    </div>
  </section>;
}

function RetrievalSummary({ labels, retrieval }: { labels: Props['labels']; retrieval: MatchRetrieval }) {
  const counts = {
    found: retrieval.union_before_limit,
    returned: retrieval.returned_candidates,
    limit: retrieval.limit,
    candidateLabel: retrieval.union_before_limit === 1 ? ui(labels, 'retrieval.candidate', 'candidate') : ui(labels, 'retrieval.candidates', 'candidates'),
  };
  const message = !retrieval.complete
    ? ui(labels, 'retrieval.snapshotIncomplete', 'Exact candidate counts were not saved. Candidates shown: {{returned}}. Counts from each search are minimums; omitted candidates cannot be recovered from this saved case.', counts)
    : retrieval.truncated
      ? ui(labels, 'retrieval.truncated', 'Showing {{returned}} of {{found}} possible matches (limit: {{limit}}). Some records were not shown or reviewed. This list is not enough to tell whether the people are the same.', counts)
      : ui(labels, 'retrieval.complete', 'Showing {{returned}} of {{found}} {{candidateLabel}} (limit: {{limit}}).', counts);
  const passes = [
    ['exact_name', 'retrieval.pass.exactName', 'Exact normalized name'],
    ['partial_birth_year', 'retrieval.pass.birthYear', 'Partial birth year'],
    ['name_variant', 'retrieval.pass.nameVariant', 'Similar name spelling'],
    ['incomplete_name', 'retrieval.pass.incompleteName', 'Partial or reordered name'],
    ['authoritative_identifier', 'retrieval.pass.identifier', 'Authoritative identifier'],
  ] as const;
  return <div className={'retrieval-summary ' + (retrieval.truncated || !retrieval.complete ? 'alert alert-warning' : 'notice')} role="status" aria-live="polite" aria-atomic="true">
    <p>{message}</p>
    <dl aria-label={ui(labels, 'retrieval.passCounts', 'Candidate counts by retrieval pass')}>
      {passes.map(([pass, key, fallback]) => <div key={pass}><dt>{ui(labels, key, fallback)}</dt><dd>{retrieval.per_pass_candidates[pass] ?? 0}</dd></div>)}
    </dl>
  </div>;
}

const decisionOptions: Array<{ value: DecisionKind; labelKey: string; label: string; descriptionKey: string; description: string }> = [
  { value: 'confirmed_non_match', labelKey: 'decision.nonMatch', label: 'Not the same person', descriptionKey: 'decision.nonMatchHelp', description: 'Record that these source records belong to different people. This blocks a later merge for this pair.' },
  { value: 'link', labelKey: 'decision.link', label: 'Link these identities', descriptionKey: 'decision.linkHelp', description: 'Link the identities without merging their records.' },
  { value: 'merge', labelKey: 'decision.merge', label: 'Merge these identities', descriptionKey: 'decision.mergeHelp', description: 'Merge the identities. liteCR keeps a redirect for the old public ID and records the change.' },
  { value: 'reverse_merge', labelKey: 'decision.reverseMerge', label: 'Reverse a merge', descriptionKey: 'decision.reverseMergeHelp', description: 'Record a reversal if later changes and identity links allow it.' },
];

function SourcePanel({ labels, title, sourceId, expectedReceive, personVersion, source }: { labels: Props['labels']; title: string; sourceId: string; expectedReceive: number; personVersion: number; source: SourceView | null }) {
  return <section className="panel source-panel"><div className="source-title"><div><p className="eyebrow">{title}</p><h3><bdi dir="ltr">{source?.public_id ?? ui(labels, 'identityDetailsLoading', 'Identity details loading')}</bdi></h3></div><span className="version-chip">{ui(labels, 'receive', 'receive')} v{expectedReceive} · {ui(labels, 'person', 'person')} v{personVersion}</span></div>
    <dl className="source-meta"><div><dt>{ui(labels, 'sourceRecord', 'Source record')}</dt><dd><bdi dir="ltr">{sourceId}</bdi></dd></div><div><dt>{ui(labels, 'sourceSystem', 'Source system')}</dt><dd><bdi dir="ltr">{source?.source_system_id ?? '—'}</bdi></dd></div><div><dt>{ui(labels, 'sourceVersion', 'Source version')}</dt><dd><bdi dir="ltr">{source?.source_version ?? ui(labels, 'notSupplied', 'not supplied')}</bdi></dd></div><div><dt>{ui(labels, 'currentReceivedVersion', 'Current received version')}</dt><dd>{source?.receive_version ?? ui(labels, 'loading', 'Loading')}</dd></div></dl>
    <div className="assertions"><h4>{ui(labels, 'originalAssertions', 'Original assertions')}</h4>
      {source === null
        ? <p role="status">{ui(labels, 'loadingAssertions', 'Loading authorized assertions…')}</p>
        : source.assertions.length === 0
          ? <p className="help">{ui(labels, 'noAssertions', 'No assertions are available for this source record.')}</p>
          : <dl>{source.assertions.map((assertion, index) => {
            const languageTag = assertion.language_tag ? canonicalLocaleTag(assertion.language_tag) : null;
            return <div className="assertion-item" key={assertion.field_path + '-' + index}>
              <dt>{assertion.field_path.replaceAll('_', ' ')}</dt>
              <dd><bdi dir="auto" lang={languageTag ?? undefined}>{assertion.original_value}</bdi></dd>
              <small>{assertion.language_tag ?? ui(labels, 'languageUnknown', 'language unknown')} · {assertion.script ?? ui(labels, 'scriptUnknown', 'script unknown')} · {ui(labels, 'sourceReceive', 'source receive')} v{assertion.receive_version}</small>
            </div>;
          })}</dl>}
    </div>
  </section>;
}

function sourceForRecord(source: SourceProjection, sourceRecordId: string, receiveVersion: number): SourceView {
  const assertions = source.assertions.filter((item) => item.source_record_id === sourceRecordId && item.receive_version === receiveVersion);
  const metadata = assertions[0];
  return { ...source, assertions, source_system_id: metadata?.source_system_id ?? '—', source_version: metadata?.source_version ?? null, receive_version: metadata?.receive_version ?? 0 };
}

function safeClass(value: string) { return value.toLowerCase().replace(/[^a-z0-9_-]/g, '-'); }
function isConflict(cause: unknown) { return cause instanceof ApiFailure && cause.status === 409; }
function messageFor(cause: unknown, labels: Props['labels']) {
  if (cause instanceof ApiFailure) {
    const translated = apiFailureCopy(cause, labels);
    if (translated) return translated;
  }
  if (cause instanceof ApiFailure && cause.status === 409) return ui(labels, 'error.conflict', 'The case or identity changed since it was loaded. Refresh to see current versions before deciding.');
  if (cause instanceof ApiFailure && cause.status === 403) return ui(labels, 'error.forbidden', 'Your current access scope does not permit this action.');
  if (cause instanceof ApiFailure && cause.status === 401) return ui(labels, 'error.unauthenticated', 'Your session has expired. Sign in again to continue.');
  if (cause instanceof ApiFailure && cause.status === 404) return ui(labels, 'error.scope', 'This action is not available in the current access scope.');
  if (cause instanceof ApiFailure && cause.status === 0) return ui(labels, 'error.network', 'The service could not be reached. Check the connection and retry.');
  if (cause instanceof ApiFailure && cause.status >= 500) return ui(labels, 'error.unavailable', 'The service is temporarily unavailable. Your changes were not confirmed.');
  return cause instanceof Error ? cause.message : ui(labels, 'error.requestFailed', 'The request could not be completed.');
}
