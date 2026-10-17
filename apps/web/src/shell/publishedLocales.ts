export type PublishedLocale = { tag: string; direction: 'ltr' | 'rtl'; enabled?: boolean };
export type PublishedLocaleOption = PublishedLocale & { label: string; searchText: string };

/** Build searchable names from the browser's CLDR data; catalog labels remain owner-reviewed. */
export function publishedLocaleOptions(locales: PublishedLocale[], displayLocale: string): PublishedLocaleOption[] {
  let displayNames: Intl.DisplayNames | undefined;
  let collator: Intl.Collator;
  try {
    displayNames = new Intl.DisplayNames([displayLocale || 'en'], { type: 'language', fallback: 'code' });
    collator = new Intl.Collator(displayLocale || 'en');
  } catch {
    displayNames = new Intl.DisplayNames(['en'], { type: 'language', fallback: 'code' });
    collator = new Intl.Collator('en');
  }
  return locales
    .filter((locale) => locale.enabled !== false)
    .map((locale) => {
      const name = displayNames?.of(locale.tag) || locale.tag;
      const label = `${name} (${locale.tag})`;
      return { ...locale, label, searchText: fold(`${name} ${locale.tag}`) };
    })
    .sort((a, b) => collator.compare(a.label, b.label) || a.tag.localeCompare(b.tag));
}

/** Match either the locale tag or its browser-localized language name. */
export function searchPublishedLocales(options: PublishedLocaleOption[], query: string): PublishedLocaleOption[] {
  const needle = fold(query.trim());
  return needle ? options.filter((option) => option.searchText.includes(needle)) : options;
}

function fold(value: string): string {
  return value.normalize('NFKD').replace(/\p{M}/gu, '').toLocaleLowerCase();
}
