export type ColorMode = 'light' | 'dark' | 'system';
export type Density = 'comfortable' | 'compact';
export type Preferences = { color_mode: ColorMode; density: Density; high_contrast: boolean; locale_tag: string };
const rtlScripts = new Set(['Adlm', 'Arab', 'Gara', 'Hebr', 'Mand', 'Mend', 'Nkoo', 'Rohg', 'Samr', 'Syrc', 'Thaa']);

export function parsePreference(value: Partial<Preferences> | null | undefined): Preferences {
  return {
    color_mode: value?.color_mode === 'light' || value?.color_mode === 'dark' ? value.color_mode : 'system',
    density: value?.density === 'compact' ? 'compact' : 'comfortable',
    high_contrast: value?.high_contrast === true,
    locale_tag: typeof value?.locale_tag === 'string' ? value.locale_tag : 'en',
  };
}

export function directionFor(locale: string): 'ltr' | 'rtl' {
  // Direction belongs to the writing system, not only to a short list of
  // language subtags. BCP 47 tags can explicitly select a script (for example
  // pa-Arab), while Intl.Locale supplies the likely script for common tags
  // without one (for example ps-AF or dv-MV).
  try {
    const parsed = new Intl.Locale(locale);
    const script = parsed.script ?? parsed.maximize().script;
    return script && rtlScripts.has(script) ? 'rtl' : 'ltr';
  } catch {
    // Malformed/unsupported tags should not break rendering. Preserve the
    // established fallback for the common RTL language tags.
    return /^(ar|fa|he|ur)(-|$)/i.test(locale) ? 'rtl' : 'ltr';
  }
}

export function resolveColorMode(mode: ColorMode, systemDark: boolean): 'light' | 'dark' {
  return mode === 'system' ? (systemDark ? 'dark' : 'light') : mode;
}
