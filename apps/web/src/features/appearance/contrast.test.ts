import { describe, expect, it } from 'vitest';
import { contrastRatio, isHexColor } from './contrast';

describe('appearance contrast checks', () => {
  it('uses WCAG relative luminance ratios for valid hex colors', () => {
    expect(contrastRatio('#000000', '#FFFFFF')).toBeCloseTo(21, 4);
    expect(contrastRatio('#FFFFFF', '#F8FAFC')).toBeLessThan(1.1);
  });
  it('rejects malformed colors instead of claiming a passing ratio', () => {
    expect(isHexColor('white')).toBe(false);
    expect(contrastRatio('red', '#FFFFFF')).toBe(0);
  });
});
