# Theme and interface requirements

The interface uses semantic theme tokens with light/dark/system mode, contrast and density preferences. These requirements guide changes; they are not a claim of human accessibility review or complete localization. See [Beta limits](BETA.md).

## Language and workflow

Use short, concrete text that names the action, outcome and next step. Distinguish unsaved data, an uncertain response after saving, and a case needing review. Preserve entered data where safe and place useful errors beside the relevant field. Matching is advisory evidence, not a verified identity.

Keep source names in their original script. Interface locale, country profile, calendar, timezone and appearance are independent; changing language never changes matching or permissions. Translation publication requires review of meaning, placeholders and workflow text. Do not imply that draft catalogs have passed human review.

## Tokens and preferences

Components use semantic colors, spacing, type and state tokens. Resolve explicit user preferences before organization and application defaults; system mode follows the device preference. Provide reset and safe fallback behavior.

Colors supplement text and icons. Maintain consistent success, warning and danger meanings. Brand configuration is bounded data, not arbitrary uploaded CSS, HTML or JavaScript. Validate assets and color combinations before publication. Preference storage must not contain patient data.

## Screen requirements

| Workflow | Requirements |
|---|---|
| Search/register | Scoped results, original names and source identifiers; distinguish no match from insufficient evidence. |
| Person detail | Stable public ID, aliases, source assertions and provenance. |
| Review | Readable side-by-side or stacked comparison; separate original values, normalized values and conflicts. |
| Identity decision | Explain consequences, require versions/reason, and wait for committed success. |
| Configuration | Distinguish draft, reviewed and published versions; enforce permissions on the server. |

## Accessibility and script handling

Target WCAG 2.2 AA. Check actual component/state contrast: normal text at least 4.5:1, qualifying large text at least 3:1, and applicable controls/graphics at least 3:1. Targets alone do not establish conformance.

Maintain keyboard navigation, visible focus, dialog escape/focus handling, screen-reader labels and errors, zoom/reflow, forced colors and reduced motion. Automated checks supplement manual workflow review.

Use logical layout properties for RTL. Isolate identifiers and phone numbers where direction differs. Check Lao/Thai marks, Vietnamese diacritics, Devanagari and other supported script combinations at realistic widths. Avoid clipping marks or splitting grapheme clusters; provide access to complete names and IDs. Verify licensed fonts and actual glyph coverage.

Check long translations and empty, loading, error, disabled and selected states in each supported mode. Print/export layouts must retain identifiers and source context. Readability and completion of registry tasks take precedence over decorative animation.
