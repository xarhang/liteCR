# liteCR FHIR R4 profile notes

The authenticated adapter currently exposes a deliberately narrow Patient read, exact identifier search, and configured create surface using FHIR R4 JSON (`4.0.1`). This note records the wire representations that liteCR owns; it is not a claim of compatibility with OpenCR or any external client implementation.

## Original name language and script

For each projected `HumanName`, liteCR uses the HL7 FHIR R4 Human Language extension when the source assertion has a BCP 47 language tag:

```json
{
  "url": "http://hl7.org/fhir/StructureDefinition/language",
  "valueCode": "lo"
}
```

FHIR R4 permits this extension on `HumanName`. It does not define a common HumanName extension for the source's script label. To keep script distinct from language and preserve the source assertion verbatim, liteCR defines this private extension:

```json
{
  "url": "urn:litecr:fhir:StructureDefinition/human-name-script",
  "valueString": "Lao"
}
```

The `urn:litecr:` identifier is liteCR-defined, not an HL7 canonical URL. Consumers that do not implement this extension may ignore it under ordinary FHIR extension handling and will then lack the explicit source script metadata; the original Unicode name remains in `HumanName.text`. External client interpretation and OpenCR compatibility remain unverified. A future published implementation guide may replace the private URN with a resolvable canonical URL without changing the stored source assertion.

## Scope and search

The FHIR client binding fixes one registry and organization per authenticated client. `Patient?identifier=system|value` performs exact authority/value lookup in that bound scope. The protected Registry API's public-ID/alias search additionally requires the request's registry and organization to match the client's grants. Neither endpoint runs demographic matching or widens scope based on user-supplied identifiers.

For read projection, the imported source path `birthDate` and liteCR path `birth_date` both map to FHIR `Patient.birthDate`; the stored source assertion is not rewritten. FHIR partial-date precision is retained (for example `1984-06` remains year-month, without an invented day).
