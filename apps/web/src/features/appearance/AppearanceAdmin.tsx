import { useEffect, useState, type FormEvent } from 'react';
import { api, ApiFailure } from '../../shell/api';
import { apiFailureCopy } from '../../shell/apiFailureCopy';
import { contrastRatio, isHexColor } from './contrast';
import '../localization/admin.css';

type Labels = (key: string, sourceFallback?: string, values?: Record<string, string | number>) => string;
type Props = { registryId: string; organizationId: string; labels: Labels };
type Tokens = { canvas: string; surface: string; text: string; text_muted: string; action: string; on_action: string; control_border: string; focus: string };
type Profile = { name: string; accent: string; on_accent: string; logo_asset_ref: string | null; color_mode: 'light' | 'dark' | 'system'; density: 'comfortable' | 'compact'; high_contrast: boolean; tokens: Tokens; version: number };
type Preference = { color_mode: Profile['color_mode'] | null; density: Profile['density'] | null; high_contrast: boolean | null; locale_tag: string | null };
const defaultTokens: Tokens = { canvas: '#F8FAFC', surface: '#FFFFFF', text: '#0F172A', text_muted: '#475569', action: '#0F766E', on_action: '#FFFFFF', control_border: '#64748B', focus: '#1D4ED8' };
const segment = (value: string) => encodeURIComponent(value.trim());

export function AppearanceAdmin({ registryId, organizationId, labels }: Props) {
  const root = `/api/v1/config/${segment(registryId)}/${segment(organizationId)}`;
  const [profile, setProfile] = useState<Profile>(() => ({ name: labels('appearanceAdmin.defaultName'), accent: '#0F766E', on_accent: '#FFFFFF', logo_asset_ref: null, color_mode: 'system', density: 'comfortable', high_contrast: false, tokens: defaultTokens, version: 1 }));
  const [active, setActive] = useState<Profile | null>(null);
  const [pref, setPref] = useState<Preference>({ color_mode: null, density: null, high_contrast: null, locale_tag: null });
  const [savedDraftVersion, setSavedDraftVersion] = useState<number | null>(null);
  const [rollbackVersion, setRollbackVersion] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [status, setStatus] = useState('');
  useEffect(() => {
    if (!registryId || !organizationId) return;
    setError('');
    void api<Profile | null>(root + '/appearance').then((item) => { if (item) { setActive(item); setProfile(item); } }).catch((e: unknown) => setError(msg(e, labels)));
    void api<Preference | null>(root + '/appearance/preference').then((item) => { if (item) setPref(item); }).catch((e: unknown) => setError(msg(e, labels)));
  }, [root]);
  const contrast = { text: contrastRatio(profile.tokens.text, profile.tokens.canvas), muted: contrastRatio(profile.tokens.text_muted, profile.tokens.canvas), action: contrastRatio(profile.tokens.on_action, profile.tokens.action), border: contrastRatio(profile.tokens.control_border, profile.tokens.surface), focus: contrastRatio(profile.tokens.focus, profile.tokens.surface), brand: contrastRatio(profile.on_accent, profile.accent) };
  const validColors = Object.values(profile.tokens).every(isHexColor) && isHexColor(profile.accent) && isHexColor(profile.on_accent);
  const invalid = !profile.name.trim() || !validColors || contrast.text < 4.5 || contrast.muted < 4.5 || contrast.action < 4.5 || contrast.border < 3 || contrast.focus < 3 || contrast.brand < 4.5;
  const payload = () => ({ ...profile, expected_version: active?.version ?? null });
  async function saveDraft(event: FormEvent) {
    event.preventDefault(); if (invalid) return;
    setBusy(true); setError(''); setStatus('');
    try { await api<void>(root + '/appearance', { method: 'PUT', body: JSON.stringify(payload()) }); setSavedDraftVersion((active?.version ?? 0) + 1); setStatus(labels('appearanceAdmin.saved')); }
    catch (e) { setError(msg(e, labels)); } finally { setBusy(false); }
  }
  async function publish() {
    if (!savedDraftVersion) { setError(labels('appearanceAdmin.saveBeforePublish')); return; }
    setBusy(true); setError(''); setStatus('');
      try { const item = await api<Profile>(root + '/appearance/publish', { method: 'POST', body: JSON.stringify({ expected_version: active?.version ?? 0 }) }); setActive(item); setProfile(item); setSavedDraftVersion(null); setStatus(labels('appearanceAdmin.themePublished', undefined, { version: item.version })); }
    catch (e) { setError(msg(e, labels)); } finally { setBusy(false); }
  }
  async function rollback() {
    const target_version = Number(rollbackVersion);
    if (!active || !Number.isInteger(target_version) || target_version < 1 || target_version === active.version) { setError(labels('appearanceAdmin.validPriorVersion')); return; }
    setBusy(true); setError(''); setStatus('');
    try { const item = await api<Profile>(root + '/appearance/rollback', { method: 'POST', body: JSON.stringify({ target_version, expected_version: active.version }) }); setActive(item); setProfile(item); setStatus(labels('appearanceAdmin.rollbackPublished', undefined, { version: item.version })); }
    catch (e) { setError(msg(e, labels)); } finally { setBusy(false); }
  }
  async function savePreference() {
    setBusy(true); setError(''); setStatus('');
    try { await api<void>(root + '/appearance/preference', { method: 'PUT', body: JSON.stringify(pref) }); setStatus(labels('appearanceAdmin.preferenceSaved')); }
    catch (e) { setError(msg(e, labels)); } finally { setBusy(false); }
  }
  function changeToken(field: keyof Tokens, value: string) { setProfile((current) => ({ ...current, tokens: { ...current.tokens, [field]: value.toUpperCase() } })); setSavedDraftVersion(null); }
  function set<K extends keyof Profile>(field: K, value: Profile[K]) { setProfile((current) => ({ ...current, [field]: value })); setSavedDraftVersion(null); }
  return <section className="admin-page" aria-labelledby="appearance-title">
    <header className="admin-heading"><div><p className="eyebrow">{labels('appearanceAdmin.eyebrow')}</p><h1 id="appearance-title">{labels('appearanceAdmin.title')}</h1><p className="lead">{labels('appearanceAdmin.lead')}</p></div><span className="admin-badge">{active ? labels('appearanceAdmin.publishedVersion', undefined, { version: active.version }) : labels('appearanceAdmin.noPublishedTheme')}</span></header>
    <form className="admin-grid" onSubmit={(e) => void saveDraft(e)} aria-busy={busy}>
      <section className="admin-card"><h2>{labels('appearanceAdmin.brandIdentity')}</h2><div className="admin-form"><label>{labels('appearanceAdmin.displayName')}<input value={profile.name} maxLength={96} onChange={(e) => set('name', e.target.value)} required /></label><div className="two-col"><label>{labels('appearanceAdmin.accentColor')}<input value={profile.accent} pattern="#[0-9A-Fa-f]{6}" onChange={(e) => set('accent', e.target.value.toUpperCase())} aria-describedby="brand-contrast" /></label><label>{labels('appearanceAdmin.textOnAccent')}<input value={profile.on_accent} pattern="#[0-9A-Fa-f]{6}" onChange={(e) => set('on_accent', e.target.value.toUpperCase())} /></label></div><label>{labels('appearanceAdmin.logoReference')}<input value={profile.logo_asset_ref ?? ''} maxLength={128} pattern="[A-Za-z0-9_.-]*" onChange={(e) => set('logo_asset_ref', e.target.value || null)} placeholder={labels('appearanceAdmin.logoPlaceholder')} /></label><p className="help">{labels('appearanceAdmin.logoHelp')}</p></div></section>
      <section className="admin-card"><h2>{labels('appearanceAdmin.semanticTokens')}</h2><div className="token-grid">{(Object.keys(profile.tokens) as Array<keyof Tokens>).map((name) => <label key={name}>{labels('appearanceAdmin.token.' + name, name.replaceAll('_', ' '))}<input value={profile.tokens[name]} pattern="#[0-9A-Fa-f]{6}" onChange={(e) => changeToken(name, e.target.value)} /></label>)}</div><ul className="contrast-list" aria-label={labels('appearanceAdmin.contrastResults')}><li>{labels('appearanceAdmin.canvasText')}: {contrast.text.toFixed(2)}:1 {contrast.text >= 4.5 ? labels('appearanceAdmin.pass') : labels('appearanceAdmin.belowRatio', undefined, { ratio: '4.5' })}</li><li>{labels('appearanceAdmin.mutedText')}: {contrast.muted.toFixed(2)}:1 {contrast.muted >= 4.5 ? labels('appearanceAdmin.pass') : labels('appearanceAdmin.belowRatio', undefined, { ratio: '4.5' })}</li><li>{labels('appearanceAdmin.actionPair')}: {contrast.action.toFixed(2)}:1 {contrast.action >= 4.5 ? labels('appearanceAdmin.pass') : labels('appearanceAdmin.belowRatio', undefined, { ratio: '4.5' })}</li><li>{labels('appearanceAdmin.controlBorder')}: {contrast.border.toFixed(2)}:1 {contrast.border >= 3 ? labels('appearanceAdmin.pass') : labels('appearanceAdmin.belowRatio', undefined, { ratio: '3' })}</li><li>{labels('appearanceAdmin.focusIndicator')}: {contrast.focus.toFixed(2)}:1 {contrast.focus >= 3 ? labels('appearanceAdmin.pass') : labels('appearanceAdmin.belowRatio', undefined, { ratio: '3' })}</li><li>{labels('appearanceAdmin.brandPair')}: {contrast.brand.toFixed(2)}:1 {contrast.brand >= 4.5 ? labels('appearanceAdmin.pass') : labels('appearanceAdmin.belowRatio', undefined, { ratio: '4.5' })}</li></ul></section>
      <section className="admin-card"><h2>{labels('appearanceAdmin.defaults')}</h2><div className="admin-form"><label>{labels('appearanceAdmin.colorMode')}<select value={profile.color_mode} onChange={(e) => set('color_mode', e.target.value as Profile['color_mode'])}><option value="system">{labels('shell.option.system')}</option><option value="light">{labels('shell.option.light')}</option><option value="dark">{labels('shell.option.dark')}</option></select></label><label>{labels('appearanceAdmin.density')}<select value={profile.density} onChange={(e) => set('density', e.target.value as Profile['density'])}><option value="comfortable">{labels('shell.option.comfortable')}</option><option value="compact">{labels('shell.option.compact')}</option></select></label><label className="check-field"><input type="checkbox" checked={profile.high_contrast} onChange={(e) => set('high_contrast', e.target.checked)} /> {labels('appearanceAdmin.highContrastMode')}</label><p className="help">{labels('appearanceAdmin.themeHelp')}</p></div></section>
      <section className="admin-card"><h2>{labels('appearanceAdmin.livePreview')}</h2><div className="brand-preview" style={{ background: profile.tokens.canvas, color: profile.tokens.text, borderColor: profile.tokens.control_border }}><strong>{profile.name || labels('appearanceAdmin.organizationName')}</strong><p>ສະບາຍດີ · Tiếng Việt · हिन्दी · <bdi dir="rtl">العربية 00042</bdi></p><button type="button" style={{ background: profile.tokens.action, color: profile.tokens.on_action, outlineColor: profile.tokens.focus }}>{labels('appearanceAdmin.previewAction')}</button></div><p className="help">{labels('appearanceAdmin.previewHelp')}</p></section>
      <section className="admin-card"><h2>{labels('appearanceAdmin.themeRelease')}</h2><p>{labels('appearanceAdmin.currentVersion')} {active?.version ?? labels('appearanceAdmin.noVersion')}</p><p>{labels('appearanceAdmin.savedDraftVersion')} {savedDraftVersion ?? labels('appearanceAdmin.noVersion')}</p><div className="button-row"><button className="button" type="submit" disabled={busy || invalid}>{labels('appearanceAdmin.saveDraft')}</button><button className="button button-primary" type="button" disabled={busy || !savedDraftVersion} onClick={() => void publish()}>{labels('appearanceAdmin.publishTheme')}</button></div><label>{labels('appearanceAdmin.rollbackTarget')}<input type="number" min="1" value={rollbackVersion} onChange={(e) => setRollbackVersion(e.target.value)} /></label><button className="button" type="button" disabled={busy || !active} onClick={() => void rollback()}>{labels('appearanceAdmin.publishRollback')}</button>{invalid && <p className="admin-warning" role="alert">{labels('appearanceAdmin.invalidWarning')}</p>}</section>
      <section className="admin-card"><h2>{labels('appearanceAdmin.myPreferences')}</h2><p className="help">{labels('appearanceAdmin.preferencesHelp')}</p><div className="admin-form"><label>{labels('appearanceAdmin.languagePreference')}<input value={pref.locale_tag ?? ''} onChange={(e) => setPref({ ...pref, locale_tag: e.target.value || null })} pattern="[A-Za-z]{2,8}(-[A-Za-z0-9]{1,8})*" placeholder={labels('appearanceAdmin.languagePlaceholder')} /></label><label>{labels('appearanceAdmin.colorPreference')}<select value={pref.color_mode ?? ''} onChange={(e) => setPref({ ...pref, color_mode: (e.target.value || null) as Preference['color_mode'] })}><option value="">{labels('appearanceAdmin.useOrganizationDefault')}</option><option value="system">{labels('shell.option.system')}</option><option value="light">{labels('shell.option.light')}</option><option value="dark">{labels('shell.option.dark')}</option></select></label><label>{labels('appearanceAdmin.densityPreference')}<select value={pref.density ?? ''} onChange={(e) => setPref({ ...pref, density: (e.target.value || null) as Preference['density'] })}><option value="">{labels('appearanceAdmin.useOrganizationDefault')}</option><option value="comfortable">{labels('shell.option.comfortable')}</option><option value="compact">{labels('shell.option.compact')}</option></select></label><label>{labels('appearanceAdmin.contrastPreference')}<select value={pref.high_contrast == null ? '' : pref.high_contrast ? 'high' : 'standard'} onChange={(e) => setPref({ ...pref, high_contrast: e.target.value === '' ? null : e.target.value === 'high' })}><option value="">{labels('appearanceAdmin.useOrganizationDefault')}</option><option value="standard">{labels('shell.option.standard')}</option><option value="high">{labels('shell.option.highContrast')}</option></select></label><button className="button" type="button" disabled={busy} onClick={() => void savePreference()}>{labels('appearanceAdmin.savePreferences')}</button><p className="help">{labels('appearanceAdmin.resolutionHelp')}</p></div></section>
    </form>
    {error && <p className="alert alert-error" role="alert">{error}</p>}{(busy || status) && <p className="inline-status" role="status" aria-live="polite">{busy ? labels('review.waitingForServer', 'Waiting for server…') : status}</p>}
  </section>;
}
function msg(e: unknown, labels: Labels) {
  if (e instanceof ApiFailure) {
    const translated = apiFailureCopy(e, labels);
    if (translated) return translated;
    if (e.status === 0) return labels('appearanceAdmin.error.network', 'The service could not be reached. Check the connection and retry.');
    if (e.status === 401) return labels('appearanceAdmin.error.unauthenticated', 'Your session has expired. Sign in again to continue.');
    if (e.status === 403 || e.status === 404) return labels('appearanceAdmin.error.scope', 'This action is not available in the current access scope.');
    if (e.status === 409) return labels('appearanceAdmin.error.conflict', 'The theme or preference changed since it was loaded. Refresh and review before retrying.');
    if (e.status >= 500) return labels('appearanceAdmin.error.unavailable', 'The service is temporarily unavailable. Your changes were not confirmed.');
    // Preserve validation details returned for this request; they are not UI copy.
    return e.message;
  }
  return e instanceof Error ? e.message : labels('appearanceAdmin.requestError');
}
