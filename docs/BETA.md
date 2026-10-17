# Beta scope and known limits

This preview is for developers and operators evaluating liteCR with synthetic data in an isolated environment. It is not approved for real patient data, clinical decisions or production operation. The application version is 0.1.2 and the current database schema is 31; publishing a Beta tag or binary is a separate step.

## Available workflows

Configure an OIDC user and scoped application grants, register invented source records, search by public ID or alias, request matching, and review possible duplicates. Test denial of access outside the granted registry/organization. Matching does not silently merge people; review decisions retain version and assignment checks.

## Known limits

- Setup requires PostgreSQL, OIDC and manual user/grant provisioning. There is no first-administrator wizard or preconfigured production account.
- Person search accepts exact public IDs or aliases, not demographic queries. Matching is a separate workflow.
- Typo/partial-name and family/twin behavior has synthetic coverage. Representative real-population accuracy has not been established.
- Matching latency depends on candidate fanout and shared resources. Local load observations are not a production SLA.
- Four translated catalogs are drafts. Human language, accessibility and representative user reviews are incomplete.
- The FHIR adapter covers a narrow Patient surface. OpenCR and external-client interoperability are unverified.
- MCP, remote AI data access and a full application memory candidate index are not available.
- Clean-host installation and independent-host recovery need further validation. Windows source checks do not establish packaged Windows release support.
- Local security tests are not OWASP certification, complete ASVS coverage or validation of another deployment's IdP/TLS/cookie configuration.

## Before sharing a Beta candidate

1. Review and commit the intended source changes. Exclude credentials, runtime state, local archives and internal reports.
2. Build and test the exact candidate from a clean checkout. Check GitHub CI after pushing; a workflow definition is not a passing run.
3. Verify a fresh tester can complete setup, login, scoped access and a synthetic registration/search/review workflow.
4. Review source licensing, bundled assets and dependency notices before distributing binaries. Do not publish local review archives as release downloads.
5. Publish these limits and provide a way to report bugs with synthetic reproductions.

Production use additionally needs adjudicated matching evaluation, approved workloads/safety limits, actual deployment security checks, qualified user/language review, independent recovery and operational acceptance.

## Bug reports

Include version/commit, platform, reproduction steps, expected behavior and actual behavior. Use invented names and identifiers. Remove credentials, cookies, tokens, database URLs, exports and patient data. Follow [security reporting](../SECURITY.md) for suspected vulnerabilities.
