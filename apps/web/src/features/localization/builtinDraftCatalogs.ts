type DraftCatalog = Record<string, string>;

const loaders: Record<string, () => Promise<{ default: DraftCatalog }>> = {
  lo: () => import('../../../../../packages/localization/catalogs/lo.json'),
  th: () => import('../../../../../packages/localization/catalogs/th.json'),
  vi: () => import('../../../../../packages/localization/catalogs/vi.json'),
  'hi-IN': () => import('../../../../../packages/localization/catalogs/hi-IN.json'),
};

export function hasBuiltInDraftCatalog(locale: string): boolean {
  return Object.hasOwn(loaders, locale);
}

export async function loadBuiltInDraftCatalog(locale: string): Promise<DraftCatalog | null> {
  const load = loaders[locale];
  return load ? (await load()).default : null;
}
