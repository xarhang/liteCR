use std::collections::BTreeSet;

/// Actor class is derived from the authenticated credential, never request JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActorKind {
    Human,
    IntegrationClient,
    System,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ActorRef {
    pub kind: ActorKind,
    pub id: String,
    /// Issuer-scoped subject namespace. Required for human OIDC identities.
    pub issuer_url: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Role {
    Registrar,
    Reviewer,
    Translator,
    TranslationReviewer,
    Administrator,
    IntegrationClient,
    Auditor,
}

impl Role {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "registrar" => Some(Self::Registrar),
            "reviewer" => Some(Self::Reviewer),
            "translator" => Some(Self::Translator),
            "translation_reviewer" => Some(Self::TranslationReviewer),
            "administrator" => Some(Self::Administrator),
            "integration_client" => Some(Self::IntegrationClient),
            "auditor" => Some(Self::Auditor),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Registrar => "registrar",
            Self::Reviewer => "reviewer",
            Self::Translator => "translator",
            Self::TranslationReviewer => "translation_reviewer",
            Self::Administrator => "administrator",
            Self::IntegrationClient => "integration_client",
            Self::Auditor => "auditor",
        }
    }

    fn permissions(self) -> &'static [Permission] {
        use Permission as P;
        match self {
            Self::Registrar => &[
                P::AppearancePreferenceManage,
                P::SourceWrite,
                P::PersonRead,
                P::MatchRequest,
                P::ReceiptRead,
            ],
            Self::Reviewer => &[
                P::AppearancePreferenceManage,
                P::PersonRead,
                P::MatchEvidenceRead,
                P::ReviewRead,
                P::ReviewDecide,
                P::IdentityLink,
                P::IdentityMerge,
                P::IdentityReverse,
            ],
            Self::Translator => &[
                P::AppearancePreferenceManage,
                P::TranslationDraftRead,
                P::TranslationDraftWrite,
            ],
            Self::TranslationReviewer => &[
                P::AppearancePreferenceManage,
                P::TranslationDraftRead,
                P::TranslationReview,
                P::TranslationPublish,
            ],
            Self::Administrator => &[
                P::AppearancePreferenceManage,
                P::AccessGrantManage,
                P::ConfigurationManage,
                P::CountryProfileManage,
                P::AppearanceManage,
            ],
            Self::IntegrationClient => &[],
            Self::Auditor => &[P::AppearancePreferenceManage, P::AuditRead],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Permission {
    SourceWrite,
    MigrationRead,
    MigrationWrite,
    PersonRead,
    PersonSearch,
    MatchRequest,
    MatchEvidenceRead,
    ReceiptRead,
    ReviewRead,
    ReviewDecide,
    IdentityLink,
    IdentityMerge,
    IdentityReverse,
    ExportRead,
    TranslationDraftRead,
    TranslationDraftWrite,
    TranslationReview,
    TranslationPublish,
    AccessGrantManage,
    ConfigurationManage,
    CountryProfileManage,
    AppearanceManage,
    AppearancePreferenceManage,
    AuditRead,
}

impl Permission {
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "source:write" => Self::SourceWrite,
            "migration:read" => Self::MigrationRead,
            "migration:write" => Self::MigrationWrite,
            "person:read" => Self::PersonRead,
            "person:search" => Self::PersonSearch,
            "match:request" => Self::MatchRequest,
            "match:evidence:read" => Self::MatchEvidenceRead,
            "receipt:read" => Self::ReceiptRead,
            "review:read" => Self::ReviewRead,
            "review:decide" => Self::ReviewDecide,
            "identity:link" => Self::IdentityLink,
            "identity:merge" => Self::IdentityMerge,
            "identity:reverse" => Self::IdentityReverse,
            "export:read" => Self::ExportRead,
            "translation:draft:read" => Self::TranslationDraftRead,
            "translation:draft:write" => Self::TranslationDraftWrite,
            "translation:review" => Self::TranslationReview,
            "translation:publish" => Self::TranslationPublish,
            "access:grant:manage" => Self::AccessGrantManage,
            "configuration:manage" => Self::ConfigurationManage,
            "country-profile:manage" => Self::CountryProfileManage,
            "appearance:manage" => Self::AppearanceManage,
            "appearance:preference:manage" => Self::AppearancePreferenceManage,
            "audit:read" => Self::AuditRead,
            _ => return None,
        })
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SourceWrite => "source:write",
            Self::MigrationRead => "migration:read",
            Self::MigrationWrite => "migration:write",
            Self::PersonRead => "person:read",
            Self::PersonSearch => "person:search",
            Self::MatchRequest => "match:request",
            Self::MatchEvidenceRead => "match:evidence:read",
            Self::ReceiptRead => "receipt:read",
            Self::ReviewRead => "review:read",
            Self::ReviewDecide => "review:decide",
            Self::IdentityLink => "identity:link",
            Self::IdentityMerge => "identity:merge",
            Self::IdentityReverse => "identity:reverse",
            Self::ExportRead => "export:read",
            Self::TranslationDraftRead => "translation:draft:read",
            Self::TranslationDraftWrite => "translation:draft:write",
            Self::TranslationReview => "translation:review",
            Self::TranslationPublish => "translation:publish",
            Self::AccessGrantManage => "access:grant:manage",
            Self::ConfigurationManage => "configuration:manage",
            Self::CountryProfileManage => "country-profile:manage",
            Self::AppearanceManage => "appearance:manage",
            Self::AppearancePreferenceManage => "appearance:preference:manage",
            Self::AuditRead => "audit:read",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ResourceScope {
    pub registry_id: String,
    pub organization_id: String,
    pub record_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeGrant {
    pub registry_id: String,
    pub organization_id: String,
    pub record_ids: Option<BTreeSet<String>>,
    pub roles: BTreeSet<Role>,
    /// Direct grants are reserved for integration-client permissions and reviewed exceptions.
    pub permissions: BTreeSet<Permission>,
}

impl ScopeGrant {
    pub fn permits_scope(&self, requested: &ResourceScope) -> bool {
        if self.registry_id != requested.registry_id
            || self.organization_id != requested.organization_id
        {
            return false;
        }
        match (&self.record_ids, &requested.record_id) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some(allowed), Some(record_id)) => allowed.contains(record_id),
        }
    }

    pub fn permits(&self, permission: Permission) -> bool {
        self.permissions.contains(&permission)
            || (permission == Permission::MigrationRead
                && self.permissions.contains(&Permission::MigrationWrite))
            || self
                .roles
                .iter()
                .any(|role| role.permissions().contains(&permission))
    }
}

/// Created only from an authenticated identity and server-side grants.
/// Fields stay private so handlers cannot widen authorization from request data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessContext {
    actor: ActorRef,
    grants: Vec<ScopeGrant>,
    request_id: String,
    correlation_id: Option<String>,
    purpose: Option<String>,
}

impl AccessContext {
    pub(crate) fn from_authorized_identity(
        actor: ActorRef,
        grants: Vec<ScopeGrant>,
        request_id: String,
        correlation_id: Option<String>,
        purpose: Option<String>,
    ) -> Option<Self> {
        if actor.id.trim().is_empty()
            || (actor.kind == ActorKind::Human
                && actor.issuer_url.as_deref().is_none_or(str::is_empty))
            || request_id.trim().is_empty()
            || grants.iter().any(|grant| {
                grant.registry_id.trim().is_empty()
                    || grant.organization_id.trim().is_empty()
                    || (actor.kind == ActorKind::Human
                        && grant.roles.contains(&Role::IntegrationClient))
                    || (actor.kind == ActorKind::IntegrationClient
                        && grant
                            .roles
                            .iter()
                            .any(|role| *role != Role::IntegrationClient))
            })
        {
            return None;
        }

        Some(Self {
            actor,
            grants,
            request_id,
            correlation_id,
            purpose,
        })
    }

    pub fn actor(&self) -> &ActorRef {
        &self.actor
    }

    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    pub fn correlation_id(&self) -> Option<&str> {
        self.correlation_id.as_deref()
    }

    pub fn purpose(&self) -> Option<&str> {
        self.purpose.as_deref()
    }

    pub fn grants(&self) -> &[ScopeGrant] {
        &self.grants
    }

    pub(crate) fn only_permission(&self, permission: Permission) -> Option<Self> {
        let grants = self
            .grants
            .iter()
            .filter_map(|grant| {
                if !grant.permissions.contains(&permission) {
                    return None;
                }
                let mut restricted = grant.clone();
                restricted.roles.clear();
                restricted.permissions.clear();
                restricted.permissions.insert(permission);
                Some(restricted)
            })
            .collect::<Vec<_>>();
        (!grants.is_empty()).then(|| Self {
            actor: self.actor.clone(),
            grants,
            request_id: self.request_id.clone(),
            correlation_id: self.correlation_id.clone(),
            purpose: self.purpose.clone(),
        })
    }

    pub(crate) fn only_organization_scope(
        &self,
        registry_id: &str,
        organization_id: &str,
    ) -> Option<Self> {
        let grants = self
            .grants
            .iter()
            .filter(|grant| {
                grant.registry_id == registry_id && grant.organization_id == organization_id
            })
            .cloned()
            .collect::<Vec<_>>();
        (!grants.is_empty()).then(|| Self {
            actor: self.actor.clone(),
            grants,
            request_id: self.request_id.clone(),
            correlation_id: self.correlation_id.clone(),
            purpose: self.purpose.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(org: &str, record: Option<&str>) -> ResourceScope {
        ResourceScope {
            registry_id: "registry-a".to_owned(),
            organization_id: org.to_owned(),
            record_id: record.map(str::to_owned),
        }
    }

    #[test]
    fn scope_grant_is_an_intersection_and_record_restrictions_are_narrower() {
        let grant = ScopeGrant {
            registry_id: "registry-a".to_owned(),
            organization_id: "org-east".to_owned(),
            record_ids: Some(BTreeSet::from(["record-1".to_owned()])),
            roles: BTreeSet::from([Role::Registrar]),
            permissions: BTreeSet::new(),
        };
        assert!(grant.permits_scope(&scope("org-east", Some("record-1"))));
        assert!(!grant.permits_scope(&scope("org-west", Some("record-1"))));
        assert!(!grant.permits_scope(&scope("org-east", Some("record-2"))));
        assert!(!grant.permits_scope(&scope("org-east", None)));
    }

    #[test]
    fn role_permissions_are_least_privilege_and_integration_is_explicit() {
        let registrar = ScopeGrant {
            registry_id: "r".to_owned(),
            organization_id: "o".to_owned(),
            record_ids: None,
            roles: BTreeSet::from([Role::Registrar]),
            permissions: BTreeSet::new(),
        };
        assert!(registrar.permits(Permission::AppearancePreferenceManage));
        assert!(registrar.permits(Permission::SourceWrite));
        assert!(!registrar.permits(Permission::MigrationWrite));
        assert!(!registrar.permits(Permission::PersonSearch));
        assert!(!registrar.permits(Permission::ExportRead));

        let integration = ScopeGrant {
            roles: BTreeSet::from([Role::IntegrationClient]),
            permissions: BTreeSet::from([Permission::SourceWrite, Permission::MatchRequest]),
            ..registrar
        };
        assert!(integration.permits(Permission::SourceWrite));
        assert!(!integration.permits(Permission::MigrationWrite));
        assert!(integration.permits(Permission::MatchRequest));
        assert!(!integration.permits(Permission::AppearancePreferenceManage));
        assert!(!integration.permits(Permission::PersonSearch));
        assert_eq!(
            Permission::parse("appearance:preference:manage"),
            Some(Permission::AppearancePreferenceManage)
        );
    }

    #[test]
    fn migration_write_is_a_separately_scoped_explicit_permission() {
        assert_eq!(
            Permission::parse("migration:read"),
            Some(Permission::MigrationRead)
        );
        assert_eq!(Permission::MigrationRead.as_str(), "migration:read");
        assert_eq!(
            Permission::parse("migration:write"),
            Some(Permission::MigrationWrite)
        );
        assert_eq!(Permission::MigrationWrite.as_str(), "migration:write");

        let source_writer = ScopeGrant {
            registry_id: "r".to_owned(),
            organization_id: "o".to_owned(),
            record_ids: None,
            roles: BTreeSet::new(),
            permissions: BTreeSet::from([Permission::SourceWrite]),
        };
        assert!(source_writer.permits(Permission::SourceWrite));
        assert!(!source_writer.permits(Permission::MigrationWrite));

        let migration_writer = ScopeGrant {
            permissions: BTreeSet::from([Permission::MigrationWrite]),
            ..source_writer
        };
        assert!(migration_writer.permits(Permission::MigrationWrite));
        assert!(migration_writer.permits(Permission::MigrationRead));
        assert!(!migration_writer.permits(Permission::SourceWrite));

        let migration_reader = ScopeGrant {
            permissions: BTreeSet::from([Permission::MigrationRead]),
            ..migration_writer.clone()
        };
        assert!(migration_reader.permits(Permission::MigrationRead));
        assert!(!migration_reader.permits(Permission::MigrationWrite));
    }

    #[test]
    fn context_rejects_empty_actor_and_role_kind_mismatch() {
        let grant = ScopeGrant {
            registry_id: "r".to_owned(),
            organization_id: "o".to_owned(),
            record_ids: None,
            roles: BTreeSet::from([Role::IntegrationClient]),
            permissions: BTreeSet::new(),
        };
        assert!(
            AccessContext::from_authorized_identity(
                ActorRef {
                    kind: ActorKind::Human,
                    id: "h-1".to_owned(),
                    issuer_url: Some("https://issuer.example".to_owned()),
                },
                vec![grant.clone()],
                "req-1".to_owned(),
                None,
                None,
            )
            .is_none()
        );
        assert!(
            AccessContext::from_authorized_identity(
                ActorRef {
                    kind: ActorKind::IntegrationClient,
                    id: String::new(),
                    issuer_url: None,
                },
                vec![grant],
                "req-1".to_owned(),
                None,
                None,
            )
            .is_none()
        );
    }
}
