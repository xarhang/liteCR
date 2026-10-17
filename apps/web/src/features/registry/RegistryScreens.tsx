import { useEffect, useRef, useState } from 'react';
import type { FormEvent } from 'react';
import { Link, useNavigate, useParams } from 'react-router-dom';
import { api, ApiFailure } from '../../shell/api';
import { apiFailureCopy } from '../../shell/apiFailureCopy';
import type { ApiEnvelope, Person } from '../../shell/api';
import { idempotencyKey } from '../../shell/config';
import type { RegistryScope } from '../../shell/config';
import { regionOptions } from '../country_profiles/regions';

type Props = { scope: RegistryScope; labels: (key: string, sourceFallback?: string, values?: Record<string, string | number>) => string };
type Registration = {
  source_system_id: string; source_record_key: string; source_version: string;
  name: string; language_tag: string; script: string; birth_date: string;
  identifier_authority: string; identifier_type: string; identifier_value: string; source_country: string;
};
const blankRegistration: Registration = {
  source_system_id: '', source_record_key: '', source_version: '1', name: '', language_tag: '', script: '',
  birth_date: '', identifier_authority: '', identifier_type: '', identifier_value: '', source_country: '',
};
type CountryProfileSuggestions = { country_code: string; identifier_types: string[] };

export function SearchScreen({ scope, labels }: Props) {
  const [id, setId] = useState(''); const [results, setResults] = useState<Person[] | null>(null);
  const [busy, setBusy] = useState(false); const [error, setError] = useState('');
  async function submit(event: FormEvent) {
    event.preventDefault(); setBusy(true); setError(''); setResults(null);
    try {
      const response = await api<ApiEnvelope<Person[]>>('/api/v1/person-search', {
        method: 'POST', body: JSON.stringify({ registry_id: scope.registryId, organization_id: scope.organizationId, public_id_or_alias: id.trim(), limit: 20 }),
      });
      setResults(response.data);
    } catch (cause) { setError(apiError(cause, labels, 'registry.search.failed', 'Search failed.')); }
    finally { setBusy(false); }
  }
  return <section className="page">
    <div className="page-heading"><div><p className="eyebrow">{labels('registry.search.eyebrow', 'Identity registry')}</p><h1>{labels('registry.search.title')}</h1>
      <p className="lead">{t(labels, 'registry.search.help', 'Search by a liteCR public ID or known alias. This search does not check demographic details.')}</p></div>
      <Link className="button button-primary" to="/register">{labels('shell.nav.register')}</Link></div>
    <form className="panel search-form" aria-busy={busy || undefined} onSubmit={submit}>
      <label htmlFor="search-id">{labels('registry.search.idLabel')}</label>
      <div className="search-row"><input id="search-id" dir="ltr" autoComplete="off" value={id} onChange={(e) => { setId(e.target.value); setResults(null); setError(''); }} disabled={busy} required />
        <button className="button button-primary" disabled={busy || !scope.registryId || !scope.organizationId}>{busy ? labels('registry.search.searching', 'Searching…') : labels('registry.search.submit')}</button></div>
      <p className="help">{t(labels, 'registry.search.privacyHelp', 'Search terms are sent to liteCR and do not appear in the page address.')}</p>
    </form>
    <p className="visually-hidden" role="status" aria-live="polite">{busy ? labels('registry.search.searching', 'Searching…') : ''}</p>
    {!scope.registryId || !scope.organizationId ? <p className="notice">{labels('registry.search.scopeRequired', 'Set the registry and organization context in the header before searching.')}</p> : null}
    {error ? <div className="alert alert-error" role="alert">{error}</div> : null}
    {results && <div className="results" aria-live="polite"><h2>{results.length === 0 ? labels('registry.search.noResult') : `${results.length} ${labels(results.length === 1 ? 'registry.search.recordFound' : 'registry.search.recordsFound')}`}</h2>
      {results.length === 0 ? <p>{labels('registry.search.noExactMatch', 'No public ID or alias matched in this registry and organization. This search does not check demographic details.')}</p> :
      results.map((person) => <PersonCard key={person.public_id} person={person} labels={labels} />)}
    </div>}
  </section>;
}

export function RegisterScreen({ scope, labels, locale = 'en' }: Props & { locale?: string }) {
  const [form, setForm] = useState(blankRegistration); const [busy, setBusy] = useState(false);
  const [profileSuggestions, setProfileSuggestions] = useState<CountryProfileSuggestions | null>(null);
  const [profileState, setProfileState] = useState<'idle' | 'loading' | 'available' | 'unavailable'>('idle');
  const profileRequest = useRef(0);
  const [error, setError] = useState(''); const [outcome, setOutcome] = useState<{ publicId: string; sourceRecordId: string; type: string } | null>(null);
  const [copyStatus, setCopyStatus] = useState('');
  const [validationErrors, setValidationErrors] = useState<Array<{ id: string; label: string; message: string }>>([]);
  const errorSummaryRef = useRef<HTMLDivElement>(null);
  const pendingSummaryFocus = useRef(false);
  const navigate = useNavigate();
  useEffect(() => {
    const country = form.source_country.trim().toUpperCase();
    const request = ++profileRequest.current;
    setProfileSuggestions(null);
    if (!scope.registryId || !scope.organizationId || !/^[A-Z]{2}$/.test(country)) {
      setProfileState('idle');
      return;
    }
    let active = true;
    setProfileState('loading');
    const path = '/api/v1/config/' + encodeURIComponent(scope.registryId) + '/' + encodeURIComponent(scope.organizationId) +
      '/country-profiles/' + encodeURIComponent(country);
    // Country profiles are scoped by registry and organization. They only provide optional suggestions.
    void api<{ country_code: string; identifier_types: string[] } | null>(path).then((profile) => {
      if (!active || request !== profileRequest.current) return;
      if (profile && profile.country_code.toUpperCase() === country) {
        setProfileSuggestions({ country_code: country, identifier_types: profile.identifier_types });
        setProfileState(profile.identifier_types.length ? 'available' : 'idle');
      } else setProfileState('idle');
    }).catch(() => {
      if (active && request === profileRequest.current) setProfileState('unavailable');
    });
    return () => { active = false; };
  }, [form.source_country, scope.registryId, scope.organizationId]);
  useEffect(() => {
    if (!pendingSummaryFocus.current || !validationErrors.length) return;
    pendingSummaryFocus.current = false;
    errorSummaryRef.current?.focus();
  }, [validationErrors]);
  function update<K extends keyof Registration>(key: K, value: Registration[K]) {
    setForm((current) => ({ ...current, [key]: value }));
    const id = registrationFieldIds[key];
    setValidationErrors((current) => current.filter((item) => item.id !== id));
  }
  async function submit(event: FormEvent) {
    event.preventDefault(); setError(''); setOutcome(null); setCopyStatus('');
    const invalidFields = Array.from(event.currentTarget.querySelectorAll<HTMLInputElement>('input'))
      .filter((input) => !input.validity.valid);
    if (invalidFields.length) {
      pendingSummaryFocus.current = true;
      setValidationErrors(invalidFields.map((input) => ({
        id: input.id,
        label: input.labels?.[0]?.textContent?.replace(/\s*\*\s*$/, '').trim() || input.id,
        message: input.validity.valueMissing
          ? labels('registry.register.validation.required', 'Enter a value for this required field.')
          : input.id === registrationFieldIds.source_country
            ? labels('registry.register.validation.countryCode', 'Enter a two-letter country or region code, such as LA.')
            : labels('registry.register.validation.invalid', 'Check this value and try again.'),
      })));
      return;
    }
    setValidationErrors([]); setBusy(true);
    const assertions = [
      { field_path: 'name', original_value: form.name, language_tag: form.language_tag || undefined, script: form.script || undefined },
      ...(form.birth_date ? [{ field_path: 'birth_date', original_value: form.birth_date }] : []),
    ];
    const identifiers = form.identifier_value ? [{
      authority: form.identifier_authority, identifier_type: form.identifier_type, value: form.identifier_value, authoritative: false,
    }] : [];
    try {
      const response = await api<ApiEnvelope<{ outcome: string; source_record_id: string; person_public_id: string }>>('/api/v1/source-records', {
        method: 'POST', headers: { 'Idempotency-Key': idempotencyKey() },
        body: JSON.stringify({
          registry_id: scope.registryId, organization_id: scope.organizationId,
          source_system_id: form.source_system_id, source_record_key: form.source_record_key,
          source_version: form.source_version || undefined, assertions, identifiers,
        }),
      });
      setOutcome({ publicId: response.data.person_public_id, sourceRecordId: response.data.source_record_id, type: response.data.outcome }); setForm(blankRegistration);
    } catch (cause) { setError(apiError(cause, labels, 'registry.register.notConfirmed', 'Registration was not confirmed.')); }
    finally { setBusy(false); }
  }
  const sourceCountryError = validationErrors.find((item) => item.id === registrationFieldIds.source_country);
  return <section className="page">
    <div className="page-heading"><div><p className="eyebrow">{labels('registry.register.eyebrow', 'Source assertions')}</p><h1>{labels('registry.register.title')}</h1>
      <p className="lead">{t(labels, 'registry.register.help', 'liteCR keeps source values as entered and creates a provisional record linked to the source. It does not merge records.')}</p></div></div>
    {error && <div className="alert alert-error" role="alert" tabIndex={-1}>{error}</div>}
    {outcome && <div className="alert alert-success" role="status">{t(labels, 'registry.register.serverConfirmed', 'Server confirmed')}: {outcome.type.replaceAll('_', ' ')}. {t(labels, 'registry.register.personId', 'Person ID')} <code><bdi dir="ltr">{outcome.publicId}</bdi></code>.
      <p>{t(labels, 'registry.register.sourceRecordId', 'Source record ID')} <code><bdi dir="ltr">{outcome.sourceRecordId}</bdi></code>.
        <button className="button" type="button" onClick={async () => {
          try {
            await navigator.clipboard.writeText(outcome.sourceRecordId);
            setCopyStatus(t(labels, 'registry.register.sourceRecordIdCopied', 'Source record ID copied.'));
          } catch {
            setCopyStatus(t(labels, 'registry.register.sourceRecordIdCopyFailed', 'Copy is unavailable. Select and copy the source record ID above.'));
          }
        }}>{t(labels, 'registry.register.copySourceRecordId', 'Copy source record ID')}</button>
      </p>
      {copyStatus && <span>{copyStatus}</span>}
      {outcome.type === 'accepted' && <button className="text-button" type="button" onClick={() => navigate('/persons/' + encodeURIComponent(outcome.publicId))}>{t(labels, 'registry.person.open', 'Open record')}</button>}</div>}
    <form className="panel form-panel" noValidate onSubmit={submit}>
      {validationErrors.length > 0 && <div className="alert alert-error form-error-summary" role="alert" tabIndex={-1} ref={errorSummaryRef}>
        <h2>{labels('registry.register.validation.summary', 'Fix the following fields before continuing.')}</h2>
        <ul>{validationErrors.map((item) => <li key={item.id}><a href={'#' + item.id}>{item.label}: {item.message}</a></li>)}</ul>
      </div>}
      <fieldset disabled={busy || !scope.registryId || !scope.organizationId}><legend>{t(labels, 'registry.register.sourceContext', 'Source and record context')}</legend><div className="field-grid">
        <Field labels={labels} id={registrationFieldIds.source_system_id} keyName="registry.register.sourceSystemId" label="Source system ID" value={form.source_system_id} onChange={(v) => update('source_system_id', v)} error={validationErrors.find((item) => item.id === registrationFieldIds.source_system_id)?.message} required />
        <Field labels={labels} id={registrationFieldIds.source_record_key} keyName="registry.register.sourceRecordKey" label="Source record key" value={form.source_record_key} onChange={(v) => update('source_record_key', v)} error={validationErrors.find((item) => item.id === registrationFieldIds.source_record_key)?.message} required />
        <Field labels={labels} id={registrationFieldIds.source_version} keyName="registry.register.sourceVersion" label="Source version" value={form.source_version} onChange={(v) => update('source_version', v)} error={validationErrors.find((item) => item.id === registrationFieldIds.source_version)?.message} />
      </div></fieldset>
      <fieldset disabled={busy || !scope.registryId || !scope.organizationId}><legend>{t(labels, 'registry.register.originalValues', 'Values as provided')}</legend><div className="field-grid">
        <Field labels={labels} id={registrationFieldIds.name} keyName="registry.register.name" label="Name as provided by source" value={form.name} onChange={(v) => update('name', v)} error={validationErrors.find((item) => item.id === registrationFieldIds.name)?.message} required />
        <Field labels={labels} id={registrationFieldIds.language_tag} keyName="registry.register.languageTag" label="Language tag (optional)" value={form.language_tag} onChange={(v) => update('language_tag', v)} error={validationErrors.find((item) => item.id === registrationFieldIds.language_tag)?.message} placeholder={t(labels, 'registry.register.languageTagPlaceholder', 'e.g. lo, th, vi')} />
        <Field labels={labels} id={registrationFieldIds.script} keyName="registry.register.scriptLabel" label="Script label (optional)" value={form.script} onChange={(v) => update('script', v)} error={validationErrors.find((item) => item.id === registrationFieldIds.script)?.message} placeholder={t(labels, 'registry.register.scriptLabelPlaceholder', 'e.g. Lao, Thai, Latin')} />
        <Field labels={labels} id={registrationFieldIds.birth_date} keyName="registry.register.birthDate" label="Birth date as supplied (optional; partial values allowed)" value={form.birth_date} onChange={(v) => update('birth_date', v)} error={validationErrors.find((item) => item.id === registrationFieldIds.birth_date)?.message} />
      </div></fieldset>
      <fieldset disabled={busy || !scope.registryId || !scope.organizationId}><legend>{t(labels, 'registry.register.identifierOptional', 'Identifier (optional)')}</legend><div className="field-grid">
        <div className="field"><label htmlFor="source-registration-country">{t(labels, 'registry.register.sourceCountry', 'Country or region of source record (optional)')}</label>
          <input id="source-registration-country" list="source-registration-country-options" dir="ltr" value={form.source_country} onChange={(event) => update('source_country', event.target.value)} maxLength={2} pattern="[A-Za-z]{2}" autoComplete="off" aria-invalid={sourceCountryError ? true : undefined} aria-describedby={sourceCountryError ? 'source-registration-country-help source-registration-country-error' : 'source-registration-country-help'} />
          <datalist id="source-registration-country-options">{regionOptions(locale).map((region) => <option key={region.code} value={region.code} label={region.label} />)}</datalist>
          <p id="source-registration-country-help" className="help">{t(labels, 'registry.register.sourceCountryHelp', 'Choose or enter the source record country/region code. This only loads format suggestions; it does not infer nationality or change stored values.')}</p>
          {sourceCountryError && <p className="field-error" id="source-registration-country-error">{sourceCountryError.message}</p>}
          <p className="help" role="status" aria-live="polite">{profileState === 'loading' ? t(labels, 'registry.register.profileLoading', 'Loading optional identifier suggestions…') : profileState === 'available' ? t(labels, 'registry.register.profileAvailable', 'Published identifier types are available as suggestions below.') : profileState === 'unavailable' ? t(labels, 'registry.register.profileUnavailable', 'Country profile suggestions are unavailable. You can continue with manual entry.') : ''}</p>
        </div>
        <Field labels={labels} id={registrationFieldIds.identifier_authority} keyName="registry.register.assigningAuthority" label="Identifier issuer (assigning authority)" value={form.identifier_authority} onChange={(v) => update('identifier_authority', v)} error={validationErrors.find((item) => item.id === registrationFieldIds.identifier_authority)?.message} />
        <div className="field"><label htmlFor={registrationFieldIds.identifier_type}>{t(labels, 'registry.register.identifierType', 'Identifier type')}</label>
          <input id={registrationFieldIds.identifier_type} list="registration-identifier-type-options" value={form.identifier_type} onChange={(event) => update('identifier_type', event.target.value)} autoComplete="off" aria-invalid={validationErrors.some((item) => item.id === registrationFieldIds.identifier_type) ? true : undefined} aria-describedby={validationErrors.some((item) => item.id === registrationFieldIds.identifier_type) ? registrationFieldIds.identifier_type + '-error' : undefined} />
          <datalist id="registration-identifier-type-options">{(profileSuggestions?.identifier_types ?? []).map((identifierType) => <option key={identifierType} value={identifierType} />)}</datalist>
          {validationErrors.find((item) => item.id === registrationFieldIds.identifier_type) && <p className="field-error" id={registrationFieldIds.identifier_type + '-error'}>{validationErrors.find((item) => item.id === registrationFieldIds.identifier_type)?.message}</p>}
        </div>
        <Field labels={labels} id={registrationFieldIds.identifier_value} keyName="registry.register.identifierValue" label="Identifier value" value={form.identifier_value} onChange={(v) => update('identifier_value', v)} error={validationErrors.find((item) => item.id === registrationFieldIds.identifier_value)?.message} />
      </div></fieldset>
      {!scope.registryId || !scope.organizationId ? <p className="alert alert-warning">{t(labels, 'registry.register.scopeRequired', 'Set the registry and organization context in the header first.')}</p> : null}
      <div className="form-actions"><button className="button button-primary" disabled={busy || !scope.registryId || !scope.organizationId}>{busy ? labels('registry.register.submitting', 'Submitting…') : labels('registry.register.submit')}</button></div>
    </form>
  </section>;
}

export function PersonDetailScreen({ scope, labels }: Props) {
  const { publicId = '' } = useParams(); const [person, setPerson] = useState<Person | null>(null);
  const [error, setError] = useState(''); const [busy, setBusy] = useState(true);
  useEffect(() => {
    let active = true; setPerson(null); setError(''); setBusy(true);
    const path = '/api/v1/registries/' + encodeURIComponent(scope.registryId) + '/organizations/' +
      encodeURIComponent(scope.organizationId) + '/persons/' + encodeURIComponent(publicId);
    void api<ApiEnvelope<Person>>(path).then((value) => { if (active) setPerson(value.data); })
      .catch((cause) => { if (active) setError(apiError(cause, labels, 'registry.person.loadFailed', 'Could not load record.')); })
      .finally(() => { if (active) setBusy(false); });
    return () => { active = false; };
  }, [scope.registryId, scope.organizationId, publicId]);
  if (busy) return <section className="page" aria-busy="true"><h1>{labels('registry.person.title')}</h1><p role="status">{t(labels, 'registry.person.loading', 'Loading record and source history…')}</p></section>;
  if (error) return <section className="page"><h1>{labels('registry.person.title')}</h1><div role="alert" className="alert alert-error">{error}</div><Link to="/">{t(labels, 'registry.person.backToSearch', 'Back to search')}</Link></section>;
  if (!person) return <section className="page"><h1>{labels('registry.person.title')}</h1><p>{t(labels, 'registry.person.unavailable', 'Record is unavailable.')}</p><Link to="/">{t(labels, 'registry.person.backToSearch', 'Back to search')}</Link></section>;
  const names = person.assertions.filter((item) => item.field_path === 'name');
  return <section className="page">
      <div className="page-heading"><div><p className="eyebrow">{labels('registry.person.title')} · {labels('registry.person.version', 'Version')} {person.resource_version}</p><h1><code><bdi dir="ltr">{person.public_id}</bdi></code></h1>
      <p className="lead">{names.length ? <bdi dir="auto">{names[0].original_value}</bdi> : t(labels, 'registry.person.noName', 'No name assertion available')}</p></div><Link className="button" to="/">{t(labels, 'registry.person.backToSearch', 'Back to search')}</Link></div>
    {person.aliases.length > 0 && <section className="panel"><h2>{t(labels, 'registry.person.aliases', 'Known aliases')}</h2><ul>{person.aliases.map((alias) => <li key={alias}><code><bdi dir="ltr">{alias}</bdi></code></li>)}</ul></section>}
    <section className="panel"><h2>{labels('registry.person.history')}</h2><p className="help">{t(labels, 'registry.person.historyHelp', 'Values below are original source assertions. No normalized or translated value is substituted here.')}</p>
      {person.assertions.length === 0 ? <p>{t(labels, 'registry.person.noAssertions', 'No source assertions are available.')}</p> : <div className="history-list">
        {person.assertions.map((assertion, index) => <article className="history-item" key={assertion.source_record_id + '-' + assertion.field_path + '-' + index}>
          <div className="history-meta"><span>{assertion.field_path.replaceAll('_', ' ')}</span><span>{t(labels, 'registry.person.source', 'Source')} {assertion.source_system_id}</span>
            <span>{t(labels, 'registry.person.version', 'Version')} {assertion.source_version ?? t(labels, 'common.notSupplied', 'not supplied')} · {t(labels, 'registry.person.receive', 'receive')} {assertion.receive_version}</span></div>
          <p className="original-value"><bdi dir="auto">{assertion.original_value}</bdi></p>
          <p className="help">{t(labels, 'registry.person.language', 'Language')}: {assertion.language_tag || t(labels, 'common.notSupplied', 'not supplied')} · {t(labels, 'registry.person.script', 'Script')}: {assertion.script || t(labels, 'common.notSupplied', 'not supplied')}</p>
        </article>)}</div>}
    </section>
  </section>;
}

function PersonCard({ person, labels }: { person: Person; labels: Props['labels'] }) {
  const name = person.assertions.find((assertion) => assertion.field_path === 'name')?.original_value;
  return <article className="person-card"><div><h3>{name ? <bdi dir="auto">{name}</bdi> : t(labels, 'registry.person.nameNotProvided', 'Name not provided')}</h3><code><bdi dir="ltr">{person.public_id}</bdi></code></div>
    <p>{person.source_record_ids.length} {t(labels, person.source_record_ids.length === 1 ? 'registry.search.sourceRecord' : 'registry.search.sourceRecords', person.source_record_ids.length === 1 ? 'source record' : 'source records')}</p>
    <Link className="button" to={'/persons/' + encodeURIComponent(person.public_id)}>{t(labels, 'registry.person.viewHistory', 'View source history')}</Link></article>;
}

const registrationFieldIds: Record<keyof Registration, string> = {
  source_system_id: 'source-system-id', source_record_key: 'source-record-key', source_version: 'source-version',
  name: 'source-name', language_tag: 'source-language-tag', script: 'source-script', birth_date: 'source-birth-date',
  identifier_authority: 'identifier-authority', identifier_type: 'identifier-type', identifier_value: 'identifier-value', source_country: 'source-registration-country',
};

function Field({ labels, id, keyName, label, value, onChange, required, placeholder, error }: {
  labels: Props['labels']; id: string; keyName: string; label: string; value: string; onChange: (value: string) => void; required?: boolean; placeholder?: string; error?: string;
}) {
  return <div className="field"><label htmlFor={id}>{t(labels, keyName, label)}{required && <span aria-hidden="true"> *</span>}</label>
    <input id={id} value={value} onChange={(event) => onChange(event.target.value)} required={required} placeholder={placeholder} autoComplete="off" aria-invalid={error ? true : undefined} aria-describedby={error ? id + '-error' : undefined} />
    {error && <p className="field-error" id={id + '-error'}>{error}</p>}</div>;
}

function t(labels: Props['labels'], key: string, fallback: string) { return labels(key, fallback); }
function apiError(cause: unknown, labels: Props['labels'], fallbackKey: string, fallback: string) {
  if (cause instanceof ApiFailure) {
    const translated = apiFailureCopy(cause, labels);
    if (translated) return translated;
    if (cause.status === 0) return t(labels, 'common.error.network', 'The service could not be reached. Check the connection and retry.');
    if (cause.status === 401) return t(labels, 'common.error.unauthenticated', 'Your session has expired. Sign in again to continue.');
    if (cause.status === 403 || cause.status === 404) return t(labels, 'common.error.scope', 'This action is not available in the current access scope.');
    if (cause.status === 409) return t(labels, 'common.error.conflict', 'The record changed or conflicts with another request. Refresh and review before retrying.');
    if (cause.status >= 500) return t(labels, 'common.error.unavailable', 'The service is temporarily unavailable. Your changes were not confirmed.');
    return cause.message;
  }
  return cause instanceof Error ? cause.message : t(labels, fallbackKey, fallback);
}
