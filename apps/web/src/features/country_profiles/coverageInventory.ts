import { REGION_CODES, regionOptions } from './regions';

export type ProfileCoverage = {
  country_code: string;
  latest_version: number;
  active_version: number | null;
};

export type CoverageRow = {
  code: string;
  label: string;
  status: 'unconfigured' | 'draft' | 'published';
  latestVersion: number | null;
  activeVersion: number | null;
  pendingDraft: boolean;
  custom: boolean;
};

export function buildCoverageInventory(items: ProfileCoverage[], locale: string): CoverageRow[] {
  const names = new Map(regionOptions(locale).map(({ code, label }) => [code, label]));
  const byCode = new Map(items.map((item) => [item.country_code.toUpperCase(), item]));
  const codes = new Set([...REGION_CODES, ...byCode.keys()]);
  const collator = new Intl.Collator(locale || 'en', { sensitivity: 'base' });
  return [...codes].map((code) => {
    const item = byCode.get(code);
    const custom = !REGION_CODES.includes(code);
    return {
      code,
      label: names.get(code) ?? code,
      status: !item ? 'unconfigured' : item.active_version !== null ? 'published' : 'draft',
      latestVersion: item?.latest_version ?? null,
      activeVersion: item?.active_version ?? null,
      pendingDraft: Boolean(item && item.latest_version > (item.active_version ?? 0)),
      custom,
    } satisfies CoverageRow;
  }).sort((a, b) => collator.compare(a.label, b.label) || a.code.localeCompare(b.code));
}
