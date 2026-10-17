import './tokens.css';
export type SurfaceMode = 'light' | 'dark';
export type Contrast = 'standard' | 'high';
export type Density = 'comfortable' | 'compact';
export function applyAppearance(mode: SurfaceMode, contrast: Contrast, density: Density): void {
  const root = document.documentElement;
  root.dataset.mode = mode; root.dataset.contrast = contrast; root.dataset.density = density;
  root.style.colorScheme = mode;
}
export function applyLocale(locale: string, direction: 'ltr' | 'rtl'): void {
  document.documentElement.lang = locale; document.documentElement.dir = direction;
}
