import { describe, expect, it } from 'vitest';
import { REGION_CODES, regionOptions } from './regions';

describe('country and region suggestions', () => {
  it('includes the pinned CLDR regular two-letter region set', () => {
    expect(REGION_CODES).toHaveLength(257);
    expect(REGION_CODES).toContain('LA');
    expect(REGION_CODES).toContain('TH');
    expect(REGION_CODES).toContain('VN');
    expect(REGION_CODES).toContain('IN');
    expect(REGION_CODES).toContain('XK');
    expect(REGION_CODES.every((code) => /^[A-Z]{2}$/.test(code))).toBe(true);
    expect(new Set(REGION_CODES).size).toBe(REGION_CODES.length);
  });

  it('shows localized region names with codes for disambiguation', () => {
    const english = regionOptions('en');
    const englishLaos = english.find((option) => option.code === 'LA');

    expect(englishLaos?.label).toMatch(/\(LA\)$/);
    expect(englishLaos?.label.length).toBeGreaterThan(4);
    expect(english).toEqual([...english].sort((a, b) => a.label.localeCompare(b.label, 'en') || a.code.localeCompare(b.code)));

    for (const locale of ['lo', 'th', 'vi', 'hi-IN']) {
      const localized = regionOptions(locale).find((option) => option.code === 'LA');
      expect(localized?.label).toMatch(/\(LA\)$/);
      expect(localized?.label).not.toBe(englishLaos?.label);
    }
  });

  it('falls back to visible codes when an unsupported locale is supplied', () => {
    expect(regionOptions('not_a_locale').some((option) => option.label.includes('(LA)'))).toBe(true);
  });
});
