# Design System Master File

> **LOGIC:** When building a specific page, first check `design-system/pages/[page-name].md`.
> If that file exists, its rules **override** this Master file.
> If not, strictly follow the rules below.

---

**Project:** liteCR
**Updated:** 2026-10-11
**Category:** Clinical Operations / Identity Registry

---

## Global Rules

### Color Palette

| Role | Source of truth | Rule |
|------|----------------|------|
| Canvas, surface, text, muted text, border | liteCR semantic theme tokens | Respect user-selected light/dark/contrast mode. |
| Action, on-action, focus | liteCR semantic theme tokens | Keep focus and action distinguishable; validate every state for contrast. |
| Info, success, warning, danger | liteCR semantic status tokens | Pair color with text/icon labels; never rely on color alone. |

Do not hard-code a second palette in page components. Refer to `docs/THEMES.md`; proposed token values there still require rendered-state validation before release.

### Typography

- **Heading and body:** existing local system sans-serif stack with script-specific fallbacks.
- **Mood:** clear, dependable, calm, work-focused.
- **Script support:** retain Lao, Thai, Devanagari and platform fallback stacks; keep Unicode line-height and mark placement readable.

Do not use remote font imports or add a font download dependency. Any bundled font requires license and actual script-coverage review.

### Spacing Variables

| Token | Value | Usage |
|-------|-------|-------|
| `--space-xs` | `4px` / `0.25rem` | Tight gaps |
| `--space-sm` | `8px` / `0.5rem` | Icon gaps, inline spacing |
| `--space-md` | `16px` / `1rem` | Standard padding |
| `--space-lg` | `24px` / `1.5rem` | Section padding |
| `--space-xl` | `32px` / `2rem` | Large gaps |
| `--space-2xl` | `48px` / `3rem` | Section margins |
| `--space-3xl` | `64px` / `4rem` | Large separation when task content allows |

### Shadow Depths

| Level | Value | Usage |
|-------|-------|-------|
| `--shadow-sm` | `0 1px 2px rgba(0,0,0,0.05)` | Subtle lift |
| `--shadow-md` | restrained elevation | Use only when it clarifies layering. |
| `--shadow-lg` | reserved for overlays | Avoid decorative elevation on ordinary work surfaces. |
| `--shadow-xl` | not used for routine screens | No hero or conversion cards. |

---

## Component Specs

### Buttons

```css
/* Primary Button */
.btn-primary {
  background: var(--color-action);
  color: var(--color-on-action);
  padding: 12px 24px;
  border-radius: 8px;
  font-weight: 600;
  transition: background-color 160ms ease, border-color 160ms ease;
  cursor: pointer;
}

.btn-primary:hover {
  filter: brightness(0.96);
}

/* Secondary Button */
.btn-secondary {
  background: var(--color-surface);
  color: var(--color-text);
  border: 1px solid var(--color-border);
  padding: 12px 24px;
  border-radius: 8px;
  font-weight: 600;
  transition: background-color 160ms ease, border-color 160ms ease;
  cursor: pointer;
}
```

### Cards

```css
.card {
  background: var(--color-surface);
  border-radius: 12px;
  padding: 24px;
  box-shadow: var(--shadow-md);
  transition: border-color 160ms ease, box-shadow 160ms ease;
}

.card:hover {
  box-shadow: var(--shadow-lg);
}
```

### Inputs

```css
.input {
  padding: 12px 16px;
  border: 1px solid var(--color-border);
  border-radius: 8px;
  font-size: 16px;
  transition: border-color 200ms ease;
}

.input:focus {
  border-color: var(--color-focus);
  outline: 2px solid var(--color-focus);
  outline-offset: 2px;
}
```

### Modals

```css
.modal-overlay {
  background: color-mix(in srgb, var(--color-text) 45%, transparent);
}

.modal {
  background: var(--color-surface);
  color: var(--color-text);
  border-radius: 16px;
  padding: 32px;
  box-shadow: var(--shadow-xl);
  max-width: 500px;
  width: 90%;
}
```

---

## Style Guidelines

**Style:** Task-focused clinical operations; clear hierarchy, concise labels, readable evidence and calm semantic tokens.

**Keywords:** Clean, simple, spacious, functional, white space, high contrast, geometric, sans-serif, grid-based, essential

**Best For:** Registry staff completing search, registration, review, and administration workflows.

**Key Effects:** Subtle hover (200-250ms), smooth transitions, sharp shadows if any, clear type hierarchy, fast loading

### Page Pattern

**Pattern Name:** Registry workbench

- **Primary task:** Put the current registry scope and next action first.
- **Content order:** Page title/instruction > scoped task controls > results/evidence > provenance/history > secondary guidance.
- **Feedback:** Show exact field errors next to inputs and connect with `aria-describedby`; after failed submit, focus the error summary. Say whether data was saved or remains unconfirmed.
- **Responsive behavior:** Preserve task order, keyboard path and identifier visibility across widths; do not introduce horizontal page overflow.

---

## Anti-Patterns (Do NOT Use)

- ❌ patient-facing portal patterns, booking funnels or wellness imagery
- ❌ sales hero sections, testimonials, conversion CTAs, fabricated KPIs or decorative carousels
- ❌ low contrast, remote fonts, or claims of identity certainty / matching superiority without approved evidence

### Additional Forbidden Patterns

- ❌ **Emojis as icons** — Use SVG icons (Heroicons, Lucide, Simple Icons)
- ❌ **Missing cursor:pointer** — All clickable elements must have cursor:pointer
- ❌ **Layout-shifting hovers** — Avoid scale transforms that shift layout
- ❌ **Low contrast text** — Maintain 4.5:1 minimum contrast ratio
- ❌ **Instant state changes** — Always use transitions (150-300ms)
- ❌ **Invisible focus states** — Focus states must be visible for a11y

---

## Pre-Delivery Checklist

Before delivering any UI code, verify:

- [ ] No emojis used as icons (use SVG instead)
- [ ] All icons from consistent icon set (Heroicons/Lucide)
- [ ] `cursor-pointer` on all clickable elements
- [ ] Hover states with smooth transitions (150-300ms)
- [ ] Light mode: text contrast 4.5:1 minimum
- [ ] Focus states visible for keyboard navigation
- [ ] `prefers-reduced-motion` respected
- [ ] Responsive: 375px, 768px, 1024px, 1440px
- [ ] No content hidden behind fixed navbars
- [ ] No horizontal scroll on mobile
- [ ] Long Lao, Thai, Vietnamese, Hindi and RTL text keeps labels, values and actions understandable
- [ ] Errors appear beside their field, set `aria-invalid`, and are named by `aria-describedby`
- [ ] No patient data or matching decision is inferred by visual theme, locale or country profile
