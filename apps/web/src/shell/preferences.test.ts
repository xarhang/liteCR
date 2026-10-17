import { describe, expect, it } from 'vitest';
import { createUiTranslator } from '../../../../packages/localization/src/webCatalogPreview';
import { directionFor, parsePreference, resolveColorMode } from './preferences';

describe('locale and preference behavior', () => {
  it('keeps direction independent from appearance and recognizes common RTL language tags', () => {
    expect(directionFor('lo')).toBe('ltr');
    expect(directionFor('ar-EG')).toBe('rtl');
    expect(directionFor('ur')).toBe('rtl');
  });
  it('derives direction from explicit or likely script for less common RTL locales', () => {
    expect(directionFor('ps-AF')).toBe('rtl');
    expect(directionFor('ug-CN')).toBe('rtl');
    expect(directionFor('sd-PK')).toBe('rtl');
    expect(directionFor('dv-MV')).toBe('rtl');
    expect(directionFor('pa-Arab')).toBe('rtl');
    expect(directionFor('wo-Gara')).toBe('rtl');
    expect(directionFor('ar-Latn')).toBe('ltr');
    expect(directionFor('pa-IN')).toBe('ltr');
    expect(directionFor('not_a_locale')).toBe('ltr');
  });
  it('uses system color mode only when the user selected system', () => {
    expect(resolveColorMode('system', true)).toBe('dark');
    expect(resolveColorMode('system', false)).toBe('light');
    expect(resolveColorMode('light', true)).toBe('light');
  });
  it('uses a safe default for unknown settings and falls back to the English source label', () => {
    expect(parsePreference({ color_mode: 'neon' as never, density: 'dense' as never, high_contrast: true }).color_mode).toBe('system');
    expect(parsePreference({ locale_tag: 'ta' }).locale_tag).toBe('ta');
    expect(createUiTranslator(undefined)('shell.nav.search')).toBe('Search');
  });
});
