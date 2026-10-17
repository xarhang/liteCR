import { useEffect, useRef, useState } from 'react';
import { api, ApiFailure } from '../../shell/api';
import { apiFailureCopy } from '../../shell/apiFailureCopy';
import { canonicalLocaleTag } from '../localization/validation';
import { buildCoverageInventory, type ProfileCoverage } from './coverageInventory';
import '../localization/admin.css';

type Props = { registryId: string; organizationId: string; labels: (key: string, sourceFallback?: string, values?: Record<string, string | number>) => string };
type Profile = { country_code: string; default_locale: string; calendar: string; timezone: string; address_fields: string[]; identifier_types: string[]; phone_region: string; version: number };
const segment = (value: string) => encodeURIComponent(value.trim());
const blankProfile = (country_code: string): Profile => ({
  country_code,
  default_locale: '',
  calendar: '',
  timezone: '',
  address_fields: [],
  identifier_types: [],
  phone_region: '',
  version: 0,
});

export function CountryProfileAdmin({ registryId, organizationId, labels }: Props) {
  const root = `/api/v1/config/${segment(registryId)}/${segment(organizationId)}/country-profiles`;
  const [code, setCode] = useState('LA');
  const codeRef = useRef('LA');
  const requestSequence = useRef(0);
  const [profile, setProfile] = useState<Profile>(blankProfile('LA'));
  const [active, setActive] = useState<Profile | null>(null);
  const [loadedCountry, setLoadedCountry] = useState<string | null>(null);
  const loadedCountryRef = useRef<string | null>(null);
  const [draftVersion, setDraftVersion] = useState<number | null>(null);
  const [rollbackVersion, setRollbackVersion] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [status, setStatus] = useState('');
  const [countryCodeTouched, setCountryCodeTouched] = useState(false);
  const [coverage, setCoverage] = useState<ProfileCoverage[] | null>(null);
  const [coverageError, setCoverageError] = useState(false);
  const [coverageQuery, setCoverageQuery] = useState('');
  async function refreshCoverage() {
    if (!registryId || !organizationId) return;
    try {
      setCoverage(await api<ProfileCoverage[]>(root));
      setCoverageError(false);
    } catch {
      setCoverageError(true);
    }
  }
  async function load() {
    if (!registryId || !organizationId) return;
    const country_code = codeRef.current.trim().toUpperCase();
    if (!/^[A-Z]{2}$/.test(country_code)) {
      setCountryCodeTouched(true);
      return;
    }
    const sequence = ++requestSequence.current;
    loadedCountryRef.current = null;
    setLoadedCountry(null);
    setActive(null);
    setDraftVersion(null);
    setProfile(blankProfile(country_code));
    setBusy(true); setError(''); setStatus('');
    try {
      const value = await api<Profile | null>(`${root}/${segment(country_code)}`);
      if (sequence !== requestSequence.current || country_code !== codeRef.current) return;
      if (value && value.country_code.trim().toUpperCase() !== country_code) {
        setError(t(labels, 'error.requestFailed', 'The service returned a profile for a different country. Retry before editing.'));
        return;
      }
      setActive(value);
      setProfile(value ?? blankProfile(country_code));
      loadedCountryRef.current = country_code;
      setLoadedCountry(country_code);
      setStatus(value ? labels('countryProfiles.status.loaded', 'Published profile v{{version}} loaded.', { version: value.version }) : t(labels, 'status.notFound', 'No published country profile exists for this code.'));
    }
    catch (e) {
      if (sequence === requestSequence.current && country_code === codeRef.current) setError(msg(e, labels));
    } finally {
      if (sequence === requestSequence.current) setBusy(false);
    }
  }
  useEffect(() => { if (registryId && organizationId) { void load(); void refreshCoverage(); } }, [root, registryId, organizationId]);
  const isLoaded = loadedCountry === code.trim().toUpperCase() && loadedCountryRef.current === code.trim().toUpperCase();
  function update<K extends keyof Profile>(field: K, value: Profile[K]) {
    const country_code = codeRef.current.trim().toUpperCase();
    if (loadedCountryRef.current !== country_code) return;
    setProfile((current) => current.country_code === country_code ? { ...current, [field]: value } : current);
    setDraftVersion(null);
  }
  async function saveDraft() {
    const country_code = codeRef.current.trim().toUpperCase();
    if (!/^[A-Z]{2}$/.test(country_code)) { setCountryCodeTouched(true); return; }
    if (loadedCountryRef.current !== country_code || profile.country_code !== country_code) return;
    const default_locale = canonicalLocaleTag(profile.default_locale);
    if (!default_locale) { setError(t(labels, 'error.invalidLocaleTag', 'Enter a valid BCP 47 locale tag no longer than 64 characters.')); return; }
    if (!profile.timezone.trim()) { setError(labels('registry.register.validation.required', 'Enter an IANA time zone before saving this country profile.')); return; }
    const sequence = ++requestSequence.current;
    setBusy(true); setError(''); setStatus('');
    const payload = { ...profile, country_code, default_locale, phone_region: profile.phone_region.toUpperCase(), address_fields: profile.address_fields, identifier_types: profile.identifier_types, expected_version: active?.version ?? null };
    try {
      await api<void>(`${root}/${segment(country_code)}`, { method: 'PUT', body: JSON.stringify(payload) });
      if (sequence !== requestSequence.current || codeRef.current !== country_code || loadedCountryRef.current !== country_code) return;
      void refreshCoverage();
      setProfile((current) => ({ ...current, default_locale })); setDraftVersion((active?.version ?? 0) + 1); setStatus(t(labels, 'status.draftSaved', 'Country profile draft saved. Country defaults do not change language, theme, permissions, or matching policy.'));
    }
    catch (e) { if (sequence === requestSequence.current && codeRef.current === country_code && loadedCountryRef.current === country_code) setError(msg(e, labels)); }
    finally { if (sequence === requestSequence.current) setBusy(false); }
  }
  async function publish() {
    const country_code = codeRef.current.trim().toUpperCase();
    if (loadedCountryRef.current !== country_code || profile.country_code !== country_code) return;
    if (!draftVersion) { setError(t(labels, 'error.saveBeforePublish', 'Save the country profile draft before publishing.')); return; }
    const sequence = ++requestSequence.current;
    setBusy(true); setError(''); setStatus('');
    try {
      const value = await api<Profile>(`${root}/${segment(country_code)}/publish`, { method: 'POST', body: JSON.stringify({ expected_version: draftVersion }) });
      if (sequence !== requestSequence.current || codeRef.current !== country_code || loadedCountryRef.current !== country_code) return;
      void refreshCoverage();
      setActive(value); setProfile(value); setDraftVersion(null); setStatus(labels('countryProfiles.status.published', 'Country profile {{country}} published at version {{version}}.', { country: country_code, version: value.version }));
    }
    catch (e) { if (sequence === requestSequence.current && codeRef.current === country_code && loadedCountryRef.current === country_code) setError(msg(e, labels)); }
    finally { if (sequence === requestSequence.current) setBusy(false); }
  }
  async function rollback() {
    const country_code = codeRef.current.trim().toUpperCase();
    if (loadedCountryRef.current !== country_code || profile.country_code !== country_code) return;
    const target_version = Number(rollbackVersion);
    if (!active || !Number.isSafeInteger(target_version) || target_version < 1 || target_version === active.version) {
      setError(t(labels, 'error.chooseRestoreVersion', 'Choose a previously published country profile version to restore.'));
      return;
    }
    const sequence = ++requestSequence.current;
    setBusy(true); setError(''); setStatus('');
    try {
      const value = await api<Profile>(`${root}/${segment(country_code)}/rollback`, {
        method: 'POST',
        body: JSON.stringify({ target_version, expected_version: active.version }),
      });
      if (sequence !== requestSequence.current || codeRef.current !== country_code || loadedCountryRef.current !== country_code) return;
      void refreshCoverage();
      setActive(value); setProfile(value); setDraftVersion(null); setRollbackVersion('');
      setStatus(labels('countryProfiles.status.restored', 'Country profile {{country}} restored as new version {{version}}.', { country: country_code, version: value.version }));
    } catch (e) { if (sequence === requestSequence.current && codeRef.current === country_code && loadedCountryRef.current === country_code) setError(msg(e, labels)); }
    finally { if (sequence === requestSequence.current) setBusy(false); }
  }
  const list = (value: string) => value.split(',').map((item) => item.trim()).filter(Boolean);
  const inventory = buildCoverageInventory(coverage ?? [], document.documentElement.lang || 'en');
  const visibleInventory = inventory.filter((row) => `${row.label} ${row.code}`.toLocaleLowerCase().includes(coverageQuery.trim().toLocaleLowerCase()));
  const invalidCountryCode = countryCodeTouched && !/^[A-Z]{2}$/.test(code.trim().toUpperCase());
  function selectProfile(countryCode: string) {
    codeRef.current = countryCode;
    setCode(countryCode);
    setProfile(blankProfile(countryCode));
    setActive(null);
    setDraftVersion(null);
    setRollbackVersion('');
    setError('');
    void load();
  }
  return <section className="admin-page" aria-labelledby="country-profile-title">
    <header className="admin-heading"><div><p className="eyebrow">{t(labels, 'eyebrow', 'Country defaults')}</p><h1 id="country-profile-title">{labels('countryProfiles.title')}</h1><p className="lead">{t(labels, 'intro', 'Set the default address fields, identifier labels, locale, calendar and time zone for this country profile.')}</p></div><span className="admin-badge">{t(labels, 'independentSettings', 'Independent settings')}</span></header>
    <div className="admin-grid" aria-busy={busy}>
      <section className="admin-card"><h2>{t(labels, 'profileScope', 'Profile scope')}</h2><label>{t(labels, 'countryCode', 'Country / region code')}<input list="country-profile-region-codes" value={code} onChange={(e) => { const next = e.target.value.toUpperCase(); codeRef.current = next; requestSequence.current += 1; loadedCountryRef.current = null; setBusy(false); setCode(next); setLoadedCountry(null); setProfile(blankProfile(next)); setActive(null); setDraftVersion(null); setRollbackVersion(''); setError(''); setStatus(''); }} onBlur={() => { if (code.trim() && !/^[A-Z]{2}$/.test(code.trim().toUpperCase())) setCountryCodeTouched(true); }} pattern="[A-Z]{2}" maxLength={2} required aria-invalid={invalidCountryCode ? true : undefined} aria-describedby={invalidCountryCode ? 'country-code-help country-code-error' : 'country-code-help'} /></label><datalist id="country-profile-region-codes">{inventory.filter((row) => !row.custom).map(({ code: regionCode, label }) => <option key={regionCode} value={regionCode} label={label} />)}</datalist><p id="country-code-help" className="help">{t(labels, 'countryCodeHelp', 'Search by a country or region name, or enter its two-letter code. Suggestions follow the interface language; custom codes remain available.')}</p>{invalidCountryCode && <p id="country-code-error" className="alert alert-error">{t(labels, 'error.invalidCountryCode', 'Enter a two-letter uppercase country code.')}</p>}<button className="button" disabled={busy || !/^[A-Z]{2}$/.test(code)} onClick={() => void load()}>{labels('countryProfiles.load')}</button><p className="admin-state">{t(labels, 'publishedVersion', 'Published version')}: {active?.version ?? t(labels, 'none', 'none')}</p></section>
      <section className="admin-card"><h2>{t(labels, 'regionalFormats', 'Regional data formats')}</h2><div className="admin-form"><label>{t(labels, 'defaultLocale', 'Default locale tag')}<input disabled={!isLoaded || busy} value={profile.default_locale} onChange={(e) => update('default_locale', e.target.value)} maxLength={64} required /></label><label>{t(labels, 'calendarIdentifier', 'Calendar identifier')}<input disabled={!isLoaded || busy} value={profile.calendar} onChange={(e) => update('calendar', e.target.value)} pattern="[A-Za-z0-9_-]{1,32}" maxLength={32} required /></label><label>{t(labels, 'ianaTimeZone', 'IANA time zone')}<input disabled={!isLoaded || busy} value={profile.timezone} onChange={(e) => update('timezone', e.target.value)} pattern="[A-Za-z0-9_+/-]{1,64}" maxLength={64} placeholder={t(labels, 'timeZoneExample', 'Asia/Vientiane')} required /></label><label>{t(labels, 'phoneRegion', 'Phone region')}<input disabled={!isLoaded || busy} value={profile.phone_region} onChange={(e) => update('phone_region', e.target.value.toUpperCase())} pattern="[A-Z]{2}" maxLength={2} required /></label></div></section>
      <section className="admin-card"><h2>{t(labels, 'fieldConventions', 'Field conventions')}</h2><div className="admin-form"><label>{t(labels, 'addressFields', 'Address fields')} <span className="help">{t(labels, 'commaSeparated24', '(comma-separated, up to 24)')}</span><textarea disabled={!isLoaded || busy} rows={3} value={profile.address_fields.join(', ')} onChange={(e) => update('address_fields', list(e.target.value))} maxLength={1200} /></label><label>{t(labels, 'identifierTypeLabels', 'Identifier type labels')} <span className="help">{t(labels, 'commaSeparated32', '(comma-separated, up to 32)')}</span><textarea disabled={!isLoaded || busy} rows={3} value={profile.identifier_types.join(', ')} onChange={(e) => update('identifier_types', list(e.target.value))} maxLength={1800} /></label></div><p className="admin-warning">{t(labels, 'formatsOnlyWarning', 'These settings describe formats only. They cannot set matching rules or alter identity resolution.')}</p></section>
      <section className="admin-card"><h2>{t(labels, 'draftAndPublish', 'Draft and publish')}</h2><p>{t(labels, 'unsavedEdits', 'Unsaved edits')}: <strong>{draftVersion ? labels('countryProfiles.draftVersion', 'Draft v{{version}}', { version: draftVersion }) : t(labels, 'none', 'none')}</strong></p><div className="button-row"><button className="button" disabled={!isLoaded || busy || !profile.default_locale.trim() || !profile.timezone.trim() || list(profile.address_fields.join(',')).length > 24 || list(profile.identifier_types.join(',')).length > 32} onClick={() => void saveDraft()}>{labels('countryProfiles.saveDraft')}</button><button className="button button-primary" disabled={!isLoaded || busy || !draftVersion} onClick={() => void publish()}>{labels('countryProfiles.publish')}</button></div><p className="help">{t(labels, 'publishPermissionHelp', 'Publishing requires the country-profile management permission. API failures and version conflicts are shown as failures.')}</p><label>{t(labels, 'restoreVersionLabel', 'Previously published version to restore')}<input disabled={!isLoaded || busy || !active} inputMode="numeric" pattern="[0-9]+" value={rollbackVersion} onChange={(e) => setRollbackVersion(e.target.value)} aria-describedby="country-profile-rollback-help" /></label><p id="country-profile-rollback-help" className="help">{labels('countryProfiles.restoreHelp', 'Restore creates a new immutable release from that version. It never edits earlier versions. The active version is {{version}}.', { version: active?.version ?? t(labels, 'notLoaded', 'not loaded') })}</p><button className="button" disabled={!isLoaded || busy || !active || !rollbackVersion || Number(rollbackVersion) === active.version} onClick={() => void rollback()}>{labels('countryProfiles.restore')}</button></section>
      <section className="admin-card country-profile-coverage" aria-labelledby="country-profile-coverage-title">
        <h2 id="country-profile-coverage-title">{t(labels, 'coverage.title', 'Country and region coverage')}</h2>
        <p className="help">{t(labels, 'coverage.help', 'The list includes standard country and region codes, plus custom codes with saved profiles. “Unconfigured” means no profile is saved. This list does not set local rules.')}</p>
        {coverageError && <p className="alert alert-error" role="alert">{t(labels, 'coverage.error', 'Could not load profile coverage. Retry by reloading the page.')}</p>}
          {coverage === null && !coverageError && <p aria-hidden="true">{t(labels, 'coverage.loading', 'Loading profile coverage…')}</p>}
        {coverage !== null && <>
          <label>{t(labels, 'coverage.search', 'Filter regions')}<input value={coverageQuery} onChange={(event) => setCoverageQuery(event.target.value)} /></label>
          <p className="admin-state">{labels('countryProfiles.coverage.count', '{{standard}} standard regions · {{custom}} configured custom codes', { standard: inventory.filter((row) => !row.custom).length, custom: inventory.filter((row) => row.custom).length })}</p>
          <div className="country-profile-coverage-list" role="list" aria-label={t(labels, 'coverage.listLabel', 'Country and region profile status')}>
            {visibleInventory.map((row) => <div className="country-profile-coverage-row" role="listitem" key={row.code}>
              <span><strong>{row.label}</strong>{row.custom && <small>{t(labels, 'coverage.custom', 'Custom code')}</small>}</span>
              <span className={`coverage-status coverage-${row.status}`}>{t(labels, `coverage.status.${row.status}`, row.status)}{row.pendingDraft && row.activeVersion !== null && <small> · {t(labels, 'coverage.pendingDraft', 'Draft pending')}</small>}</span>
              <span className="coverage-version">{row.activeVersion ? `v${row.activeVersion}` : row.latestVersion ? t(labels, 'coverage.draftOnly', 'Draft only') : '—'}</span>
              <button type="button" className="button button-subtle" disabled={busy} onClick={() => selectProfile(row.code)}>{t(labels, 'coverage.edit', 'Select to edit')}</button>
            </div>)}
          </div>
          {visibleInventory.length === 0 && <p className="admin-state" role="status">{t(labels, 'coverage.noResults', 'No matching regions.')}</p>}
        </>}
      </section>
    </div>{coverage === null && !coverageError && <p className="visually-hidden" role="status">{t(labels, 'coverage.loading', 'Loading profile coverage…')}</p>}{error && <p className="alert alert-error" role="alert">{error}</p>}{(busy || status) && <p className="inline-status" role="status" aria-live="polite">{busy ? labels('review.waitingForServer', 'Waiting for server…') : status}</p>}
  </section>;
}
function t(labels: Props['labels'], key: string, fallback: string) { return labels('countryProfiles.' + key, fallback); }
function msg(e: unknown, labels: Props['labels']) {
  if (e instanceof ApiFailure) {
    const translated = apiFailureCopy(e, labels);
    if (translated) return translated;
    if (e.status === 0) return t(labels, 'error.network', 'The service could not be reached. Check the connection and retry.');
    if (e.status === 401) return t(labels, 'error.unauthenticated', 'Your session has expired. Sign in again to continue.');
    if (e.status === 403 || e.status === 404) return t(labels, 'error.scope', 'This action is not available in the current access scope.');
    if (e.status === 409) return t(labels, 'error.conflict', 'The profile changed or conflicts with another request. Refresh and review before retrying.');
    if (e.status >= 500) return t(labels, 'error.unavailable', 'The service is temporarily unavailable. Your changes were not confirmed.');
    return e.message;
  }
  return e instanceof Error ? e.message : t(labels, 'error.requestFailed', 'The country profile request could not be completed.');
}
