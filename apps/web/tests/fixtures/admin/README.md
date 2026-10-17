# Administrative browser-test fixture

These helpers provision disposable synthetic database/IdP state and independent actor sessions for the administrative Playwright specifications. They contain workstation-specific process and checkout assumptions and are not the public Beta setup procedure. Use [local Beta setup](../../../../../docs/BETA-SETUP.md) for ordinary evaluation.

The specifications can instead use an externally provisioned isolated fixture through the documented `LITECR_E2E_*` environment settings. Inspect `scope.ts` for required settings and validation. Do not use real accounts, shared databases or patient data. Session-storage files and credentials must remain outside Git.

Without a complete fixture, fixture-backed browser cases skip; skipped cases are not passing end-to-end evidence. Partially configured fixtures fail their preconditions. The helpers must be reviewed and adapted for the developer's environment before execution, including ownership-checked cleanup and loopback service bindings. They do not establish packaged-runtime, clean-host or production acceptance.
