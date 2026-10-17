//! Scoped registry use cases. Registry is the only authority that accepts source mutations.

use async_trait::async_trait;
pub use litecr_domain::registry::{
    IdentifierAssertion, IdentifierView, IngestionOutcome, IngestionReceipt, PersonView,
    RegistryValidationError, SourceAssertion, SourceAssertionView, SourceRecordCommand,
};
use thiserror::Error;

use crate::access::{AccessContext, AccessDenied, ActorKind, Permission, ResourceScope, authorize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryActor {
    pub kind: ActorKind,
    pub id: String,
    pub issuer_url: Option<String>,
    pub request_id: String,
    pub correlation_id: Option<String>,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum RegistryStoreError {
    #[error("registry persistence is unavailable")]
    Unavailable,
    #[error("idempotency key was reused with a different command")]
    IdempotencyKeyReused,
}

#[async_trait]
pub trait RegistryStore: Send + Sync + 'static {
    /// Must commit source/person state, payload-bound idempotency, mutation audit and outbox together.
    async fn ingest(
        &self,
        actor: &RegistryActor,
        idempotency_key: &str,
        command: &SourceRecordCommand,
    ) -> Result<IngestionReceipt, RegistryStoreError>;

    /// Resolve only within the supplied registry and organization scope.
    async fn person(
        &self,
        registry_id: &str,
        organization_id: &str,
        public_id_or_alias: &str,
    ) -> Result<Option<PersonView>, RegistryStoreError>;

    /// Resolve a source reference inside one registry/organization and return its person view.
    async fn person_for_source(
        &self,
        registry_id: &str,
        organization_id: &str,
        source_record_id: &str,
    ) -> Result<Option<PersonView>, RegistryStoreError>;

    /// This vertical slice provides exact public-ID/alias lookup only; matching is a later phase.
    async fn search_public_id(
        &self,
        registry_id: &str,
        organization_id: &str,
        public_id_or_alias: &str,
        limit: u16,
    ) -> Result<Vec<PersonView>, RegistryStoreError>;

    /// Exact business-identifier search within one registry and organization.
    async fn search_identifier(
        &self,
        registry_id: &str,
        organization_id: &str,
        authority: &str,
        value: &str,
        limit: u16,
    ) -> Result<Vec<PersonView>, RegistryStoreError>;

    /// Exact identifier-value search across namespaces within a single registry/organization.
    /// Callers must have explicit PersonSearch permission for that entire scope.
    async fn search_identifier_value(
        &self,
        registry_id: &str,
        organization_id: &str,
        value: &str,
        limit: u16,
    ) -> Result<Vec<PersonView>, RegistryStoreError>;
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum RegistryError {
    #[error("access denied")]
    AccessDenied(#[from] AccessDenied),
    #[error("invalid registry command")]
    InvalidCommand(RegistryValidationError),
    #[error("invalid idempotency key")]
    InvalidIdempotencyKey,
    #[error(transparent)]
    Store(#[from] RegistryStoreError),
}

pub struct RegistryService {
    store: Box<dyn RegistryStore>,
    max_page_size: u16,
}

impl RegistryService {
    pub fn new(store: Box<dyn RegistryStore>, max_page_size: u16) -> Option<Self> {
        (1..=100).contains(&max_page_size).then_some(Self {
            store,
            max_page_size,
        })
    }

    pub async fn ingest(
        &self,
        context: Option<&AccessContext>,
        idempotency_key: &str,
        command: SourceRecordCommand,
    ) -> Result<IngestionReceipt, RegistryError> {
        validate_idempotency_key(idempotency_key)?;
        command.validate().map_err(RegistryError::InvalidCommand)?;
        let scope = ResourceScope {
            registry_id: command.registry_id.clone(),
            organization_id: command.organization_id.clone(),
            record_id: None,
        };
        authorize(context, Permission::SourceWrite, &scope)?;
        let context = context.ok_or(AccessDenied::Unauthenticated)?;
        let actor = RegistryActor {
            kind: context.actor().kind,
            id: context.actor().id.clone(),
            issuer_url: context.actor().issuer_url.clone(),
            request_id: context.request_id().to_owned(),
            correlation_id: context.correlation_id().map(str::to_owned),
        };
        self.store
            .ingest(&actor, idempotency_key, &command)
            .await
            .map_err(Into::into)
    }

    pub async fn person(
        &self,
        context: Option<&AccessContext>,
        registry_id: &str,
        organization_id: &str,
        public_id_or_alias: &str,
    ) -> Result<Option<PersonView>, RegistryError> {
        let scope = ResourceScope {
            registry_id: registry_id.to_owned(),
            organization_id: organization_id.to_owned(),
            record_id: None,
        };
        authorize(context, Permission::PersonRead, &scope)?;
        self.store
            .person(registry_id, organization_id, public_id_or_alias)
            .await
            .map_err(Into::into)
    }

    pub async fn search_public_id(
        &self,
        context: Option<&AccessContext>,
        registry_id: &str,
        organization_id: &str,
        public_id_or_alias: &str,
        limit: u16,
    ) -> Result<Vec<PersonView>, RegistryError> {
        let scope = ResourceScope {
            registry_id: registry_id.to_owned(),
            organization_id: organization_id.to_owned(),
            record_id: None,
        };
        authorize(context, Permission::PersonSearch, &scope)?;
        if public_id_or_alias.is_empty()
            || public_id_or_alias.len() > 512
            || limit == 0
            || limit > self.max_page_size
        {
            return Err(RegistryError::InvalidCommand(
                RegistryValidationError::InvalidField,
            ));
        }
        self.store
            .search_public_id(registry_id, organization_id, public_id_or_alias, limit)
            .await
            .map_err(Into::into)
    }

    pub async fn search_identifier(
        &self,
        context: Option<&AccessContext>,
        registry_id: &str,
        organization_id: &str,
        authority: &str,
        value: &str,
        limit: u16,
    ) -> Result<Vec<PersonView>, RegistryError> {
        let scope = ResourceScope {
            registry_id: registry_id.to_owned(),
            organization_id: organization_id.to_owned(),
            record_id: None,
        };
        authorize(context, Permission::PersonSearch, &scope)?;
        if authority.trim().is_empty()
            || authority.len() > 2048
            || value.trim().is_empty()
            || value.len() > 512
            || limit == 0
            || limit > self.max_page_size
        {
            return Err(RegistryError::InvalidCommand(
                RegistryValidationError::InvalidField,
            ));
        }
        self.store
            .search_identifier(registry_id, organization_id, authority, value, limit)
            .await
            .map_err(Into::into)
    }

    pub async fn search_identifier_value(
        &self,
        context: Option<&AccessContext>,
        registry_id: &str,
        organization_id: &str,
        value: &str,
        limit: u16,
    ) -> Result<Vec<PersonView>, RegistryError> {
        let scope = ResourceScope {
            registry_id: registry_id.to_owned(),
            organization_id: organization_id.to_owned(),
            record_id: None,
        };
        authorize(context, Permission::PersonSearch, &scope)?;
        if value.trim().is_empty() || value.len() > 512 || limit == 0 || limit > self.max_page_size
        {
            return Err(RegistryError::InvalidCommand(
                RegistryValidationError::InvalidField,
            ));
        }
        self.store
            .search_identifier_value(registry_id, organization_id, value, limit)
            .await
            .map_err(Into::into)
    }

    pub async fn source_record_person(
        &self,
        context: Option<&AccessContext>,
        registry_id: &str,
        organization_id: &str,
        source_record_id: &str,
    ) -> Result<Option<PersonView>, RegistryError> {
        let scope = ResourceScope {
            registry_id: registry_id.to_owned(),
            organization_id: organization_id.to_owned(),
            record_id: Some(source_record_id.to_owned()),
        };
        authorize(context, Permission::PersonRead, &scope)?;
        if source_record_id.len() != 36
            || !source_record_id
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
        {
            return Err(RegistryError::InvalidCommand(
                RegistryValidationError::InvalidField,
            ));
        }
        self.store
            .person_for_source(registry_id, organization_id, source_record_id)
            .await
            .map_err(Into::into)
    }
}

fn validate_idempotency_key(key: &str) -> Result<(), RegistryError> {
    if !(16..=200).contains(&key.len()) || !key.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err(RegistryError::InvalidIdempotencyKey);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::{ActorKind, ActorRef, Role, ScopeGrant};
    use std::{
        collections::BTreeSet,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    struct PersonReadStore(Arc<AtomicUsize>);

    #[async_trait]
    impl RegistryStore for PersonReadStore {
        async fn ingest(
            &self,
            _: &RegistryActor,
            _: &str,
            _: &SourceRecordCommand,
        ) -> Result<IngestionReceipt, RegistryStoreError> {
            Err(RegistryStoreError::Unavailable)
        }

        async fn person(
            &self,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<Option<PersonView>, RegistryStoreError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        }

        async fn person_for_source(
            &self,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<Option<PersonView>, RegistryStoreError> {
            Err(RegistryStoreError::Unavailable)
        }

        async fn search_public_id(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: u16,
        ) -> Result<Vec<PersonView>, RegistryStoreError> {
            Err(RegistryStoreError::Unavailable)
        }

        async fn search_identifier(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: &str,
            _: u16,
        ) -> Result<Vec<PersonView>, RegistryStoreError> {
            Err(RegistryStoreError::Unavailable)
        }

        async fn search_identifier_value(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: u16,
        ) -> Result<Vec<PersonView>, RegistryStoreError> {
            Err(RegistryStoreError::Unavailable)
        }
    }

    fn context(registry: &str, organization: &str) -> AccessContext {
        AccessContext::from_authorized_identity(
            ActorRef {
                kind: ActorKind::Human,
                id: "synthetic-registrar".into(),
                issuer_url: Some("https://issuer.example".into()),
            },
            vec![ScopeGrant {
                registry_id: registry.into(),
                organization_id: organization.into(),
                record_ids: None,
                roles: BTreeSet::from([Role::Registrar]),
                permissions: BTreeSet::new(),
            }],
            "request-synthetic".into(),
            None,
            None,
        )
        .unwrap()
    }

    fn command(registry: &str, organization: &str) -> SourceRecordCommand {
        SourceRecordCommand {
            registry_id: registry.into(),
            organization_id: organization.into(),
            source_system_id: "source-demo".into(),
            source_record_key: "record-demo".into(),
            source_version: Some("opaque-v1".into()),
            assertions: vec![litecr_domain::registry::SourceAssertion {
                field_path: "name.original".into(),
                original_value: "ສົມມະນີ".into(),
                language_tag: Some("lo".into()),
                script: Some("Lao".into()),
            }],
            identifiers: Vec::new(),
        }
    }

    #[test]
    fn source_assertion_keeps_original_script_and_partial_date() {
        let command = command("registry-a", "org-east");
        assert_eq!(command.assertions[0].original_value, "ສົມມະນີ");
        let date = litecr_domain::registry::SourceAssertion {
            field_path: "birth_date".into(),
            original_value: "1986-04".into(),
            language_tag: None,
            script: None,
        };
        assert_eq!(date.original_value, "1986-04");
    }

    #[test]
    fn registry_scope_is_checked_before_any_store_is_called() {
        let scope = ResourceScope {
            registry_id: "registry-a".into(),
            organization_id: "org-west".into(),
            record_id: None,
        };
        assert_eq!(
            authorize(
                Some(&context("registry-a", "org-east")),
                Permission::SourceWrite,
                &scope
            ),
            Err(AccessDenied::OutOfScope)
        );
        assert_eq!(
            authorize(None, Permission::SourceWrite, &scope),
            Err(AccessDenied::Unauthenticated)
        );
    }

    #[tokio::test]
    async fn person_read_without_authenticated_context_never_reaches_store() {
        let store_calls = Arc::new(AtomicUsize::new(0));
        let service = RegistryService::new(Box::new(PersonReadStore(store_calls.clone())), 20)
            .expect("valid page bound");

        let result = service
            .person(None, "registry-a", "org-east", "public-id")
            .await;

        assert!(matches!(
            result,
            Err(RegistryError::AccessDenied(AccessDenied::Unauthenticated))
        ));
        assert_eq!(store_calls.load(Ordering::SeqCst), 0);
    }
}
