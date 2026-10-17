# Workspace Page Design

> This file is the product-specific override for the registry workspace. It replaces generic marketing and patient-portal layouts in the generated Master with an operational identity-registry interface.

## Purpose and visual direction

Use a calm, high-trust clinical operations workspace. Keep identity search, registration, review, and administration obvious and fast for trained staff. Use a data-dense Swiss layout with a deep teal navigation rail, cool near-white canvas, white work surfaces, restrained sea-glass accent, and semantic red/amber/green states. Avoid decorative medical imagery, invented patient statistics, sales-oriented hero sections, and marketing proof patterns.

## Typography and language

Use the existing local system font stack and script-specific Noto fallbacks. Do not fetch fonts at runtime. Keep body text at 16px or larger, use tabular/monospace styling only for machine identifiers, and allow labels to wrap naturally in Lao, Thai, Vietnamese, Hindi, Arabic, and longer locale strings. Keep directionality inherited from the active BCP 47 locale and isolate record identifiers with `bdi`/LTR.

## Layout

- Desktop: sticky top bar, 240–256px navigation rail, centered work area capped near 1472px.
- Tablet/mobile: move navigation above the work area, wrap links into two or three columns, and stack form actions without horizontal scrolling.
- Keep content forms and record history spacious enough for clinical data while allowing compact preference controls.
- Use clear page title, concise instruction, primary action, and content surface hierarchy; use no KPI or patient-count cards unless backed by live API values.

## Interaction and accessibility

- Every navigation item has a consistent, decorative inline SVG plus translated text; the accessible name comes from its translated label.
- Retain 44px targets, visible focus, skip navigation, semantic headings, form labels, inline errors, and keyboard operation.
- Use subtle 150–200ms color/shadow feedback without layout shifts; disable transitions when reduced motion is requested.
- Keep existing appearance settings and organization-owned theme tokens effective. High-contrast mode overrides the navigation rail colors and active state.
- Keep language search, editable BCP 47 locale, dark mode, density, and contrast controls in a compact native disclosure. Start it collapsed so the registry task remains the first focus of the page; show the active locale and color mode in the summary.

## Implementation palette

The navigation chrome uses deep teal `#153944` with sea-glass `#a7eee2`. Main work surfaces continue to use the existing theme tokens so organization branding and light/dark mode remain supported. The muted border is `#d8e3e5`; destructive, warning, and success status colors remain semantic tokens.
