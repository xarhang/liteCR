// Derived from Unicode CLDR 48.2 common/validity/region.xml, the
// idStatus="regular" region identifiers restricted to two-letter codes.
// CLDR data is licensed under Unicode-3.0. This is a picker aid, not
// jurisdiction, residency, or legal validation; custom codes remain editable.
// https://github.com/unicode-org/cldr/blob/release-48-2/common/validity/region.xml
const CLDR_REGULAR_REGION_SPEC = `
AC~G AI AL~M AO AQ~U AW~X AZ BA~B BD~J BL~O BQ~T BV~W BY~Z
CA CC~D CF~I CK~R CU~Z DE DG DJ~K DM DO DZ EA EC EE EG~H ER~T
FI~K FM FO FR GA~B GD~I GL~N GP~U GW GY HK HM~N HR HT~U
IC~E IL~O IQ~T JE JM JO~P KE KG~I KM~N KP KR KW KY~Z
LA~C LI LK LR~V LY MA MC~H MK~Z NA NC NE~G NI NL NO~P NR NU NZ
OM PA PE~H PK~N PR~T PW PY QA RE RO RS RU RW SA~E SG~O SR~T SV SX~Z
TA TC~D TF~H TJ~O TR TT TV~W TZ UA UG UM US UY~Z VA VC VE VG VI VN VU
WF WS XK YE YT ZA ZM ZW`;

function expandRange(token: string): string[] {
  const [start, end] = token.split('~');
  if (!end) return [start];

  // CLDR validity ranges abbreviate the final code point (for example,
  // "AC~G" means AC, AD, AE, AF and AG).
  if (start.length === 2 && end.length === 1) {
    const codes: string[] = [];
    for (let letter = start.charCodeAt(1); letter <= end.charCodeAt(0); letter += 1) {
      codes.push(start[0] + String.fromCharCode(letter));
    }
    return codes;
  }

  throw new Error(`Unsupported CLDR region range: ${token}`);
}

export const REGION_CODES = [...new Set(CLDR_REGULAR_REGION_SPEC.trim().split(/\s+/).flatMap(expandRange))].sort();

export type RegionOption = { code: string; label: string };

export function regionOptions(locale: string): RegionOption[] {
  let displayName: Intl.DisplayNames | undefined;
  try {
    displayName = new Intl.DisplayNames([locale || 'en'], { type: 'region', fallback: 'code' });
  } catch {
    displayName = undefined;
  }

  let collator: Intl.Collator;
  try {
    collator = new Intl.Collator(locale || 'en', { sensitivity: 'base' });
  } catch {
    collator = new Intl.Collator('en', { sensitivity: 'base' });
  }

  return REGION_CODES.map((code) => {
    let name = code;
    try {
      name = displayName?.of(code) || code;
    } catch {
      // Keep the code visible if the browser's Intl data does not know it.
    }
    return { code, label: `${name} (${code})` };
  }).sort((a, b) => collator.compare(a.label, b.label) || a.code.localeCompare(b.code));
}
