export function isHexColor(value: string) { return /^#[0-9a-fA-F]{6}$/.test(value); }

export function contrastRatio(a: string, b: string) {
  if (!isHexColor(a) || !isHexColor(b)) return 0;
  const luminance = (hex: string) => {
    const rgb = [1, 3, 5].map((index) => parseInt(hex.slice(index, index + 2), 16) / 255)
      .map((value) => value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4);
    return .2126 * rgb[0] + .7152 * rgb[1] + .0722 * rgb[2];
  };
  const [lighter, darker] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (lighter + .05) / (darker + .05);
}
