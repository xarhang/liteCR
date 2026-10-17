# liteCR web

React + TypeScript + Vite single-page application. Build with npm ci and npm run build; generated static assets are written to apps/web/dist for embedding in the native release. API calls use relative same-origin /api/v1 paths, secure cookies and cache: no-store.

The search endpoint currently accepts only an exact public ID or alias. This UI does not implement demographic matching. Patient responses live only in React component state and are discarded when a screen unmounts or reloads. localStorage is limited to the nonsensitive registry/organization context identifiers.

For authenticated end-to-end verification, run the integrated SPA/API origin (or Vite preview with LITECR_API_PROXY_TARGET pointing at the development API), then set LITECR_E2E_BASE_URL, LITECR_E2E_STORAGE_STATE, LITECR_E2E_REGISTRY_ID, and LITECR_E2E_ORGANIZATION_ID to an isolated ephemeral synthetic environment and run npm run test:e2e. The workflow creates a synthetic source record and must not be pointed at production or a shared operational database. LITECR_E2E_APP_COMMAND may be provided for a locally managed app process.

The language field accepts a BCP 47 locale tag, so a newly published locale can be selected without rebuilding the SPA. The catalog bundle endpoint is queried for that locale; missing bundles display a clear fallback notice. Initial Lao/English/Thai/Vietnamese UI strings and Hindi/Arabic script samples are provisional. Hindi is one Devanagari example and is not full support for India’s languages. RTL is a layout fixture. Neither the sample strings nor script layout is a human translation, font-coverage, or accessibility approval.

The English source catalog covers the current statically referenced UI keys, and the translation manager can register missing built-in UI source messages without replacing existing source text. Run npm run check:catalog to verify source-key coverage and requested locale options. This does not publish translated locale bundles or establish human language, font, or accessibility approval.
