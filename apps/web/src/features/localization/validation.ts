export function placeholdersIn(text: string): Set<string> {
  return new Set([...text.matchAll(/\{([A-Za-z0-9_.-]+)\}/g)].map((match) => match[1]));
}

export function missingPlaceholders(source: string, translation: string): string[] {
  const translated = placeholdersIn(translation);
  return [...placeholdersIn(source)].filter((name) => !translated.has(name));
}

/** Ensure a draft uses the exact source placeholder multiset, including repeat counts. */
export function placeholdersMatch(source: string, translation: string): boolean {
  const tokens = (text: string) => [...text.matchAll(/\{([A-Za-z0-9_.-]+)\}/g)].map((match) => match[1]).sort();
  return JSON.stringify(tokens(source)) === JSON.stringify(tokens(translation));
}

/** Return one canonical BCP 47 tag, or null when it is invalid or too long. */
export function canonicalLocaleTag(input: string): string | null {
  const value = input.trim();
  if (!value || value.length > 64 || value.includes('_')) return null;
  try {
    Intl.getCanonicalLocales(value);
    const canonical = canonicalCasePreservingAliases(value);
    return canonical.length <= 64 ? canonical : null;
  } catch {
    return null;
  }
}

function canonicalCasePreservingAliases(tag: string): string {
  const subtags = tag.split('-');
  const canonical = [subtags[0].toLowerCase()];
  let inExtension = false;
  for (const subtag of subtags.slice(1)) {
    if (subtag.length === 1) {
      inExtension = true;
      canonical.push(subtag.toLowerCase());
    } else if (inExtension) {
      canonical.push(subtag.toLowerCase());
    } else if (/^[A-Za-z]{4}$/.test(subtag)) {
      canonical.push(subtag[0].toUpperCase() + subtag.slice(1).toLowerCase());
    } else if (/^(?:[A-Za-z]{2}|[0-9]{3})$/.test(subtag)) {
      canonical.push(subtag.toUpperCase());
    } else {
      canonical.push(subtag.toLowerCase());
    }
  }
  return canonical.join('-');
}
