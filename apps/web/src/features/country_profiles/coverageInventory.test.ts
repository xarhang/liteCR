import { describe, expect, it } from 'vitest';
import { REGION_CODES } from './regions';
import { buildCoverageInventory } from './coverageInventory';

describe('country profile coverage inventory', () => {
  it('lists every regular CLDR region and includes custom codes with accurate saved-state distinctions', () => {
    const rows = buildCoverageInventory([
      { country_code: 'LA', latest_version: 4, active_version: 3 },
      { country_code: 'IN', latest_version: 2, active_version: 2 },
      { country_code: 'ZZ', latest_version: 1, active_version: null },
    ], 'en');
    const byCode = new Map(rows.map((row) => [row.code, row]));

    expect(rows.filter((row) => !row.custom)).toHaveLength(REGION_CODES.length);
    expect(rows.map((row) => row.code)).toContain('ZZ');
    expect(byCode.get('LA')).toMatchObject({ status: 'published', latestVersion: 4, activeVersion: 3, pendingDraft: true });
    expect(byCode.get('IN')).toMatchObject({ status: 'published', latestVersion: 2, activeVersion: 2 });
    expect(byCode.get('ZZ')).toMatchObject({ status: 'draft', custom: true, activeVersion: null, pendingDraft: true });
    expect(byCode.get('ZZ')?.label).toBe('ZZ');
    expect(byCode.get('TH')).toMatchObject({ status: 'unconfigured', custom: false, latestVersion: null });
  });
});
