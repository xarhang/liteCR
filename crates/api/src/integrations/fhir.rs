//! Narrow FHIR R4 adapter. Only read, exact identifier search and configured create are exposed.

use std::{collections::HashMap, sync::Arc};

use axum::{
    Extension, Json, Router,
    body::{Body, to_bytes},
    extract::{
        Path, Query, State,
        rejection::{PathRejection, QueryRejection},
    },
    http::{HeaderMap, HeaderValue, Request, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use litecr_application::{
    access::{AccessContext, ActorKind, Permission, ResourceScope, ScopeGrant},
    registry::{
        IdentifierAssertion, IdentifierView, PersonView, RegistryError, RegistryService,
        SourceAssertion, SourceRecordCommand,
    },
};
use serde::{Deserialize, Serialize};

const FHIR_JSON: &str = "application/fhir+json; charset=utf-8";
const FHIR_VERSION: &str = "4.0.1";
const MAX_RESOURCE_BYTES: usize = 1_048_576;
const MAX_SEARCH_RESULTS: u16 = 100;
const HUMAN_LANGUAGE_EXTENSION: &str = "http://hl7.org/fhir/StructureDefinition/language";
const LITECR_NAME_SCRIPT_EXTENSION: &str = "urn:litecr:fhir:StructureDefinition/human-name-script";

/// Server-owned integration binding. The map key is `issuer_url#subject`; use `#subject`
/// only when the trusted actor has no issuer configured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FhirClientBinding {
    pub registry_id: String,
    pub organization_id: String,
    pub source_system_id: String,
    pub create_enabled: bool,
}

#[derive(Clone)]
struct ApiState {
    registry: Arc<RegistryService>,
    clients: Arc<HashMap<String, FhirClientBinding>>,
}

/// Build the authenticated FHIR R4 adapter. The orchestrator must place this router behind
/// `protected_routes`; all data operations still call the ordinary scoped Registry service.
pub fn router(
    registry: Arc<RegistryService>,
    clients: HashMap<String, FhirClientBinding>,
) -> Router {
    Router::new()
        .route("/fhir/metadata", get(capability_statement))
        .route("/fhir/r4/metadata", get(capability_statement))
        .route("/fhir/r4/Patient", get(search_patient).post(create_patient))
        .route("/fhir/r4/Patient/{id}", get(read_patient))
        .route("/fhir/Patient", get(search_patient).post(create_patient))
        .route("/fhir/Patient/{id}", get(read_patient))
        .fallback(fhir_not_found)
        .method_not_allowed_fallback(fhir_method_not_allowed)
        .with_state(ApiState {
            registry,
            clients: Arc::new(clients),
        })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CapabilityStatement {
    resource_type: &'static str,
    status: &'static str,
    date: &'static str,
    kind: &'static str,
    fhir_version: &'static str,
    format: [&'static str; 1],
    rest: [RestCapability; 1],
}

#[derive(Debug, Serialize)]
struct RestCapability {
    mode: &'static str,
    resource: Vec<ResourceCapability>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ResourceCapability {
    #[serde(rename = "type")]
    resource_type: &'static str,
    interaction: Vec<Interaction>,
    search_param: Vec<SearchParameter>,
}

#[derive(Debug, Serialize)]
struct Interaction {
    code: &'static str,
}

#[derive(Debug, Serialize)]
struct SearchParameter {
    name: &'static str,
    #[serde(rename = "type")]
    search_type: &'static str,
    documentation: &'static str,
}

async fn capability_statement(
    Extension(context): Extension<AccessContext>,
    State(state): State<ApiState>,
) -> Result<Response, FhirError> {
    let binding = client_binding(&state, &context)?;
    let scope = ResourceScope {
        registry_id: binding.registry_id.clone(),
        organization_id: binding.organization_id.clone(),
        record_id: None,
    };
    let can_read = has_scoped_permission(&context, &scope, Permission::PersonRead);
    let can_search = has_scoped_permission(&context, &scope, Permission::PersonSearch);
    let can_create =
        binding.create_enabled && has_scoped_permission(&context, &scope, Permission::SourceWrite);

    let mut interactions = Vec::new();
    if can_read {
        interactions.push(Interaction { code: "read" });
    }
    if can_search {
        interactions.push(Interaction {
            code: "search-type",
        });
    }
    if can_create {
        interactions.push(Interaction { code: "create" });
    }
    let search_param = if can_search {
        vec![SearchParameter {
            name: "identifier",
            search_type: "token",
            documentation: "Exact identifier token search within the bound registry and organization. Use system|value to restrict assigning authority, or value alone to search all authorities in that scope; no demographic matching.",
        }]
    } else {
        Vec::new()
    };
    let resource = if interactions.is_empty() {
        Vec::new()
    } else {
        vec![ResourceCapability {
            resource_type: "Patient",
            interaction: interactions,
            search_param,
        }]
    };
    let body = CapabilityStatement {
        resource_type: "CapabilityStatement",
        status: "active",
        date: "2026-10-08",
        kind: "instance",
        fhir_version: FHIR_VERSION,
        format: ["json"],
        rest: [RestCapability {
            mode: "server",
            resource,
        }],
    };
    Ok(fhir_json((StatusCode::OK, Json(body))))
}

fn has_scoped_permission(
    context: &AccessContext,
    scope: &ResourceScope,
    permission: Permission,
) -> bool {
    context
        .grants()
        .iter()
        .any(|grant: &ScopeGrant| grant.permits_scope(scope) && grant.permits(permission))
}

#[derive(Debug, Deserialize)]
struct IdentifierQuery {
    identifier: String,
}

async fn search_patient(
    Extension(context): Extension<AccessContext>,
    State(state): State<ApiState>,
    query: Result<Query<IdentifierQuery>, QueryRejection>,
) -> Result<Response, FhirError> {
    let Query(query) = query.map_err(|_| FhirError::invalid_identifier())?;
    let (system, value) = parse_identifier_token(&query.identifier)?;
    let binding = client_binding(&state, &context)?;
    let people = match system {
        Some(system) => {
            state
                .registry
                .search_identifier(
                    Some(&context),
                    &binding.registry_id,
                    &binding.organization_id,
                    system,
                    value,
                    MAX_SEARCH_RESULTS,
                )
                .await
        }
        None => {
            state
                .registry
                .search_identifier_value(
                    Some(&context),
                    &binding.registry_id,
                    &binding.organization_id,
                    value,
                    MAX_SEARCH_RESULTS,
                )
                .await
        }
    }
    .map_err(FhirError::from_registry)?;
    if people.len() == usize::from(MAX_SEARCH_RESULTS) {
        return Err(FhirError::too_costly());
    }
    let entries = people
        .into_iter()
        .map(|person| {
            Ok(BundleEntry {
                resource: fhir_patient(person)?,
                search: SearchEntry { mode: "match" },
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let total = entries.len() as u32;
    Ok(fhir_json((
        StatusCode::OK,
        Json(SearchBundle {
            resource_type: "Bundle",
            bundle_type: "searchset",
            total,
            entry: entries,
        }),
    )))
}

fn parse_identifier_token(token: &str) -> Result<(Option<&str>, &str), FhirError> {
    let (system, value) = match token.split_once('|') {
        Some((system, value))
            if !system.is_empty()
                && !value.is_empty()
                && !system.contains('|')
                && !value.contains('|')
                && system.len() <= 2048
                && value.len() <= 512 =>
        {
            (Some(system), value)
        }
        Some(_) => return Err(FhirError::invalid_identifier()),
        None if !token.is_empty() && !token.contains('|') && token.len() <= 512 => (None, token),
        None => return Err(FhirError::invalid_identifier()),
    };
    Ok((system, value))
}

async fn read_patient(
    Extension(context): Extension<AccessContext>,
    State(state): State<ApiState>,
    id: Result<Path<String>, PathRejection>,
) -> Result<Response, FhirError> {
    let Path(id) = id.map_err(|_| FhirError::not_found())?;
    if !valid_fhir_id(&id) {
        return Err(FhirError::not_found());
    }
    let binding = client_binding(&state, &context)?;
    let person = state
        .registry
        .person(
            Some(&context),
            &binding.registry_id,
            &binding.organization_id,
            &id,
        )
        .await
        .map_err(FhirError::from_registry)?
        .ok_or_else(FhirError::not_found)?;
    Ok(fhir_json((StatusCode::OK, Json(fhir_patient(person)?))))
}

async fn create_patient(
    Extension(context): Extension<AccessContext>,
    State(state): State<ApiState>,
    request: Request<Body>,
) -> Result<Response, FhirError> {
    let (parts, body) = request.into_parts();
    let headers = parts.headers;
    if !is_fhir_json_request(&headers) {
        return Err(FhirError::invalid("FHIR_CONTENT_TYPE_INVALID"));
    }
    let payload = to_bytes(body, MAX_RESOURCE_BYTES)
        .await
        .map_err(|_| FhirError::too_large())?;
    let patient: PatientCreate = serde_json::from_slice(&payload)
        .map_err(|_| FhirError::invalid("FHIR_RESOURCE_INVALID"))?;
    if patient.resource_type != "Patient" {
        return Err(FhirError::invalid("FHIR_RESOURCE_TYPE_INVALID"));
    }
    let binding = client_binding(&state, &context)?;
    if context.actor().kind != ActorKind::IntegrationClient || !binding.create_enabled {
        return Err(FhirError::forbidden());
    }
    let idempotency_key = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| FhirError::invalid("IDEMPOTENCY_KEY_REQUIRED"))?;
    let source_record_key = headers
        .get("x-source-record-key")
        .and_then(|value| value.to_str().ok())
        .filter(|key| {
            !key.is_empty() && key.len() <= 512 && key.bytes().all(|b| b.is_ascii_graphic())
        })
        .ok_or_else(|| FhirError::invalid("SOURCE_RECORD_KEY_REQUIRED"))?;
    let assertions = patient_assertions(&patient)?;
    let identifiers = patient
        .identifier
        .into_iter()
        .map(|identifier| {
            Ok(IdentifierAssertion {
                authority: identifier
                    .system
                    .ok_or_else(|| FhirError::invalid("IDENTIFIER_SYSTEM_REQUIRED"))?,
                identifier_type: identifier
                    .r#type
                    .as_ref()
                    .ok_or_else(|| FhirError::invalid("IDENTIFIER_TYPE_REQUIRED"))
                    .and_then(|kind| {
                        serde_json::to_string(kind)
                            .map_err(|_| FhirError::invalid("IDENTIFIER_TYPE_INVALID"))
                    })?,
                value: identifier
                    .value
                    .ok_or_else(|| FhirError::invalid("IDENTIFIER_VALUE_REQUIRED"))?,
                authoritative: false,
            })
        })
        .collect::<Result<Vec<_>, FhirError>>()?;
    if identifiers.is_empty() {
        return Err(FhirError::invalid("IDENTIFIER_REQUIRED"));
    }
    let receipt = state
        .registry
        .ingest(
            Some(&context),
            idempotency_key,
            SourceRecordCommand {
                registry_id: binding.registry_id.clone(),
                organization_id: binding.organization_id.clone(),
                source_system_id: binding.source_system_id.clone(),
                source_record_key: source_record_key.to_owned(),
                source_version: patient.meta.and_then(|meta| meta.version_id),
                assertions,
                identifiers,
            },
        )
        .await
        .map_err(FhirError::from_registry)?;
    let person = state
        .registry
        .person(
            Some(&context),
            &binding.registry_id,
            &binding.organization_id,
            &receipt.person_public_id,
        )
        .await
        .map_err(FhirError::from_registry)?
        .ok_or_else(FhirError::unavailable)?;
    let status = if receipt.replayed {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    let patient = fhir_patient(person)?;
    let location = format!("/fhir/r4/Patient/{}", patient.id);
    let mut response = fhir_json((status, Json(patient))).into_response();
    if let Ok(value) = HeaderValue::from_str(&location) {
        response.headers_mut().insert(header::LOCATION, value);
    }
    Ok(response)
}

fn patient_assertions(patient: &PatientCreate) -> Result<Vec<SourceAssertion>, FhirError> {
    if patient.identifier.len() > 64
        || patient.name.as_ref().is_some_and(|names| names.len() > 8)
        || patient
            .telecom
            .as_ref()
            .is_some_and(|items| items.len() > 16)
        || patient
            .address
            .as_ref()
            .is_some_and(|items| items.len() > 8)
    {
        return Err(FhirError::invalid("FHIR_REPEATING_ELEMENT_LIMIT_EXCEEDED"));
    }
    if patient.name.as_ref().is_some_and(|names| {
        names.iter().any(|name| {
            name.text.as_ref().is_some_and(|v| v.len() > 16_384)
                || name.family.as_ref().is_some_and(|v| v.len() > 16_384)
                || name
                    .given
                    .iter()
                    .chain(&name.prefix)
                    .chain(&name.suffix)
                    .any(|v| v.len() > 16_384)
        })
    }) {
        return Err(FhirError::invalid("FHIR_NAME_VALUE_LIMIT_EXCEEDED"));
    }
    if patient
        .language
        .as_ref()
        .is_some_and(|language| !valid_language_tag(language) || language.len() > 64)
    {
        return Err(FhirError::invalid("PATIENT_LANGUAGE_INVALID"));
    }
    let mut assertions = Vec::new();
    if let Some(names) = &patient.name {
        assertions.push(json_assertion("fhir.name", names)?);
        let texts = names
            .iter()
            .filter_map(|name| name.text.as_deref())
            .collect::<Vec<_>>();
        if texts.len() > 1 {
            return Err(FhirError::invalid(
                "MULTIPLE_HUMANNAME_TEXT_VALUES_UNSUPPORTED",
            ));
        }
        if let Some(text) = texts.first() {
            if text.is_empty() {
                return Err(FhirError::invalid("HUMANNAME_TEXT_EMPTY"));
            }
            let name_language = names
                .iter()
                .find(|name| name.text.as_deref() == Some(*text))
                .map(human_name_language)
                .transpose()?
                .flatten();
            assertions.push(assertion_with_language(
                "name.original",
                text,
                name_language,
            ));
        }
    }
    if let Some(language) = &patient.language {
        assertions.push(assertion("fhir.resource.language", language));
    }
    if let Some(birth_date) = &patient.birth_date {
        if !valid_fhir_date(birth_date) {
            return Err(FhirError::invalid("BIRTH_DATE_INVALID"));
        }
        assertions.push(assertion("birth_date", birth_date));
    }
    if let Some(gender) = &patient.gender {
        if !matches!(gender.as_str(), "male" | "female" | "other" | "unknown") {
            return Err(FhirError::invalid("GENDER_INVALID"));
        }
        assertions.push(assertion("gender", gender));
    }
    if let Some(active) = patient.active {
        assertions.push(json_assertion("fhir.active", &active)?);
    }
    if let Some(telecom) = &patient.telecom {
        assertions.push(json_assertion("fhir.telecom", telecom)?);
    }
    if let Some(address) = &patient.address {
        assertions.push(json_assertion("fhir.address", address)?);
    }
    if assertions.is_empty() {
        return Err(FhirError::invalid("SUPPORTED_PATIENT_DATA_REQUIRED"));
    }
    Ok(assertions)
}

fn assertion(field_path: &str, value: &str) -> SourceAssertion {
    SourceAssertion {
        field_path: field_path.to_owned(),
        original_value: value.to_owned(),
        language_tag: None,
        script: None,
    }
}

fn assertion_with_language(
    field_path: &str,
    value: &str,
    language_tag: Option<&str>,
) -> SourceAssertion {
    SourceAssertion {
        field_path: field_path.to_owned(),
        original_value: value.to_owned(),
        language_tag: language_tag.map(str::to_owned),
        script: None,
    }
}

fn valid_language_tag(value: &str) -> bool {
    if value.is_empty() || value.len() > 64 {
        return false;
    }
    let mut parts = value.split('-');
    let Some(language) = parts.next() else {
        return false;
    };
    (2..=8).contains(&language.len())
        && language.bytes().all(|byte| byte.is_ascii_alphabetic())
        && parts.all(|part| {
            (1..=8).contains(&part.len()) && part.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
}

fn human_name_language(name: &FhirHumanName) -> Result<Option<&str>, FhirError> {
    let mut languages = name
        .extension
        .iter()
        .filter(|extension| extension.url == HUMAN_LANGUAGE_EXTENSION);
    let Some(extension) = languages.next() else {
        return Ok(None);
    };
    if languages.next().is_some()
        || extension.value_string.is_some()
        || extension
            .value_code
            .as_deref()
            .is_none_or(|value| !valid_language_tag(value))
    {
        return Err(FhirError::invalid("HUMANNAME_LANGUAGE_INVALID"));
    }
    Ok(extension.value_code.as_deref())
}

fn json_assertion<T: Serialize>(field_path: &str, value: &T) -> Result<SourceAssertion, FhirError> {
    Ok(assertion(
        field_path,
        &serde_json::to_string(value).map_err(|_| FhirError::invalid("FHIR_RESOURCE_INVALID"))?,
    ))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PatientCreate {
    #[serde(rename = "resourceType")]
    resource_type: String,
    #[serde(default)]
    identifier: Vec<FhirIdentifier>,
    #[serde(default)]
    meta: Option<FhirCreateMeta>,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    name: Option<Vec<FhirHumanName>>,
    #[serde(default, rename = "birthDate")]
    birth_date: Option<String>,
    #[serde(default)]
    gender: Option<String>,
    #[serde(default)]
    active: Option<bool>,
    #[serde(default)]
    telecom: Option<Vec<FhirTelecom>>,
    #[serde(default)]
    address: Option<Vec<FhirAddress>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct FhirCreateMeta {
    #[serde(default)]
    version_id: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FhirIdentifier {
    #[serde(default)]
    system: Option<String>,
    #[serde(default)]
    value: Option<String>,
    #[serde(default, rename = "type")]
    r#type: Option<FhirCodeableConcept>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FhirCodeableConcept {
    #[serde(default)]
    coding: Vec<FhirCoding>,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FhirCoding {
    #[serde(default)]
    system: Option<String>,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    display: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FhirHumanName {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    extension: Vec<FhirExtension>,
    #[serde(default, rename = "use")]
    use_code: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    family: Option<String>,
    #[serde(default)]
    given: Vec<String>,
    #[serde(default)]
    prefix: Vec<String>,
    #[serde(default)]
    suffix: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FhirExtension {
    url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    value_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    value_string: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FhirTelecom {
    #[serde(default)]
    system: Option<String>,
    #[serde(default)]
    value: Option<String>,
    #[serde(default, rename = "use")]
    use_code: Option<String>,
    #[serde(default)]
    rank: Option<u32>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FhirAddress {
    #[serde(default, rename = "use")]
    use_code: Option<String>,
    #[serde(default)]
    r#type: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    line: Vec<String>,
    #[serde(default)]
    city: Option<String>,
    #[serde(default)]
    district: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    postal_code: Option<String>,
    #[serde(default)]
    country: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FhirPatient {
    resource_type: &'static str,
    id: String,
    meta: FhirMeta,
    identifier: Vec<PatientIdentifier>,
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    active: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<Vec<FhirHumanName>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    birth_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gender: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    telecom: Option<Vec<FhirTelecom>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    address: Option<Vec<FhirAddress>>,
}

#[derive(Debug, Serialize)]
struct FhirMeta {
    #[serde(rename = "versionId")]
    version_id: String,
}

#[derive(Debug, Serialize)]
struct PatientIdentifier {
    system: String,
    value: String,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    identifier_type: Option<FhirCodeableConcept>,
}

fn fhir_patient(person: PersonView) -> Result<FhirPatient, FhirError> {
    let mut patient = FhirPatient {
        resource_type: "Patient",
        id: person.public_id,
        meta: FhirMeta {
            version_id: person.resource_version.to_string(),
        },
        identifier: person
            .identifiers
            .into_iter()
            .map(identifier_response)
            .collect::<Result<Vec<_>, _>>()?,
        language: None,
        active: None,
        name: None,
        birth_date: None,
        gender: None,
        telecom: None,
        address: None,
    };
    let mut original_names = Vec::new();
    for assertion in person.assertions {
        if assertion.script.is_some() && assertion.field_path != "name.original" {
            return Err(FhirError::non_representable());
        }
        match assertion.field_path.as_str() {
            "birth_date" | "birthDate" => {
                if assertion.language_tag.is_some() {
                    return Err(FhirError::non_representable());
                }
                if !valid_fhir_date(&assertion.original_value) {
                    return Err(FhirError::non_representable());
                }
                patient.birth_date = Some(assertion.original_value);
            }
            "gender" => {
                if assertion.language_tag.is_some() {
                    return Err(FhirError::non_representable());
                }
                if !matches!(
                    assertion.original_value.as_str(),
                    "male" | "female" | "other" | "unknown"
                ) {
                    return Err(FhirError::non_representable());
                }
                patient.gender = Some(assertion.original_value);
            }
            "fhir.name" => {
                if assertion.language_tag.is_some() {
                    return Err(FhirError::non_representable());
                }
                patient.name = Some(decode_json(&assertion.original_value)?);
            }
            "fhir.resource.language" => {
                if assertion.language_tag.is_some()
                    || !valid_language_tag(&assertion.original_value)
                {
                    return Err(FhirError::non_representable());
                }
                patient.language = Some(assertion.original_value);
            }
            "name.original" => {
                if let Some(language_tag) = assertion.language_tag.as_deref() {
                    if !valid_language_tag(language_tag) {
                        return Err(FhirError::non_representable());
                    }
                }
                let extensions = name_extensions(
                    assertion.language_tag.as_deref(),
                    assertion.script.as_deref(),
                )?;
                original_names.push((assertion.original_value, extensions));
            }
            "fhir.active" => {
                if assertion.language_tag.is_some() {
                    return Err(FhirError::non_representable());
                }
                patient.active = Some(decode_json(&assertion.original_value)?);
            }
            "fhir.telecom" => {
                if assertion.language_tag.is_some() {
                    return Err(FhirError::non_representable());
                }
                patient.telecom = Some(decode_json(&assertion.original_value)?);
            }
            "fhir.address" => {
                if assertion.language_tag.is_some() {
                    return Err(FhirError::non_representable());
                }
                patient.address = Some(decode_json(&assertion.original_value)?);
            }
            _ => return Err(FhirError::non_representable()),
        }
    }
    if !original_names.is_empty() {
        let names = patient.name.get_or_insert_with(Vec::new);
        for (text, extensions) in original_names {
            if let Some(name) = names
                .iter_mut()
                .find(|name| name.text.as_deref() == Some(text.as_str()))
            {
                for extension in extensions {
                    if !name.extension.iter().any(|existing| {
                        existing.url == extension.url
                            && existing.value_code == extension.value_code
                            && existing.value_string == extension.value_string
                    }) {
                        name.extension.push(extension);
                    }
                }
                continue;
            }
            names.push(FhirHumanName {
                extension: extensions,
                use_code: None,
                text: Some(text),
                family: None,
                given: Vec::new(),
                prefix: Vec::new(),
                suffix: Vec::new(),
            });
        }
    }
    Ok(patient)
}

/// FHIR R4 defines the Human Language extension for HumanName, but no standard
/// HumanName script extension. Preserve each source value independently: use
/// the HL7 extension for BCP 47 language tags and a documented liteCR extension
/// for the source's verbatim script label.
fn name_extensions(
    language: Option<&str>,
    script: Option<&str>,
) -> Result<Vec<FhirExtension>, FhirError> {
    let mut extensions = Vec::new();
    if let Some(language) = language {
        if !valid_language_tag(language) {
            return Err(FhirError::non_representable());
        }
        extensions.push(FhirExtension {
            url: HUMAN_LANGUAGE_EXTENSION.to_owned(),
            value_code: Some(language.to_owned()),
            value_string: None,
        });
    }
    if let Some(script) = script {
        if script.trim().is_empty() || script.len() > 32 {
            return Err(FhirError::non_representable());
        }
        extensions.push(FhirExtension {
            url: LITECR_NAME_SCRIPT_EXTENSION.to_owned(),
            value_code: None,
            value_string: Some(script.to_owned()),
        });
    }
    Ok(extensions)
}

fn identifier_response(identifier: IdentifierView) -> Result<PatientIdentifier, FhirError> {
    let identifier_type = serde_json::from_str(&identifier.identifier_type)
        .ok()
        .or_else(|| {
            Some(FhirCodeableConcept {
                coding: Vec::new(),
                text: Some(identifier.identifier_type.clone()),
            })
        });
    Ok(PatientIdentifier {
        system: identifier.authority,
        value: identifier.value,
        identifier_type,
    })
}

fn decode_json<T: for<'de> Deserialize<'de>>(value: &str) -> Result<T, FhirError> {
    serde_json::from_str(value).map_err(|_| FhirError::non_representable())
}

#[derive(Debug, Serialize)]
struct SearchBundle {
    #[serde(rename = "resourceType")]
    resource_type: &'static str,
    #[serde(rename = "type")]
    bundle_type: &'static str,
    total: u32,
    entry: Vec<BundleEntry>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BundleEntry {
    resource: FhirPatient,
    search: SearchEntry,
}

#[derive(Debug, Serialize)]
struct SearchEntry {
    mode: &'static str,
}

#[derive(Debug, Serialize)]
struct OperationOutcome {
    #[serde(rename = "resourceType")]
    resource_type: &'static str,
    issue: Vec<OperationIssue>,
}

#[derive(Debug, Serialize)]
struct OperationIssue {
    severity: &'static str,
    code: &'static str,
    diagnostics: &'static str,
}

#[derive(Debug)]
struct FhirError {
    status: StatusCode,
    code: &'static str,
    diagnostics: &'static str,
}

impl FhirError {
    fn invalid(code: &'static str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "invalid",
            diagnostics: code,
        }
    }
    fn invalid_identifier() -> Self {
        Self::invalid("identifier must be one exact value or system|value token")
    }
    fn forbidden() -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            code: "forbidden",
            diagnostics: "The requested operation is not permitted.",
        }
    }
    fn not_found() -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: "not-found",
            diagnostics: "The requested Patient was not found.",
        }
    }
    fn too_large() -> Self {
        Self {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            code: "too-costly",
            diagnostics: "Patient resource exceeds the supported size bound.",
        }
    }
    fn too_costly() -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "too-costly",
            diagnostics: "The exact identifier search exceeded the bounded result limit.",
        }
    }
    fn non_representable() -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "not-supported",
            diagnostics: "The stored source value cannot be represented losslessly by this FHIR profile.",
        }
    }
    fn unavailable() -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "transient",
            diagnostics: "The registry is temporarily unavailable.",
        }
    }

    fn from_registry(error: RegistryError) -> Self {
        match error {
            RegistryError::AccessDenied(_) => Self::forbidden(),
            RegistryError::InvalidCommand(_) => {
                Self::invalid("The registry rejected the Patient mapping.")
            }
            RegistryError::InvalidIdempotencyKey => {
                Self::invalid("Idempotency-Key is missing or invalid.")
            }
            RegistryError::Store(
                litecr_application::registry::RegistryStoreError::IdempotencyKeyReused,
            ) => Self {
                status: StatusCode::CONFLICT,
                code: "conflict",
                diagnostics: "The idempotency key was reused with a different command.",
            },
            RegistryError::Store(litecr_application::registry::RegistryStoreError::Unavailable) => {
                Self::unavailable()
            }
        }
    }
}

impl IntoResponse for FhirError {
    fn into_response(self) -> Response {
        fhir_json((
            self.status,
            Json(OperationOutcome {
                resource_type: "OperationOutcome",
                issue: vec![OperationIssue {
                    severity: "error",
                    code: self.code,
                    diagnostics: self.diagnostics,
                }],
            }),
        ))
        .into_response()
    }
}

/// Convert authentication middleware failures into this adapter's error contract.
/// The access layer stays transport-agnostic for every non-FHIR route.
pub(crate) fn access_failure(status: StatusCode) -> Response {
    let (code, diagnostics) = match status {
        StatusCode::UNAUTHORIZED => ("login", "Authentication is required."),
        StatusCode::FORBIDDEN => ("forbidden", "The requested operation is not permitted."),
        _ => ("security", "The request could not be authorized."),
    };
    FhirError {
        status,
        code,
        diagnostics,
    }
    .into_response()
}

fn client_binding<'a>(
    state: &'a ApiState,
    context: &AccessContext,
) -> Result<&'a FhirClientBinding, FhirError> {
    if context.actor().kind != ActorKind::IntegrationClient {
        return Err(FhirError::forbidden());
    }
    state
        .clients
        .get(&binding_key(context))
        .ok_or_else(FhirError::forbidden)
}

async fn fhir_not_found() -> Response {
    FhirError::not_found().into_response()
}

async fn fhir_method_not_allowed() -> Response {
    FhirError {
        status: StatusCode::METHOD_NOT_ALLOWED,
        code: "not-supported",
        diagnostics: "This Patient interaction is not supported.",
    }
    .into_response()
}

/// Bindings are issuer-scoped because OIDC subjects are not globally unique.
/// Use `#subject` only when the trusted actor has no issuer configured.
fn binding_key(context: &AccessContext) -> String {
    format!(
        "{}#{}",
        context.actor().issuer_url.as_deref().unwrap_or(""),
        context.actor().id
    )
}

fn fhir_json<T: IntoResponse>(response: T) -> Response {
    let mut response = response.into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(FHIR_JSON));
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert("x-fhir-version", HeaderValue::from_static(FHIR_VERSION));
    response
}

fn is_fhir_json_request(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            let media_type = v.split(';').next().unwrap_or("").trim();
            media_type.eq_ignore_ascii_case("application/fhir+json")
                || media_type.eq_ignore_ascii_case("application/json")
        })
}

fn valid_fhir_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
}

fn valid_fhir_date(date: &str) -> bool {
    let parts: Vec<_> = date.split('-').collect();
    match parts.as_slice() {
        [year] => year.len() == 4 && year.bytes().all(|b| b.is_ascii_digit()),
        [year, month] => {
            year.len() == 4
                && year.bytes().all(|b| b.is_ascii_digit())
                && month.len() == 2
                && month.bytes().all(|b| b.is_ascii_digit())
                && (1..=12).contains(&month.parse::<u8>().unwrap_or(0))
        }
        [year, month, day] => {
            year.len() == 4
                && year.bytes().all(|b| b.is_ascii_digit())
                && month.len() == 2
                && month.bytes().all(|b| b.is_ascii_digit())
                && day.len() == 2
                && day.bytes().all(|b| b.is_ascii_digit())
                && valid_calendar_day(year, month, day)
        }
        _ => false,
    }
}

fn valid_calendar_day(year: &str, month: &str, day: &str) -> bool {
    let (Ok(year), Ok(month), Ok(day)) =
        (year.parse::<u16>(), month.parse::<u8>(), day.parse::<u8>())
    else {
        return false;
    };
    let max_day = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 400 == 0 || (year % 4 == 0 && year % 100 != 0) => 29,
        2 => 28,
        _ => return false,
    };
    (1..=max_day).contains(&day)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CREATE_FIXTURE: &str =
        include_str!("../../../../tests/contracts/fhir-r4/patient-create-synthetic.json");

    #[test]
    fn partial_dates_keep_precision_and_invalid_calendar_dates_are_rejected() {
        for valid in ["1986", "1986-04", "1986-04-23", "2000-02-29"] {
            assert!(valid_fhir_date(valid), "{valid}");
        }
        for invalid in [
            "1986-4",
            "1986-13",
            "1900-02-29",
            "1986-04-31",
            "1986-04-23T00:00:00Z",
            "unknown",
        ] {
            assert!(!valid_fhir_date(invalid), "{invalid}");
        }
    }

    #[test]
    fn fhir_id_and_exact_identifier_inputs_are_bounded() {
        assert!(valid_fhir_id("litecr-123"));
        assert!(!valid_fhir_id("not/an/id"));
        let q = IdentifierQuery {
            identifier: "urn:clinic:mrn|00042".to_owned(),
        };
        assert_eq!(
            q.identifier.split_once('|'),
            Some(("urn:clinic:mrn", "00042"))
        );
    }

    #[test]
    fn official_shaped_synthetic_create_fixture_maps_supported_fields_and_rejects_unknown_fields() {
        let parsed: PatientCreate = serde_json::from_str(CREATE_FIXTURE).unwrap();
        let assertions = patient_assertions(&parsed).unwrap();
        assert!(
            assertions
                .iter()
                .any(|a| a.field_path == "birth_date" && a.original_value == "1986-04")
        );
        assert!(assertions.iter().any(|a| a.field_path == "fhir.name"));
        assert!(assertions.iter().any(|a| {
            a.field_path == "name.original"
                && a.original_value == "ນາງ ສຸກສະຫວັນ"
                && a.language_tag.as_deref() == Some("lo")
        }));
        assert!(
            assertions
                .iter()
                .any(|a| { a.field_path == "fhir.resource.language" && a.original_value == "en" })
        );
        assert!(
            serde_json::from_str::<PatientCreate>(
                r#"{"resourceType":"Patient","deceasedBoolean":false}"#
            )
            .is_err()
        );
    }

    #[test]
    fn structured_given_and_family_without_humanname_text_do_not_create_match_name() {
        let patient: PatientCreate = serde_json::from_str(
            r#"{"resourceType":"Patient","name":[{"family":"Example","given":["Synthetic"]}]}"#,
        )
        .unwrap();
        let assertions = patient_assertions(&patient).unwrap();
        assert!(assertions.iter().any(|a| a.field_path == "fhir.name"));
        assert!(!assertions.iter().any(|a| a.field_path == "name.original"));
    }

    #[test]
    fn read_projection_preserves_partial_birth_date_and_identifier_authority() {
        let person = PersonView {
            public_id: "litecr-synthetic-42".into(),
            resource_version: 3,
            source_record_ids: vec!["source-synthetic-42".into()],
            assertions: vec![
                litecr_domain::registry::SourceAssertionView {
                    source_record_id: "source-synthetic-42".into(),
                    source_system_id: "synthetic-system".into(),
                    source_version: Some("synthetic-v1".into()),
                    receive_version: 1,
                    field_path: "birth_date".into(),
                    original_value: "1986-04".into(),
                    language_tag: None,
                    script: None,
                },
                litecr_domain::registry::SourceAssertionView {
                    source_record_id: "source-synthetic-42".into(),
                    source_system_id: "synthetic-system".into(),
                    source_version: Some("synthetic-v1".into()),
                    receive_version: 1,
                    field_path: "fhir.name".into(),
                    original_value: r#"[{"use":"official","text":"ນາງ ສຸກສະຫວັນ","family":"ສຸກສະຫວັນ","given":["ນາງ"]}]"#.into(),
                    language_tag: None,
                    script: None,
                },
                litecr_domain::registry::SourceAssertionView {
                    source_record_id: "source-synthetic-42".into(),
                    source_system_id: "synthetic-system".into(),
                    source_version: Some("synthetic-v1".into()),
                    receive_version: 1,
                    field_path: "fhir.resource.language".into(),
                    original_value: "en".into(),
                    language_tag: None,
                    script: None,
                },
                litecr_domain::registry::SourceAssertionView {
                    source_record_id: "source-synthetic-42".into(),
                    source_system_id: "synthetic-system".into(),
                    source_version: Some("synthetic-v1".into()),
                    receive_version: 1,
                    field_path: "name.original".into(),
                    original_value: "ນາງ ສຸກສະຫວັນ".into(),
                    language_tag: Some("lo".into()),
                    script: None,
                },
            ],
            aliases: Vec::new(),
            identifiers: vec![IdentifierView {
                authority: "urn:synthetic:authority".into(),
                identifier_type: r#"{"text":"synthetic MRN"}"#.into(),
                value: "SYN-42".into(),
                authoritative: false,
            }],
        };
        let output = serde_json::to_value(fhir_patient(person).unwrap()).unwrap();
        assert_eq!(output["id"], "litecr-synthetic-42");
        assert_eq!(output["meta"]["versionId"], "3");
        assert_eq!(output["birthDate"], "1986-04");
        assert_eq!(output["language"], "en");
        assert_eq!(output["name"][0]["text"], "ນາງ ສຸກສະຫວັນ");
        assert_eq!(output["name"][0]["extension"][0]["valueCode"], "lo");
        assert_eq!(output["name"][0]["family"], "ສຸກສະຫວັນ");
        assert_eq!(output["name"][0]["given"][0], "ນາງ");
        assert_eq!(output["identifier"][0]["system"], "urn:synthetic:authority");
        assert_eq!(output["identifier"][0]["value"], "SYN-42");
        assert_eq!(output["identifier"][0]["type"]["text"], "synthetic MRN");
    }

    #[test]
    fn read_projection_preserves_script_with_a_litecr_extension() {
        let mut person = PersonView {
            public_id: "litecr-synthetic-42".into(),
            resource_version: 1,
            source_record_ids: vec!["source-synthetic-42".into()],
            assertions: vec![litecr_domain::registry::SourceAssertionView {
                source_record_id: "source-synthetic-42".into(),
                source_system_id: "synthetic-system".into(),
                source_version: None,
                receive_version: 1,
                field_path: "name.original".into(),
                original_value: "ສົມມະນີ".into(),
                language_tag: Some("lo".into()),
                script: Some("Lao".into()),
            }],
            aliases: Vec::new(),
            identifiers: vec![IdentifierView {
                authority: "urn:synthetic:authority".into(),
                identifier_type: "mrn".into(),
                value: "SYN-42".into(),
                authoritative: false,
            }],
        };
        let output = serde_json::to_value(fhir_patient(person.clone()).unwrap()).unwrap();
        assert!(output.get("language").is_none());
        assert_eq!(output["name"][0]["text"], "ສົມມະນີ");
        assert_eq!(
            output["name"][0]["extension"][0]["url"],
            HUMAN_LANGUAGE_EXTENSION
        );
        assert_eq!(output["name"][0]["extension"][0]["valueCode"], "lo");
        assert_eq!(
            output["name"][0]["extension"][1]["url"],
            LITECR_NAME_SCRIPT_EXTENSION
        );
        assert_eq!(output["name"][0]["extension"][1]["valueString"], "Lao");
        person.assertions[0].script = None;
        person.assertions[0].language_tag = None;
        person.assertions[0].field_path = "unknown.internal.value".into();
        assert!(
            fhir_patient(person).is_err(),
            "unknown internal values are not silently omitted"
        );
    }

    #[test]
    fn source_script_is_separate_from_standard_name_language() {
        let extensions = name_extensions(Some("lo"), Some("Lao")).unwrap();
        assert_eq!(extensions[0].url, HUMAN_LANGUAGE_EXTENSION);
        assert_eq!(extensions[0].value_code.as_deref(), Some("lo"));
        assert_eq!(extensions[0].value_string, None);
        assert_eq!(extensions[1].url, LITECR_NAME_SCRIPT_EXTENSION);
        assert_eq!(extensions[1].value_code, None);
        assert_eq!(extensions[1].value_string.as_deref(), Some("Lao"));
        assert!(name_extensions(Some("not a tag"), Some("Lao")).is_err());
        assert!(name_extensions(Some("lo"), Some(" ")).is_err());
    }

    #[test]
    fn patient_resource_language_is_independent_from_human_name_language() {
        let patient: PatientCreate = serde_json::from_str(
            r#"{
                "resourceType":"Patient",
                "language":"en",
                "name":[{
                    "text":"ສົມມະນີ",
                    "extension":[{
                        "url":"http://hl7.org/fhir/StructureDefinition/language",
                        "valueCode":"lo"
                    }]
                }]
            }"#,
        )
        .unwrap();
        let assertions = patient_assertions(&patient).unwrap();
        assert!(assertions.iter().any(|assertion| {
            assertion.field_path == "fhir.resource.language"
                && assertion.original_value == "en"
                && assertion.language_tag.is_none()
        }));
        assert!(assertions.iter().any(|assertion| {
            assertion.field_path == "name.original"
                && assertion.original_value == "ສົມມະນີ"
                && assertion.language_tag.as_deref() == Some("lo")
        }));

        let invalid_name_language: PatientCreate = serde_json::from_str(
            r#"{
                "resourceType":"Patient",
                "name":[{
                    "text":"Synthetic",
                    "extension":[{
                        "url":"http://hl7.org/fhir/StructureDefinition/language",
                        "valueString":"lo"
                    }]
                }]
            }"#,
        )
        .unwrap();
        assert!(patient_assertions(&invalid_name_language).is_err());
    }
}
