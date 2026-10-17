import { useEffect, useMemo, useRef, useState } from 'react';
import type { FormEvent } from 'react';
import { Link, Navigate, Route, Routes, useLocation } from 'react-router-dom';
import { api, ApiFailure } from './api';
import { readScope, saveScope } from './config';
import type { RegistryScope } from './config';
import { applyAppearance, applyLocale } from '../../../../packages/design-system/src/index';
import { createUiTranslator, UI_NAMESPACE } from '../../../../packages/localization/src/webCatalogPreview';
import { publishedLocaleOptions, searchPublishedLocales } from './publishedLocales';
import type { PublishedLocale } from './publishedLocales';
import { directionFor, parsePreference, resolveColorMode } from './preferences';
import type { Preferences } from './preferences';
import { PersonDetailScreen, RegisterScreen, SearchScreen } from '../features/registry/RegistryScreens';
import { ReviewWorkspace } from '../features/review/ReviewWorkspace';
import { TranslationManager } from '../features/localization/TranslationManager';
import { AppearanceAdmin } from '../features/appearance/AppearanceAdmin';
import { CountryProfileAdmin } from '../features/country_profiles/CountryProfileAdmin';
import { RouteFocusManager } from './RouteFocusManager';

type Session = { actor_kind: 'human' | 'integration_client' | 'system'; actor_id: string; request_id: string };
type CatalogBundle = { locale: string; namespace: string; revision: number; entries: Record<string, string>; fallback_locale: string };
type ThemeProfile = { color_mode: 'light' | 'dark' | 'system'; tokens?: { canvas: string; surface: string; text: string; text_muted: string; action: string; on_action: string; control_border: string; focus: string } };

export function App() {
  const location = useLocation();
  const [session, setSession] = useState<Session | null>(null);
  const [sessionReady, setSessionReady] = useState(false);
  const [sessionError, setSessionError] = useState('');
  const [sessionCheckAttempt, setSessionCheckAttempt] = useState(0);
  const [sessionRetryBusy, setSessionRetryBusy] = useState(false);
  const signInLinkRef = useRef<HTMLAnchorElement>(null);
  const [signOutBusy, setSignOutBusy] = useState(false);
  const [signOutError, setSignOutError] = useState('');
  const [scope, setScope] = useState<RegistryScope>(readScope);
  const [scopeDraft, setScopeDraft] = useState(scope);
  const [scopeOpen, setScopeOpen] = useState(!scope.registryId || !scope.organizationId);
  const scopeToggleRef = useRef<HTMLButtonElement>(null);
  const [preferences, setPreferences] = useState<Preferences>(parsePreference(null));
  const [localeDraft, setLocaleDraft] = useState('en');
  const [localeSearch, setLocaleSearch] = useState('');
  const [publishedLocales, setPublishedLocales] = useState<PublishedLocale[]>([]);
  const [preferenceMessage, setPreferenceMessage] = useState('');
  const [bundle, setBundle] = useState<CatalogBundle | null>(null);
  const [bundleMessage, setBundleMessage] = useState('');
  const [profile, setProfile] = useState<ThemeProfile | null>(null);
  const [busy, setBusy] = useState(false);
  const [systemDark, setSystemDark] = useState(() => window.matchMedia?.('(prefers-color-scheme: dark)').matches ?? false);
  const mode = resolveColorMode(preferences.color_mode, systemDark);
  const direction = directionFor(preferences.locale_tag);
  const localeOptions = useMemo(() => publishedLocaleOptions(publishedLocales, preferences.locale_tag), [publishedLocales, preferences.locale_tag]);
  const matchingLocales = useMemo(() => searchPublishedLocales(localeOptions, localeSearch), [localeOptions, localeSearch]);
  const labels = useMemo(() => createUiTranslator(bundle?.namespace === UI_NAMESPACE ? bundle.entries : undefined), [bundle]);

  useEffect(() => {
    let active = true;
    let confirmedSignedOut = false;
    if (sessionCheckAttempt === 0) setSessionReady(false);
    void api<Session>('/api/v1/access/session').then((value) => {
      if (active) { setSession(value); setSessionError(''); }
    }).catch((error) => {
      if (!active) return;
      setSession(null);
      if (error instanceof ApiFailure && error.status === 401) { confirmedSignedOut = true; setSessionError(''); }
      else if (error instanceof ApiFailure && error.status === 0) setSessionError(labels('common.error.network', 'The service could not be reached. Check the connection and retry.'));
      else if (error instanceof ApiFailure && (error.status === 403 || error.status === 404)) setSessionError(labels('common.error.scope', 'This action is not available in the current access scope.'));
      else if (error instanceof ApiFailure && error.status >= 500) setSessionError(labels('shell.session.error', 'Sign-in status could not be checked. Try again.'));
      else setSessionError(labels('shell.session.error', 'Sign-in status could not be checked. Try again.'));
    }).finally(() => {
      if (!active) return;
      setSessionReady(true);
      setSessionRetryBusy(false);
      // A confirmed signed-out response removes the retry action. Move keyboard
      // focus to the next useful action instead of letting it fall to the page.
      if (sessionCheckAttempt > 0 && confirmedSignedOut) signInLinkRef.current?.focus();
    });
    return () => { active = false; };
  }, [sessionCheckAttempt]);

  useEffect(() => {
    if (session && sessionCheckAttempt > 0) document.getElementById('main-content')?.focus();
  }, [session, sessionCheckAttempt]);

  useEffect(() => {
    const query = window.matchMedia('(prefers-color-scheme: dark)');
    const update = () => setSystemDark(query.matches);
    query.addEventListener?.('change', update);
    return () => query.removeEventListener?.('change', update);
  }, []);
  useEffect(() => {
    applyAppearance(mode, preferences.high_contrast ? 'high' : 'standard', preferences.density);
    applyLocale(preferences.locale_tag, direction);
  }, [mode, preferences.high_contrast, preferences.density, preferences.locale_tag, direction]);
  useEffect(() => {
    if (!session || !scope.registryId || !scope.organizationId) return;
    let active = true;
    const root = '/api/v1/config/' + encodeURIComponent(scope.registryId) + '/' + encodeURIComponent(scope.organizationId);
    void api<Partial<Preferences> | null>(root + '/appearance/preference')
      .then((value) => { if (active) setPreferences(parsePreference(value)); })
      .catch(() => { if (active) setPreferenceMessage(labels('shell.preference.loadError')); });
    void api<ThemeProfile | null>(root + '/appearance')
      .then((value) => { if (active) setProfile(value); }).catch(() => { if (active) setProfile(null); });
    return () => { active = false; };
  }, [session, scope.registryId, scope.organizationId]);
  useEffect(() => {
    setLocaleDraft(preferences.locale_tag);
  }, [preferences.locale_tag]);

  useEffect(() => {
    if (!scope.registryId || !scope.organizationId) { setPublishedLocales([]); return; }
    let active = true;
    const url = '/api/v1/bundles/' + encodeURIComponent(scope.registryId) + '/' + encodeURIComponent(scope.organizationId) + '/locales';
    void api<PublishedLocale[]>(url).then((value) => {
      if (active) setPublishedLocales(Array.isArray(value) ? value : []);
    }).catch(() => { if (active) setPublishedLocales([]); });
    return () => { active = false; };
  }, [scope.registryId, scope.organizationId]);

  useEffect(() => {
    if (!scope.registryId || !scope.organizationId) { setBundle(null); return; }
    let active = true;
    const url = '/api/v1/bundles/' + encodeURIComponent(scope.registryId) + '/' + encodeURIComponent(scope.organizationId) + '/' + encodeURIComponent(preferences.locale_tag) + '/ui';
    void api<CatalogBundle | null>(url).then((value) => {
      if (!active) return;
      if (!value) {
        setBundle(null);
        setBundleMessage(labels('shell.catalog.missing'));
        return;
      }
      setBundle(value);
      setBundleMessage(labels('shell.catalog.published', undefined, { revision: value.revision }));
    }).catch(() => {
      if (active) {
        setBundle(null);
        setBundleMessage(labels('shell.catalog.missing'));
      }
    });
    return () => { active = false; };
  }, [scope.registryId, scope.organizationId, preferences.locale_tag]);

  async function updatePreference(change: Partial<Preferences>) {
    if (!session || !scope.registryId || !scope.organizationId) {
      setPreferenceMessage(labels('shell.preference.signInRequired'));
      return;
    }
    const next = { ...preferences, ...change };
    setBusy(true); setPreferenceMessage('');
    const base = '/api/v1/config/' + encodeURIComponent(scope.registryId) + '/' + encodeURIComponent(scope.organizationId);
    try {
      await api<void>(base + '/appearance/preference', {
        method: 'PUT',
        body: JSON.stringify({ color_mode: next.color_mode, density: next.density, high_contrast: next.high_contrast, locale_tag: next.locale_tag }),
      });
      setPreferences(next); setPreferenceMessage(labels('shell.preference.saved'));
    } catch { setPreferenceMessage(labels('shell.preference.saveError')); }
    finally { setBusy(false); }
  }
  function submitScope(event: FormEvent) {
    event.preventDefault();
    const next = { registryId: scopeDraft.registryId.trim(), organizationId: scopeDraft.organizationId.trim() };
    saveScope(next); setScope(next); setScopeOpen(false); scopeToggleRef.current?.focus();
  }
  function closeScope() { setScopeOpen(false); scopeToggleRef.current?.focus(); }
  async function signOut() {
    if (signOutBusy) return;
    setSignOutBusy(true);
    setSignOutError('');
    try {
      await api<void>('/api/v1/access/logout', { method: 'POST' });
      setSession(null);
      setSessionError('');
    } catch {
      // A failed logout response is ambiguous. Keep the authenticated workspace
      // visible unless the session endpoint confirms that no session remains.
      try {
        const current = await api<Session>('/api/v1/access/session');
        setSession(current);
        setSignOutError(labels('shell.signOut.failed', 'Sign-out could not be confirmed. You may still be signed in. Try again.'));
      } catch (error) {
        if (error instanceof ApiFailure && error.status === 401) {
          setSession(null);
          setSessionError('');
          setSignOutError('');
        } else {
          setSignOutError(labels('shell.signOut.failed', 'Sign-out could not be confirmed. You may still be signed in. Try again.'));
        }
      }
    } finally {
      setSignOutBusy(false);
    }
  }

  if (!sessionReady) return <main className="auth-screen"><p role="status">{labels('shell.session.checking')}</p></main>;
  if (!session) return <main className="auth-screen"><section className="auth-card">
    <p className="brand-mark"><span className="brand-symbol" aria-hidden="true">l</span><span className="brand-name">liteCR</span><span>{labels('shell.brand')}</span></p><h1>{labels('shell.signIn')}</h1>
    <p>{labels('shell.signIn.instructions')}</p>
    {sessionError && <div className="alert alert-error" role="alert">{sessionError}</div>}
    {sessionError && <div className="auth-retry">
      <button className="button full-width" type="button" disabled={sessionRetryBusy} onClick={() => {
        if (sessionRetryBusy) return;
        setSessionRetryBusy(true);
        setSessionCheckAttempt((attempt) => attempt + 1);
      }}>{sessionRetryBusy ? labels('shell.session.retryPending', 'Checking…') : labels('shell.session.retry', 'Check again')}</button>
      {sessionRetryBusy && <p className="help" role="status">{labels('shell.session.checking')}</p>}
    </div>}
    <a ref={signInLinkRef} className="button button-primary full-width" href="/api/v1/access/oidc/login">{labels('shell.signIn.continue')}</a>
    <p className="help">{labels('shell.signIn.passwordNote')}</p>
  </section></main>;

  return <div className="app-shell" data-mode={mode} data-contrast={preferences.high_contrast ? 'high' : 'standard'} data-density={preferences.density}>
    <RouteFocusManager ready={sessionReady && Boolean(session)} />
    <a className="skip-link" href="#main-content">{labels('shell.skip')}</a>
    <header className="topbar">
      <Link className="brand-mark" to="/" aria-label={labels('shell.home')}><span className="brand-symbol" aria-hidden="true">l</span><span className="brand-name">liteCR</span><span>{labels('shell.brand')}</span></Link>
      <div className="topbar-context"><span className="session-status"><span className="status-dot" aria-hidden="true" /> {labels('shell.session.signedIn')}</span>
        <button ref={scopeToggleRef} type="button" className="button button-subtle" aria-label={labels('shell.scope.edit')} aria-expanded={scopeOpen} aria-controls="registry-scope-form" onClick={() => { if (scopeOpen) closeScope(); else { setScopeDraft(scope); setScopeOpen(true); } }}>{labels('shell.scope.title')}</button>
        <button className="button button-subtle" disabled={signOutBusy} onClick={() => void signOut()}>{signOutBusy ? labels('shell.signOut.pending', 'Signing out…') : signOutError ? labels('shell.signOut.retry', 'Retry sign out') : labels('shell.signOut')}</button></div>
    </header>
    {signOutBusy && <p className="inline-status" role="status">{labels('shell.signOut.pending', 'Signing out…')}</p>}
    {signOutError && <div className="alert alert-error" role="alert">{signOutError}</div>}
    <form id="registry-scope-form" className="scope-bar" hidden={!scopeOpen} onSubmit={submitScope}>
      <strong>{labels('shell.scope.title')}</strong>
      <label>{labels('shell.scope.registryId')}<input value={scopeDraft.registryId} onChange={(e) => setScopeDraft({ ...scopeDraft, registryId: e.target.value })} required /></label>
      <label>{labels('shell.scope.organizationId')}<input value={scopeDraft.organizationId} onChange={(e) => setScopeDraft({ ...scopeDraft, organizationId: e.target.value })} required /></label>
      <button className="button button-primary">{labels('shell.scope.apply')}</button>
      {scope.registryId && <button type="button" className="button" onClick={closeScope}>{labels('shell.scope.close')}</button>}
    </form>
    <div className="workspace">
      <aside className="side-nav" aria-label={labels('shell.nav.main')}><p className="nav-label">{labels('shell.nav.workspace')}</p><nav>
        <Link aria-label={labels('shell.nav.openSearch')} aria-current={location.pathname === '/' ? 'page' : undefined} className={location.pathname === '/' ? 'nav-link active' : 'nav-link'} to="/"><NavIcon name="search" />{labels('shell.nav.search')}</Link>
        <Link aria-label={labels('shell.nav.openRegister')} aria-current={location.pathname === '/register' ? 'page' : undefined} className={location.pathname === '/register' ? 'nav-link active' : 'nav-link'} to="/register"><NavIcon name="register" />{labels('shell.nav.register')}</Link>
        <Link aria-label={labels('shell.nav.openReview')} aria-current={location.pathname === '/review' ? 'page' : undefined} className={location.pathname === '/review' ? 'nav-link active' : 'nav-link'} to="/review"><NavIcon name="review" />{labels('shell.nav.review')}</Link>
        <Link aria-current={location.pathname === '/appearance' ? 'page' : undefined} className={location.pathname === '/appearance' ? 'nav-link active' : 'nav-link'} to="/appearance"><NavIcon name="settings" />{labels('shell.nav.settings')}</Link>
        <Link aria-current={location.pathname.startsWith('/admin/translations') ? 'page' : undefined} className={location.pathname.startsWith('/admin/translations') ? 'nav-link active' : 'nav-link'} to="/admin/translations"><NavIcon name="language" />{labels('shell.nav.translationManager')}</Link>
        <Link aria-current={location.pathname === '/admin/appearance' ? 'page' : undefined} className={location.pathname === '/admin/appearance' ? 'nav-link active' : 'nav-link'} to="/admin/appearance"><NavIcon name="appearance" />{labels('shell.nav.appearanceAdmin')}</Link>
        <Link aria-current={location.pathname === '/admin/country-profiles' ? 'page' : undefined} className={location.pathname === '/admin/country-profiles' ? 'nav-link active' : 'nav-link'} to="/admin/country-profiles"><NavIcon name="country" />{labels('shell.nav.countryProfiles')}</Link>
      </nav><div className="nav-context"><span>{labels('shell.scope.current')}</span><strong>{scope.registryId || labels('shell.scope.notSet')}</strong><small>{scope.organizationId || labels('shell.scope.chooseOrganization')}</small></div></aside>
      <main id="main-content" className="main-content" tabIndex={-1}>
        <details className="workspace-preferences">
          <summary>
            <span>{labels('shell.tools.label')}</span>
            <span className="preference-current" dir="auto">{preferences.locale_tag} · {labels(`shell.option.${preferences.color_mode === 'system' ? 'system' : preferences.color_mode}`)}</span>
          </summary>
          <div className="global-tools" role="group" aria-label={labels('shell.tools.label')}>
          <div className="locale-control locale-discovery">
            <label htmlFor="published-locale-search">{labels('shell.locale.search', 'Search published languages')}</label>
            <input id="published-locale-search" type="search" dir="auto" value={localeSearch} onChange={(event) => setLocaleSearch(event.target.value)} autoComplete="off" aria-describedby="published-locale-help" />
            <select aria-label={labels('shell.locale.published', 'Published UI languages')} value="" onChange={(event) => { if (event.target.value) setLocaleDraft(event.target.value); }}>
              <option value="">{matchingLocales.length ? labels('shell.locale.choosePublished', 'Choose a published language') : labels('shell.locale.noPublished', 'No published UI languages match this search')}</option>
              {matchingLocales.map((item) => <option value={item.tag} key={item.tag}>{item.label}</option>)}
            </select>
            <p id="published-locale-help" className="help">{labels('shell.locale.searchHelp', 'Search by language name or BCP 47 tag. Choosing a published language fills the locale tag below.')}</p>
          </div>
          <form className="locale-control" onSubmit={(event) => { event.preventDefault(); void updatePreference({ locale_tag: localeDraft.trim() }); }}>
            <label htmlFor="locale-tag">{labels('shell.locale.label')}</label>
            <input id="locale-tag" dir="ltr" value={localeDraft} pattern="[A-Za-z]{2,8}(-[A-Za-z0-9]{1,8})*" required
              disabled={busy} onChange={(e) => setLocaleDraft(e.target.value)} autoComplete="off" />
            <button className="button" disabled={busy || localeDraft.trim() === preferences.locale_tag}>{labels('shell.locale.save')}</button>
          </form>
          <label>{labels('shell.theme.label')}<select value={preferences.color_mode} disabled={busy} onChange={(e) => void updatePreference({ color_mode: e.target.value as Preferences['color_mode'] })}>
            <option value="system">{labels('shell.option.system')}</option><option value="light">{labels('shell.option.light')}</option><option value="dark">{labels('shell.option.dark')}</option></select></label>
          <label>{labels('shell.contrast.label')}<select value={preferences.high_contrast ? 'high' : 'standard'} disabled={busy} onChange={(e) => void updatePreference({ high_contrast: e.target.value === 'high' })}>
            <option value="standard">{labels('shell.option.standard')}</option><option value="high">{labels('shell.option.highContrast')}</option></select></label>
          <label>{labels('shell.density.label')}<select value={preferences.density} disabled={busy} onChange={(e) => void updatePreference({ density: e.target.value as Preferences['density'] })}>
            <option value="comfortable">{labels('shell.option.comfortable')}</option><option value="compact">{labels('shell.option.compact')}</option></select></label>
          </div>
        </details>
        {preferenceMessage && <p className="inline-status" role="status">{preferenceMessage}</p>}
        {bundleMessage && <p className="catalog-status" role="status">{bundleMessage}</p>}
        {profile?.tokens && <ThemeTokens profile={profile} mode={mode} highContrast={preferences.high_contrast} />}
        <Routes>
          <Route path="/" element={<SearchScreen scope={scope} labels={labels} />} />
          <Route path="/register" element={<RegisterScreen scope={scope} labels={labels} locale={preferences.locale_tag} />} />
          <Route path="/persons/:publicId" element={<PersonDetailScreen scope={scope} labels={labels} />} />
          <Route path="/review" element={<ReviewWorkspace scope={scope} labels={labels} />} />
          <Route path="/appearance" element={<AppearanceInfo labels={labels} />} />
          <Route path="/admin/translations" element={<TranslationManager registryId={scope.registryId} organizationId={scope.organizationId} labels={labels} />} />
          <Route path="/admin/appearance" element={<AppearanceAdmin registryId={scope.registryId} organizationId={scope.organizationId} labels={labels} />} />
          <Route path="/admin/country-profiles" element={<CountryProfileAdmin registryId={scope.registryId} organizationId={scope.organizationId} labels={labels} />} />
          <Route path="*" element={<Navigate to="/" replace />} />
        </Routes>
        <footer className="provisional-note"><strong>{labels('shell.footer.coverage')}</strong> {labels('shell.footer.copy')}</footer>
      </main>
    </div>
  </div>;
}

function ThemeTokens({ profile, mode, highContrast }: { profile: ThemeProfile; mode: 'light' | 'dark'; highContrast: boolean }) {
  useEffect(() => {
    const values = profile.tokens;
    if (!values || highContrast || profile.color_mode !== mode) return;
    const root = document.documentElement;
    const pairs: Array<[string, string]> = [
      ['--color-canvas', values.canvas], ['--color-surface', values.surface], ['--color-text', values.text],
      ['--color-text-muted', values.text_muted], ['--color-action', values.action],
      ['--color-on-action', values.on_action], ['--color-border', values.control_border], ['--color-focus', values.focus],
    ];
    for (const [key, value] of pairs) if (/^#[0-9a-fA-F]{6}$/.test(value)) root.style.setProperty(key, value);
    return () => { for (const [key] of pairs) root.style.removeProperty(key); };
  }, [profile, mode, highContrast]);
  return null;
}

function NavIcon({ name }: { name: 'search' | 'register' | 'review' | 'settings' | 'language' | 'appearance' | 'country' }) {
  const common = { fill: 'none', stroke: 'currentColor', strokeLinecap: 'round' as const, strokeLinejoin: 'round' as const, strokeWidth: 1.8 };
  return <svg className="nav-icon" viewBox="0 0 24 24" aria-hidden="true" focusable="false">
    {name === 'search' && <><circle {...common} cx="10.8" cy="10.8" r="6.4" /><path {...common} d="m16 16 4.2 4.2" /></>}
    {name === 'register' && <><circle {...common} cx="9" cy="8" r="3.2" /><path {...common} d="M3.5 20c.4-3.4 2.2-5.2 5.5-5.2 2 0 3.4.6 4.3 1.8M18 9v7m-3.5-3.5h7" /></>}
    {name === 'review' && <><path {...common} d="M7 3.8h8l4 4V20a1.5 1.5 0 0 1-1.5 1.5h-11A1.5 1.5 0 0 1 5 20V5.3a1.5 1.5 0 0 1 1.5-1.5Z" /><path {...common} d="M14.5 4v4h4M8.5 14l2.2 2.2 4.8-5" /></>}
    {name === 'settings' && <><circle {...common} cx="12" cy="12" r="3" /><path {...common} d="m19.4 15 .1.1a1.7 1.7 0 1 1-2.4 2.4l-.1-.1a1.7 1.7 0 0 0-2.9 1.2v.2a1.7 1.7 0 1 1-3.4 0v-.2A1.7 1.7 0 0 0 8 17.4l-.1.1a1.7 1.7 0 1 1-2.4-2.4l.1-.1a1.7 1.7 0 0 0-1.2-2.9h-.2a1.7 1.7 0 1 1 0-3.4h.2a1.7 1.7 0 0 0 1.2-2.9l-.1-.1a1.7 1.7 0 1 1 2.4-2.4l.1.1a1.7 1.7 0 0 0 2.9-1.2v-.2a1.7 1.7 0 1 1 3.4 0v.2a1.7 1.7 0 0 0 2.9 1.2l.1-.1a1.7 1.7 0 1 1 2.4 2.4l-.1.1a1.7 1.7 0 0 0 1.2 2.9h.2a1.7 1.7 0 1 1 0 3.4h-.2a1.7 1.7 0 0 0-1.2 2.9Z" transform="translate(-1.5 -1.5) scale(.94)" /></>}
    {name === 'language' && <><path {...common} d="M4 5h11M9.5 3v2m3.5 0c-.7 4-3.2 6.6-7 8.5M6.5 8c1.2 2 3.2 3.8 5.7 5M14 20l3.3-8 3.2 8m-5.4-2h4.4" /></>}
    {name === 'appearance' && <><path {...common} d="M12 3.5a8.5 8.5 0 1 0 0 17h1.2a2 2 0 0 0 1.4-3.4 1.5 1.5 0 0 1 1.1-2.6h1.1a4.7 4.7 0 0 0 4.7-4.7C21.5 6.3 17.2 3.5 12 3.5Z" /><circle cx="7.5" cy="10" r=".8" fill="currentColor" /><circle cx="10.5" cy="7" r=".8" fill="currentColor" /><circle cx="15" cy="7.5" r=".8" fill="currentColor" /></>}
    {name === 'country' && <><circle {...common} cx="12" cy="12" r="9" /><path {...common} d="M3.5 12h17M12 3c2.4 2.5 3.5 5.5 3.5 9s-1.1 6.5-3.5 9c-2.4-2.5-3.5-5.5-3.5-9S9.6 5.5 12 3Z" /></>}
  </svg>;
}

function AppearanceInfo({ labels }: { labels: (key: string, sourceFallback?: string, values?: Record<string, string | number>) => string }) {
  return <section className="page"><p className="eyebrow">{labels('shell.appearance.eyebrow')}</p><h1>{labels('shell.nav.settings')}</h1>
    <p className="lead">{labels('shell.appearance.lead')}</p>
    <div className="panel script-fixtures"><h2>{labels('shell.appearance.samples')}</h2>
      <p lang="lo">ສະບາຍດີ · ຂໍ້ມູນຕົວຢ່າງ (ຕົວຢ່າງສັງເຄາະ)</p>
      <p lang="en">English · Synthetic sample only</p><p lang="th">ภาษาไทย · ตัวอย่างสังเคราะห์</p>
      <p lang="vi">Tiếng Việt · Dữ liệu tổng hợp</p><p lang="hi-IN">हिन्दी · कृत्रिम उदाहरण</p>
      <p lang="ar" dir="rtl">مثال اصطناعي · <bdi dir="ltr">00000000-0000-4000-8000-000000000042</bdi></p>
      <p className="help">{labels('shell.appearance.indiaHelp')}</p>
    </div></section>;
}
