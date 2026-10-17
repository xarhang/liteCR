import { describe, expect, it } from 'vitest';
import { publishedLocaleOptions, searchPublishedLocales } from './publishedLocales';

describe('published locale discovery', () => {
  const available = publishedLocaleOptions([
    { tag: 'en', direction: 'ltr' },
    { tag: 'hi-IN', direction: 'ltr' },
    { tag: 'ar', direction: 'rtl' },
    { tag: 'fr-CA', direction: 'ltr', enabled: false },
  ], 'en');

  it('shows enabled published locales by language name and tag, excluding disabled catalogs', () => {
    expect(available.map((option) => option.tag)).toEqual(expect.arrayContaining(['en', 'hi-IN', 'ar']));
    expect(available.some((option) => option.tag === 'fr-CA')).toBe(false);
    expect(searchPublishedLocales(available, 'Hindi').map((option) => option.tag)).toEqual(['hi-IN']);
    expect(searchPublishedLocales(available, 'HI-in').map((option) => option.tag)).toEqual(['hi-IN']);
    expect(searchPublishedLocales(available, 'Arabic').map((option) => option.tag)).toEqual(['ar']);
  });

  it('returns all published options for a blank search and no options for an unmatched query', () => {
    expect(searchPublishedLocales(available, '  ')).toEqual(available);
    expect(searchPublishedLocales(available, 'not-a-language')).toEqual([]);
  });
});
