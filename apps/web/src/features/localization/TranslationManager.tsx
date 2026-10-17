import { useEffect, useMemo, useRef, useState, type FormEvent } from 'react';
import { api, ApiFailure } from '../../shell/api';
import { apiFailureCopy } from '../../shell/apiFailureCopy';
import { canonicalLocaleTag } from './validation';
import { placeholdersIn, placeholdersMatch } from './validation';
import { hasBuiltInDraftCatalog, loadBuiltInDraftCatalog } from './builtinDraftCatalogs';
import { englishUiMessages, UI_NAMESPACE } from '../../../../../packages/localization/src/webCatalogPreview';
import { directionFor } from '../../shell/preferences';
import './admin.css';

type Props = { registryId: string; organizationId: string; labels: (key: string, sourceFallback?: string, values?: Record<string, string | number>) => string };
type Locale = { tag: string; direction: string; enabled: boolean };
type Draft = { locale: string; namespace: string; key: string; value: string; plural_forms: Record<string, string> | null; source_revision: number; state: 'draft' | 'in_review' | 'approved'; stale: boolean };
type Bundle = { locale: string; namespace: string; revision: number; entries: Record<string, string>; fallback_locale: string };
type Entry = { key: string; source_text: string; source_revision: number; placeholders: string[]; plural: boolean; draft: Draft | null };
type EntryWorkflowState = 'missing' | 'stale' | 'draft' | 'inReview' | 'approved' | 'published';
const segment = (value: string) => encodeURIComponent(value.trim());
const placeholderTokens = (text: string) => [...text.matchAll(/\{([A-Za-z0-9_.-]+)\}/g)].map((match) => match[1]).sort();

export function TranslationManager({ registryId, organizationId, labels }: Props) {
  const root = `/api/v1/config/${segment(registryId)}/${segment(organizationId)}`;
  const [locales, setLocales] = useState<Locale[]>([]);
  const [locale, setLocale] = useState('lo');
  const [newLocale, setNewLocale] = useState('');
  const [direction, setDirection] = useState<'ltr' | 'rtl'>('ltr');
  const [namespace, setNamespace] = useState(UI_NAMESPACE);
  const [key, setKey] = useState('');
  const [catalogFilter, setCatalogFilter] = useState('');
  const [sourceText, setSourceText] = useState('');
  const [sourceRevision, setSourceRevision] = useState(1);
  const [placeholderText, setPlaceholderText] = useState('');
  const [plural, setPlural] = useState(false);
  const [pluralFormsText, setPluralFormsText] = useState('{\n  "other": ""\n}');
  const [value, setValue] = useState('');
  const [draft, setDraft] = useState<Draft | null>(null);
  const [published, setPublished] = useState<Bundle | null>(null);
  const [entries, setEntries] = useState<Entry[]>([]);
  const [entriesScope, setEntriesScope] = useState<string | null>(null);
  const [loadedSelection, setLoadedSelection] = useState<string | null>(null);
  const [rollbackRevision, setRollbackRevision] = useState('');
  const [busy, setBusy] = useState(false);
  const [publishArmed, setPublishArmed] = useState(false);
  const [error, setError] = useState('');
  const [status, setStatus] = useState('');
  const publishTriggerRef = useRef<HTMLButtonElement>(null);
  const publishConfirmRef = useRef<HTMLButtonElement>(null);
  const focusPublishConfirmation = useRef(false);
  const returnFocusToPublish = useRef(false);

  useEffect(() => { if (!registryId || !organizationId) return; void api<Locale[]>(root + '/locales').then((data) => { setLocales(data); if (data[0]) { setLocale(data[0].tag); setDirection(data[0].direction === 'rtl' ? 'rtl' : 'ltr'); } }).catch((e: unknown) => setError(message(e, labels))); }, [root, registryId, organizationId]);
  useEffect(() => { setPublishArmed(false); }, [locale, namespace]);
  useEffect(() => {
    if (publishArmed && focusPublishConfirmation.current) {
      focusPublishConfirmation.current = false;
      publishConfirmRef.current?.focus();
    } else if (!publishArmed && returnFocusToPublish.current) {
      returnFocusToPublish.current = false;
      publishTriggerRef.current?.focus();
    }
  }, [publishArmed]);
  const placeholders = useMemo(() => new Set(placeholderText.split(',').map((item) => item.trim()).filter(Boolean)), [placeholderText]);
  const selectionIdentity = JSON.stringify([root, locale, namespace.trim(), key.trim()]);
  const entriesScopeIdentity = JSON.stringify([root, locale, namespace.trim()]);
  const entriesMatchScope = entriesScope === entriesScopeIdentity;
  const selectionLoaded = loadedSelection === selectionIdentity;
  const sourceMissingPlaceholders = useMemo(() => {
    const found = placeholdersIn(sourceText);
    return [...found].filter((item) => !placeholders.has(item));
  }, [sourceText, placeholders]);
  const draftPlaceholdersMatch = useMemo(() => placeholdersMatch(sourceText, value), [sourceText, value]);
  const pluralForms = useMemo(() => {
    if (!plural) return null;
    try {
      const parsed: unknown = JSON.parse(pluralFormsText);
      if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) return null;
      const pairs = Object.entries(parsed);
      if (!pairs.length || pairs.some(([form, text]) => !/^[a-z]+$/.test(form) || typeof text !== 'string' || !text.trim())) return null;
      return Object.fromEntries(pairs) as Record<string, string>;
    } catch { return null; }
  }, [plural, pluralFormsText]);
  const pluralPlaceholdersMatch = pluralForms ? Object.values(pluralForms).every((text) => placeholdersMatch(sourceText, text)) : false;
  const draftPath = `${root}/locales/${segment(locale)}/${segment(namespace)}/${segment(key)}`;

  async function loadKey() {
    if (!key.trim() || !namespace.trim()) return;
    setPublishArmed(false);
    setLoadedSelection(null);
    setBusy(true); setError(''); setStatus('');
    try {
      const [existing, bundle, catalog] = await Promise.all([
        api<Draft | null>(draftPath),
        api<Bundle | null>(`/api/v1/bundles/${segment(registryId)}/${segment(organizationId)}/${segment(locale)}/${segment(namespace)}`),
        api<Entry[]>(`${root}/locales/${segment(locale)}/${segment(namespace)}/entries`),
      ]);
      const source = catalog.find((item) => item.key === key);
      const sourceTextFallback = source?.source_text ?? englishUiMessages[key] ?? '';
      setSourceText(sourceTextFallback);
      setSourceRevision(source?.source_revision ?? 1);
      setPlaceholderText(source?.placeholders.join(', ') ?? [...placeholdersIn(sourceTextFallback)].join(', '));
      setPlural(source?.plural ?? false);
      setDraft(existing); setValue(existing?.value ?? ''); setPluralFormsText(JSON.stringify(existing?.plural_forms ?? { other: existing?.value ?? '' }, null, 2)); setPublished(bundle);
      setEntries(catalog); setCatalogFilter('');
      setEntriesScope(entriesScopeIdentity);
      setLoadedSelection(selectionIdentity);
      setStatus(existing ? t(labels, 'status.draftLoaded', 'Draft loaded from the translation service.') : t(labels, 'status.noDraft', 'No draft exists for this key. This key appears missing until a draft is saved.'));
    } catch (e) { setError(message(e, labels)); }
    finally { setBusy(false); }
  }
  async function loadEntries() {
    setPublishArmed(false);
    setBusy(true); setError(''); setStatus('');
    try {
      const [data, bundle] = await Promise.all([
        api<Entry[]>(`${root}/locales/${segment(locale)}/${segment(namespace)}/entries`),
        api<Bundle | null>(`/api/v1/bundles/${segment(registryId)}/${segment(organizationId)}/${segment(locale)}/${segment(namespace)}`),
      ]);
      setEntries(data); setCatalogFilter(''); setEntriesScope(entriesScopeIdentity); setPublished(bundle);
      setStatus(data.length ? labels('translation.status.keysLoaded', 'Loaded {{count}} catalog keys. Missing and stale states come from the server.', { count: data.length }) : t(labels, 'status.noKeys', 'No source keys are registered in this namespace.'));
    } catch (e) { setError(message(e, labels)); }
    finally { setBusy(false); }
  }
  async function registerMissingUiSources() {
    if (namespace !== UI_NAMESPACE) return;
    setBusy(true); setError(''); setStatus('');
    try {
      const current = await api<Entry[]>(`${root}/locales/${segment(locale)}/${segment(UI_NAMESPACE)}/entries`);
      const existing = new Set(current.map((entry) => entry.key));
      const missing = Object.entries(englishUiMessages).filter(([messageKey]) => !existing.has(messageKey));
      let next = 0;
      const workers = Array.from({ length: Math.min(6, missing.length) }, async () => {
        while (next < missing.length) {
          const [messageKey, text] = missing[next++];
          await api<void>(`${root}/sources/${segment(UI_NAMESPACE)}/${segment(messageKey)}`, {
            method: 'PUT',
            body: JSON.stringify({ text, revision: 1, placeholders: [...placeholdersIn(text)], plural: false }),
          });
        }
      });
      await Promise.all(workers);
      const refreshed = await api<Entry[]>(`${root}/locales/${segment(locale)}/${segment(UI_NAMESPACE)}/entries`);
      setEntries(refreshed); setCatalogFilter('');
      setEntriesScope(entriesScopeIdentity);
      setStatus(labels('translation.status.sourcesRegistered', 'Registered {{count}} missing built-in UI source keys. Existing source messages were left unchanged.', { count: missing.length }));
    } catch (e) { setError(message(e, labels)); }
    finally { setBusy(false); }
  }
  async function importBuiltInDraftCatalog() {
    setBusy(true); setError(''); setStatus('');
    try {
      const catalog = await loadBuiltInDraftCatalog(locale);
      if (!catalog) throw new Error(labels('translation.error.missingBuiltInDraft', 'No built-in draft catalog is available for this locale.'));
      const catalogKeys = Object.keys(catalog);
      if (!catalogKeys.length || catalogKeys.some((messageKey) => !Object.hasOwn(englishUiMessages, messageKey))) {
        throw new Error(labels('translation.error.catalogKeyMismatch', 'The built-in draft catalog contains no suggestions or has keys that are not in the current source catalog.'));
      }
      const missingSuggestions = Object.keys(englishUiMessages).filter((messageKey) => !Object.hasOwn(catalog, messageKey));

      let currentLocales = await api<Locale[]>(root + '/locales');
      if (!currentLocales.some((item) => item.tag === locale)) {
        try {
          await api<void>(root + '/locales', { method: 'POST', body: JSON.stringify({ tag: locale, direction: directionFor(locale) }) });
        } catch (error) {
          currentLocales = await api<Locale[]>(root + '/locales');
          if (!currentLocales.some((item) => item.tag === locale)) throw error;
        }
        currentLocales = await api<Locale[]>(root + '/locales');
      }
      setLocales(currentLocales);

      let current = await api<Entry[]>(`${root}/locales/${segment(locale)}/${segment(UI_NAMESPACE)}/entries`);
      let currentByKey = new Map(current.map((entry) => [entry.key, entry]));
      for (const [messageKey, translated] of Object.entries(catalog)) {
        const sourceText = currentByKey.get(messageKey)?.source_text ?? englishUiMessages[messageKey];
        const expected = placeholderTokens(sourceText);
        const actual = placeholderTokens(translated);
        if (!translated.trim() || translated.length > 10000 || translated.includes('<') || translated.includes('>') || expected.join('\n') !== actual.join('\n')) {
          throw new Error(labels('translation.error.invalidBuiltInDraft', 'The built-in draft for {{key}} is blank, unsafe, or has incompatible placeholders.', { key: messageKey }));
        }
        if (currentByKey.get(messageKey)?.plural) throw new Error(labels('translation.error.unsupportedBuiltInPlural', 'The built-in draft importer does not support plural source key {{key}}.', { key: messageKey }));
      }

      const knownSources = new Set(current.map((entry) => entry.key));
      const missingSources = Object.entries(englishUiMessages).filter(([messageKey]) => !knownSources.has(messageKey));
      let nextSource = 0;
      await Promise.all(Array.from({ length: Math.min(6, missingSources.length) }, async () => {
        while (nextSource < missingSources.length) {
          const [messageKey, text] = missingSources[nextSource++];
          await api<void>(`${root}/sources/${segment(UI_NAMESPACE)}/${segment(messageKey)}`, {
            method: 'PUT',
            body: JSON.stringify({ text, revision: 1, placeholders: [...placeholdersIn(text)], plural: false }),
          });
        }
      }));

      const [registered, bundle] = await Promise.all([
        api<Entry[]>(`${root}/locales/${segment(locale)}/${segment(UI_NAMESPACE)}/entries`),
        api<Bundle | null>(`/api/v1/bundles/${segment(registryId)}/${segment(organizationId)}/${segment(locale)}/${segment(UI_NAMESPACE)}`),
      ]);
      current = registered;
      currentByKey = new Map(current.map((entry) => [entry.key, entry]));
      const translations = Object.entries(catalog).filter(([messageKey]) => {
        const entry = currentByKey.get(messageKey);
        const published = !!bundle && (messageKey in bundle.entries || Object.keys(bundle.entries).some((keyName) => keyName.startsWith(`${messageKey}_`)));
        return !!entry && !entry.draft && !published;
      });
      let nextDraft = 0;
      await Promise.all(Array.from({ length: Math.min(6, translations.length) }, async () => {
        while (nextDraft < translations.length) {
          const [messageKey, value] = translations[nextDraft++];
          const entry = currentByKey.get(messageKey);
          if (!entry) continue;
          await api<void>(`${root}/locales/${segment(locale)}/${segment(UI_NAMESPACE)}/${segment(messageKey)}`, {
            method: 'PUT',
            body: JSON.stringify({ value, source_revision: entry.source_revision, plural_forms: null }),
          });
        }
      }));
      const [refreshed, refreshedBundle] = await Promise.all([
        api<Entry[]>(`${root}/locales/${segment(locale)}/${segment(UI_NAMESPACE)}/entries`),
        api<Bundle | null>(`/api/v1/bundles/${segment(registryId)}/${segment(organizationId)}/${segment(locale)}/${segment(UI_NAMESPACE)}`),
      ]);
      setEntries(refreshed); setCatalogFilter(''); setEntriesScope(entriesScopeIdentity); setPublished(refreshedBundle);
      setStatus(missingSuggestions.length
        ? labels('translation.status.draftCatalogImportedPartial', 'Imported {{count}} draft suggestions; {{missing}} newer source messages have no suggestion yet. Existing drafts and published entries were left unchanged. Review every translation before approval or publication.', { count: translations.length, missing: missingSuggestions.length })
        : labels('translation.status.draftCatalogImported', 'Imported {{count}} draft suggestions; existing drafts and published entries were left unchanged. Review every translation before approval or publication.', { count: translations.length }));
    } catch (e) { setError(message(e, labels)); }
    finally { setBusy(false); }
  }
  function chooseEntry(item: Entry) {
    setPublishArmed(false);
    setKey(item.key); setSourceText(item.source_text); setSourceRevision(item.source_revision);
    setPlaceholderText(item.placeholders.join(', ')); setDraft(item.draft); setValue(item.draft?.value ?? '');
    setPlural(item.plural); setPluralFormsText(JSON.stringify(item.draft?.plural_forms ?? { other: item.draft?.value ?? '' }, null, 2));
    setLoadedSelection(JSON.stringify([root, locale, namespace.trim(), item.key.trim()]));
      setStatus(item.draft?.stale ? t(labels, 'status.draftStale', 'This draft is stale against the current source.') : item.draft ? labels('translation.status.draftStateLoaded', 'Loaded {{state}} draft.', { state: catalogState(item.draft.state, labels) }) : t(labels, 'status.translationMissing', 'Translation is missing for this locale and key.'));
  }
  async function addLocale(event: FormEvent) {
    event.preventDefault(); setBusy(true); setError(''); setStatus('');
    try {
      const tag = canonicalLocaleTag(newLocale);
      if (!tag) { setError(t(labels, 'error.invalidLocaleTag', 'Enter a valid BCP 47 locale tag no longer than 64 characters.')); return; }
      await api<void>(root + '/locales', { method: 'POST', body: JSON.stringify({ tag, direction }) });
      const next = await api<Locale[]>(root + '/locales');
      const storedTag = next.find((item) => canonicalLocaleTag(item.tag) === tag)?.tag ?? tag;
      setLocales(next); setLocale(storedTag); setNewLocale(''); setStatus(t(labels, 'status.localeAdded', 'Locale added. Translation text still needs human review.'));
    } catch (e) { setError(message(e, labels)); } finally { setBusy(false); }
  }
  async function saveSource() {
    if (!selectionLoaded) return;
    setBusy(true); setError(''); setStatus('');
    try {
      await api<void>(`${root}/sources/${segment(namespace)}/${segment(key)}`, { method: 'PUT', body: JSON.stringify({ text: sourceText, revision: sourceRevision, placeholders: [...placeholders], plural }) });
      setStatus(t(labels, 'status.sourceSaved', 'Source message saved. Translation changes remain drafts until reviewed and published.'));
    } catch (e) { setError(message(e, labels)); } finally { setBusy(false); }
  }
  async function saveDraft(event: FormEvent) {
    event.preventDefault();
    if (!selectionLoaded) return;
    setBusy(true); setError(''); setStatus('');
    try {
      await api<void>(draftPath, { method: 'PUT', body: JSON.stringify({ value: plural ? pluralForms?.other : value, source_revision: sourceRevision, plural_forms: pluralForms }) });
      await loadKey(); setStatus(t(labels, 'status.draftSaved', 'Draft saved. It is not live until an authorized reviewer approves and publishes it.'));
    } catch (e) { setError(message(e, labels)); } finally { setBusy(false); }
  }
  async function review(decision: 'approve' | 'request_changes') {
    if (!selectionLoaded) return;
    setBusy(true); setError(''); setStatus('');
    try { await api(draftPath + '/review', { method: 'POST', body: JSON.stringify({ decision }) }); await loadKey(); setStatus(decision === 'approve' ? t(labels, 'status.approved', 'Draft approved.') : t(labels, 'status.changesRequested', 'Changes requested.')); }
    catch (e) { setError(message(e, labels)); } finally { setBusy(false); }
  }
  async function publish() {
    if (!selectionLoaded) return;
    if (!publishArmed) {
      setError(''); setStatus(''); focusPublishConfirmation.current = true; setPublishArmed(true);
      return;
    }
    setPublishArmed(false);
    setBusy(true); setError(''); setStatus('');
    try {
      const bundle = await api<Bundle>(`${root}/locales/${segment(locale)}/${segment(namespace)}/publish`, { method: 'POST', body: JSON.stringify({ expected_revision: published?.revision ?? null }) });
      setPublished(bundle); setStatus(labels('translation.status.published', 'Published as revision {{revision}}.', { revision: bundle.revision }));
    } catch (e) { setError(message(e, labels)); } finally { setBusy(false); }
  }
  async function rollback() {
    if (!selectionLoaded) return;
    const target = Number(rollbackRevision);
    if (!Number.isInteger(target) || target < 1) { setError(t(labels, 'error.invalidRevision', 'Enter a valid published revision number.')); return; }
    setBusy(true); setError(''); setStatus('');
    try { const bundle = await api<Bundle>(`${root}/locales/${segment(locale)}/${segment(namespace)}/rollback`, { method: 'POST', body: JSON.stringify({ target_revision: target, expected_revision: published?.revision ?? null }) }); setPublished(bundle); setStatus(labels('translation.status.rollbackPublished', 'Rollback published as new revision {{revision}}.', { revision: bundle.revision })); }
    catch (e) { setError(message(e, labels)); } finally { setBusy(false); }
  }
  const stale = draft?.stale ?? false;
  const stateLabel = stale ? t(labels, 'state.staleSource', 'Stale source') : draft ? catalogState(draft.state, labels) : t(labels, 'state.missing', 'Missing');
  function entryWorkflowState(entry: Entry): EntryWorkflowState {
    if (entry.draft?.stale) return 'stale';
    if (entry.draft?.state === 'draft') return 'draft';
    if (entry.draft?.state === 'in_review') return 'inReview';
    if (entry.draft?.state === 'approved') return 'approved';
    if (published && (entry.key in published.entries || (entry.plural && Object.keys(published.entries).some((name) => name.startsWith(`${entry.key}_`))))) return 'published';
    return 'missing';
  }
  function entryState(entry: Entry) {
    const state = entryWorkflowState(entry);
    if (state === 'stale') return t(labels, 'state.stale', 'Stale');
    if (state === 'draft' || state === 'inReview' || state === 'approved') return catalogState(state === 'inReview' ? 'in_review' : state, labels);
    if (state === 'published') return t(labels, 'state.published', 'Published');
    return t(labels, 'state.missing', 'Missing');
  }
  const coverage = entries.reduce<Record<EntryWorkflowState | 'total', number>>((counts, entry) => {
    counts.total += 1;
    counts[entryWorkflowState(entry)] += 1;
    return counts;
  }, { total: 0, missing: 0, stale: 0, draft: 0, inReview: 0, approved: 0, published: 0 });
  const normalizedFilter = foldForSearch(catalogFilter.trim(), locale);
  const visibleEntries = entries.filter((entry) => !normalizedFilter || foldForSearch(entry.key, locale).includes(normalizedFilter));
  const publishedPreview = plural && published ? Object.entries(published.entries).filter(([name]) => name.startsWith(`${key}_`)).map(([name, text]) => `${name.slice(key.length + 1)}: ${text}`).join('\n') : published?.entries[key];
  return <section className="admin-page" aria-labelledby="translation-title">
    <header className="admin-heading"><div><p className="eyebrow">{t(labels, 'eyebrow', 'Translation management')}</p><h1 id="translation-title">{labels('translation.title')}</h1><p className="lead">{t(labels, 'intro', 'Choose a language and text group, select a message to edit, then review and publish the translations.')}</p></div><span className="admin-badge">{t(labels, 'serverCatalog', 'Server catalog')}</span></header>
    <div className="admin-grid" aria-busy={busy}>
      <section className="admin-card" aria-labelledby="locale-settings-title"><h2 id="locale-settings-title">{t(labels, 'locales', 'Locales')}</h2>
        <label>{t(labels, 'activeLocale', 'Active locale')}<select value={locale} disabled={busy} onChange={(e) => { const next = locales.find((item) => item.tag === e.target.value); setLocale(e.target.value); if (next) setDirection(next.direction as 'ltr' | 'rtl'); setEntries([]); setCatalogFilter(''); setPublished(null); setDraft(null); }}>{locales.map((item) => <option key={item.tag} value={item.tag}>{item.tag} · {item.direction.toUpperCase()}{item.enabled ? '' : ` · ${t(labels, 'disabled', 'disabled')}`}</option>)}</select></label>
        <form className="admin-form" onSubmit={(e) => void addLocale(e)}><h3>{t(labels, 'addLocale', 'Add a locale')}</h3><label>{t(labels, 'localeTag', 'BCP 47 locale tag')}<input value={newLocale} onChange={(e) => { setNewLocale(e.target.value); setDirection(directionFor(e.target.value)); }} maxLength={64} required placeholder={t(labels, 'localeExample', 'vi-VN')} /></label><label>{t(labels, 'textDirection', 'Text direction')}<select value={direction} onChange={(e) => setDirection(e.target.value as 'ltr' | 'rtl')}><option value="ltr">{t(labels, 'leftToRight', 'Left to right')}</option><option value="rtl">{t(labels, 'rightToLeft', 'Right to left')}</option></select></label><button className="button" disabled={busy}>{t(labels, 'addLocale', 'Add locale')}</button></form>
      </section>
      <section className="admin-card" aria-labelledby="message-key-title"><h2 id="message-key-title">{t(labels, 'messageKey', 'Find a message')}</h2><div className="admin-form two-col"><label>{labels('translation.namespace')}<input value={namespace} onChange={(e) => setNamespace(e.target.value)} required maxLength={64} /></label><label>{labels('translation.key')}<input value={key} onChange={(e) => setKey(e.target.value)} required maxLength={128} /></label></div><p className="help">{t(labels, 'messageSelectorHelp', 'These IDs select which messages to load. They do not change the text readers see.')}</p><div className="button-row"><button className="button" disabled={busy || !namespace.trim()} onClick={() => void loadEntries()}>{labels('translation.loadKeys')}</button><button className="button" disabled={busy || !key.trim()} onClick={() => void loadKey()}>{t(labels, 'loadKeyBundle', 'Load message')}</button>{namespace === UI_NAMESPACE && <button className="button button-subtle" disabled={busy} onClick={() => void registerMissingUiSources()}>{t(labels, 'registerMissingUiSources', 'Register missing built-in UI source keys')}</button>}{namespace === UI_NAMESPACE && hasBuiltInDraftCatalog(locale) && <button className="button button-subtle" disabled={busy} onClick={() => void importBuiltInDraftCatalog()}>{t(labels, 'importDraftCatalog', 'Import built-in draft translations')}</button>}</div>{namespace === UI_NAMESPACE && hasBuiltInDraftCatalog(locale) && <p className="help">{t(labels, 'importDraftCatalogHelp', 'This imports unreviewed translation suggestions as drafts. It does not publish them and will not overwrite existing drafts or published text.')}</p>}
        {entriesMatchScope && entries.length > 0 && <label>{labels('translation.key')}<input type="search" value={catalogFilter} onChange={(event) => setCatalogFilter(event.target.value)} /></label>}
      {entriesMatchScope && entries.length > 0 && <div className="catalog-entry-list" aria-label={t(labels, 'keyStatusAccessibleName', 'Translation message status')}><h3>{t(labels, 'namespaceKeys', 'Messages in this group')}</h3><span className="catalog-filter-count">{visibleEntries.length} / {entries.length}</span>{visibleEntries.map((entry) => <button type="button" className={entry.key === key ? 'catalog-entry selected' : 'catalog-entry'} key={entry.key} onClick={() => chooseEntry(entry)}><span><code>{entry.key}</code><small>{entry.source_text}</small></span><strong>{entryState(entry)}</strong></button>)}</div>}
        {entriesMatchScope && <section className="admin-card coverage-summary" aria-labelledby="translation-coverage-title" lang={locale} dir={direction}>
          <h3 id="translation-coverage-title">{labels('translation.coverage.heading', 'Translation progress — {{locale}} / {{namespace}}', { locale, namespace })}</h3>
          <dl className="coverage-grid">
            <div><dt>{t(labels, 'coverage.total', 'Total messages')}</dt><dd>{coverage.total}</dd></div>
            <div><dt>{t(labels, 'coverage.missing', 'Missing')}</dt><dd>{coverage.missing}</dd></div>
            <div><dt>{t(labels, 'coverage.stale', 'Stale')}</dt><dd>{coverage.stale}</dd></div>
            <div><dt>{t(labels, 'coverage.draft', 'Draft')}</dt><dd>{coverage.draft}</dd></div>
            <div><dt>{t(labels, 'coverage.inReview', 'In review')}</dt><dd>{coverage.inReview}</dd></div>
            <div><dt>{t(labels, 'coverage.approved', 'Approved')}</dt><dd>{coverage.approved}</dd></div>
            <div><dt>{t(labels, 'coverage.published', 'Already published')}</dt><dd>{coverage.published}</dd></div>
          </dl>
          <p className="help">{t(labels, 'coverage.help', 'These counts show server workflow states only. They do not certify translation quality or mean that a qualified language or accessibility review has taken place.')}</p>
        </section>}
        {stale && <p className="admin-warning" role="alert">{t(labels, 'staleSourceWarning', 'The source changed since this draft was prepared. Refresh the source text and update the draft before review.')}</p>}
        <div className="admin-form"><label>{t(labels, 'sourceText', 'Source text')}<textarea rows={3} value={sourceText} onChange={(e) => setSourceText(e.target.value)} maxLength={4000} /></label><div className="two-col"><label>{t(labels, 'sourceRevision', 'Source revision')}<input type="number" min="1" value={sourceRevision} onChange={(e) => setSourceRevision(Number(e.target.value))} /></label><label>{t(labels, 'requiredPlaceholders', 'Required placeholders')}<input value={placeholderText} onChange={(e) => setPlaceholderText(e.target.value)} placeholder={t(labels, 'placeholderNames', 'name, count')} aria-describedby="placeholder-help" /></label></div><label className="check-field"><input type="checkbox" checked={plural} onChange={(e) => setPlural(e.target.checked)} /> {t(labels, 'pluralMessage', 'Source message has plural forms')}</label><p id="placeholder-help" className="help">{t(labels, 'placeholderHelp', 'Use comma-separated names. Placeholders such as {{name}} must appear in every translation form.')}</p>{sourceMissingPlaceholders.length > 0 && <p role="alert" className="admin-warning">{labels('translation.addDetectedPlaceholders', 'Add detected source placeholders: {{placeholders}}', { placeholders: sourceMissingPlaceholders.join(', ') })}</p>}<button className="button button-subtle" disabled={busy || !selectionLoaded || !key.trim() || sourceMissingPlaceholders.length > 0} onClick={() => void saveSource()}>{t(labels, 'saveSourceKey', 'Save source key')}</button></div>
        <form className="admin-form" onSubmit={(e) => void saveDraft(e)}>{plural ? <label>{t(labels, 'pluralForms', 'Plural translation forms')}<textarea dir={direction} lang={locale} rows={7} value={pluralFormsText} onChange={(e) => setPluralFormsText(e.target.value)} spellCheck maxLength={12000} aria-describedby="draft-label plural-help" /></label> : <label>{t(labels, 'translationDraft', 'Translation draft')}<textarea dir={direction} lang={locale} rows={4} value={value} onChange={(e) => setValue(e.target.value)} maxLength={4000} required aria-describedby="draft-label" /></label>}{plural && <p id="plural-help" className="help">{t(labels, 'pluralJsonHelp', 'Enter a JSON object mapping every required locale plural category to text, for example {{example}}. The API validates the exact category set.').replace('{{example}}', '{ "one": "{count} item", "other": "{count} items" }')}</p>}{(plural ? (!pluralForms || !pluralForms.other || !pluralPlaceholdersMatch) : !draftPlaceholdersMatch) && <p className="admin-warning" role="alert">{labels('translation.error.placeholderMismatch', 'Every translation form must use exactly the same placeholders as the source message.')}</p>}<p id="draft-label" className="help">{t(labels, 'draftReviewHelp', 'Draft content is provisional until reviewed. Check script coverage and font rendering with a qualified language reviewer.')}</p><button className="button" disabled={busy || !selectionLoaded || (plural ? !pluralForms || !pluralForms.other || !pluralPlaceholdersMatch : !value.trim() || !draftPlaceholdersMatch)}>{t(labels, 'saveDraft', 'Save draft')}</button></form>
      </section>
      <section className="admin-card" aria-labelledby="review-publish-title"><h2 id="review-publish-title">{t(labels, 'reviewAndRelease', 'Review and release')}</h2><p>{t(labels, 'currentState', 'Current state')}: <strong>{selectionLoaded ? stateLabel : t(labels, 'loadKeyBundle', 'Load message')}</strong></p><p className="help">{t(labels, 'roleSeparationHelp', 'Authoring, review, and release use separate server permissions. The service checks each action; the browser does not grant these roles.')}</p><div className="button-row"><button className="button" disabled={busy || !selectionLoaded || !draft} onClick={() => void review('request_changes')}>{t(labels, 'requestChanges', 'Request changes')}</button><button className="button" disabled={busy || !selectionLoaded || !draft || stale} onClick={() => void review('approve')}>{t(labels, 'approveDraft', 'Approve draft')}</button></div><div className="admin-form"><button ref={publishTriggerRef} className="button button-primary" disabled={busy || !selectionLoaded || !draft || stale || draft.state !== 'approved' || publishArmed} onClick={() => void publish()}>{t(labels, 'publishBundle', 'Publish translations')}</button><p className="help">{t(labels, 'publishRequirementsHelp', 'Publishing updates every message in the selected text group and requires an approved, current draft.')}</p>{publishArmed && <div className="publish-confirmation" role="alert"><p>{labels('translation.publishScopeWarning', 'Publishing will update all messages in the {{locale}} text group {{namespace}}, not just the message open now. Current published revision: {{revision}}. Only an approved, current draft can be published.', { locale, namespace, revision: published?.revision ?? t(labels, 'notLoaded', 'not loaded') })}</p><div className="button-row"><button ref={publishConfirmRef} type="button" className="button button-primary" disabled={busy} onClick={() => void publish()}>{t(labels, 'confirmPublishBundle', 'Confirm publish')}</button><button type="button" className="button button-subtle" disabled={busy} onClick={() => { returnFocusToPublish.current = true; setPublishArmed(false); }}>{t(labels, 'cancelPublish', 'Cancel')}</button></div></div>}</div>
        <div className="admin-form"><label>{t(labels, 'rollbackTarget', 'Rollback target revision')}<input type="number" min="1" value={rollbackRevision} onChange={(e) => setRollbackRevision(e.target.value)} /></label><p className="help">{t(labels, 'currentPublishedRevision', 'Current published revision')}: {published?.revision ?? t(labels, 'notLoaded', 'not loaded')}</p><button className="button" disabled={busy || !selectionLoaded || !published} onClick={() => void rollback()}>{t(labels, 'publishRollback', 'Publish rollback')}</button></div>
      </section>
      <section className="admin-card" aria-labelledby="preview-title"><h2 id="preview-title">{t(labels, 'preview', 'Preview')}</h2><p className="help">{t(labels, 'previewHelp', 'Preview shows this local draft beside the currently published value. It does not activate draft content.')}</p><div className="preview-pair"><div><span>{t(labels, 'draft', 'Draft')} · {locale} · {direction.toUpperCase()}</span><p dir={direction} lang={locale}>{plural ? pluralFormsText : value || t(labels, 'noDraftText', 'No draft text entered.')}</p></div><div><span>{t(labels, 'publishedBundle', 'Published bundle')} · {t(labels, 'revision', 'revision')} {published?.revision ?? '—'}</span><p dir={direction} lang={locale}>{publishedPreview || t(labels, 'noPublishedValue', 'No published value for this key.')}</p></div></div></section>
    </div>
    {selectionLoaded && <p className="admin-state" role="status">{t(labels, 'statusLabel', 'Status')}: <strong>{stateLabel}</strong>{draft && <> · {t(labels, 'sourceRevision', 'source revision')} {draft.source_revision}</>}</p>}
    {error && <p className="alert alert-error" role="alert">{error}</p>}{(busy || status) && <p className="inline-status" role="status" aria-live="polite">{busy ? labels('review.waitingForServer', 'Waiting for server…') : status}</p>}
  </section>;
}

function t(labels: Props['labels'], key: string, fallback: string) { return labels('translation.' + key, fallback); }
function foldForSearch(value: string, locale: string) {
  try { return value.toLocaleLowerCase(locale); } catch { return value.toLowerCase(); }
}
function catalogState(state: string, labels: Props['labels']) {
  const known: Record<string, string> = {
    draft: t(labels, 'state.draft', 'Draft'),
    in_review: t(labels, 'state.inReview', 'In review'),
    approved: t(labels, 'state.approved', 'Approved'),
  };
  return known[state] ?? state.replaceAll('_', ' ');
}
function message(error: unknown, labels: Props['labels']) {
  if (error instanceof ApiFailure) {
    const translated = apiFailureCopy(error, labels);
    if (translated) return translated;
    if (error.status === 0) return t(labels, 'error.network', 'The service could not be reached. Check the connection and retry.');
    if (error.status === 401) return t(labels, 'error.unauthenticated', 'Your session has expired. Sign in again to continue.');
    if (error.status === 403 || error.status === 404) return t(labels, 'error.scope', 'This action is not available in the current access scope.');
    if (error.status === 409) return t(labels, 'error.conflict', 'The catalog changed since it was loaded. Refresh and review before retrying.');
    if (error.status >= 500) return t(labels, 'error.unavailable', 'The service is temporarily unavailable. Your changes were not confirmed.');
    return error.message;
  }
  return error instanceof Error ? error.message : t(labels, 'error.requestFailed', 'The catalog request could not be completed.');
}
