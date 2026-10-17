import { describe, expect, it } from 'vitest';
import { canonicalLocaleTag, missingPlaceholders, placeholdersIn, placeholdersMatch } from './validation';

describe('translation placeholder validation', () => {
  it('collects named placeholders without interpreting user text as markup', () => {
    expect([...placeholdersIn('Hello {person.name}; {count}!')]).toEqual(['person.name', 'count']);
  });
  it('reports source placeholders that a draft dropped', () => {
    expect(missingPlaceholders('Hello {name}, {count}', 'ສະບາຍດີ {name}')).toEqual(['count']);
  });
  it('accepts expanded translations that preserve all required placeholders', () => {
    expect(missingPlaceholders('Record {id}', 'Registro {id} actualizado')).toEqual([]);
  });
  it('requires an exact placeholder multiset, rejecting extras and changed repeat counts', () => {
    expect(placeholdersMatch('Hello {name}', 'ສະບາຍດີ {name}')).toBe(true);
    expect(placeholdersMatch('Hello {name}', 'ສະບາຍດີ {name} {unknown}')).toBe(false);
    expect(placeholdersMatch('Hello {name} {name}', 'ສະບາຍດີ {name}')).toBe(false);
  });
});

describe('locale tag validation', () => {
  it('canonicalizes valid BCP 47 tags including script and Unicode extensions', () => {
    expect(canonicalLocaleTag(' EN-us ')).toBe('en-US');
    expect(canonicalLocaleTag('zh-hant-tw-u-nu-hanidec')).toBe('zh-Hant-TW-u-nu-hanidec');
    expect(canonicalLocaleTag('hi-IN')).toBe('hi-IN');
    expect(canonicalLocaleTag('IW')).toBe('iw');
  });

  it.each(['', 'lo_LA', 'en--US', 'en-u', 'abcdefghi', 'a'.repeat(65)])(
    'rejects malformed or oversized locale tag %s',
    (tag) => expect(canonicalLocaleTag(tag)).toBeNull(),
  );
});
