use thiserror::Error;

use super::{AccessContext, Permission, ResourceScope};

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum AccessDenied {
    #[error("access denied")]
    Unauthenticated,
    #[error("access denied")]
    OutOfScope,
    #[error("access denied")]
    PermissionMissing,
}

#[derive(Debug, Error)]
pub enum AccessError {
    #[error(transparent)]
    Denied(#[from] AccessDenied),
    #[error("access policy unavailable")]
    Unavailable,
}

/// A use-case boundary helper; handlers must call it before loading protected data.
pub fn authorize(
    context: Option<&AccessContext>,
    permission: Permission,
    resource: &ResourceScope,
) -> Result<(), AccessDenied> {
    let context = context.ok_or(AccessDenied::Unauthenticated)?;
    if context
        .grants()
        .iter()
        .any(|grant| grant.permits_scope(resource) && grant.permits(permission))
    {
        Ok(())
    } else if context.grants().iter().any(|grant| {
        grant.registry_id == resource.registry_id
            && grant.organization_id == resource.organization_id
    }) {
        Err(AccessDenied::PermissionMissing)
    } else {
        Err(AccessDenied::OutOfScope)
    }
}

/// Infrastructure-backed check when grants need current revocation/assignment data.
pub trait AccessPolicy: Send + Sync + 'static {
    fn authorize(
        &self,
        context: Option<&AccessContext>,
        permission: Permission,
        resource: &ResourceScope,
    ) -> Result<(), AccessError> {
        authorize(context, permission, resource).map_err(Into::into)
    }
}

impl<T: Send + Sync + 'static> AccessPolicy for T {}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::access::{ActorKind, ActorRef, Role, ScopeGrant};

    fn context() -> AccessContext {
        AccessContext::from_authorized_identity(
            ActorRef {
                kind: ActorKind::Human,
                id: "synthetic-user".to_owned(),
                issuer_url: Some("https://issuer.example".to_owned()),
            },
            vec![ScopeGrant {
                registry_id: "registry-a".to_owned(),
                organization_id: "org-east".to_owned(),
                record_ids: None,
                roles: BTreeSet::from([Role::Registrar]),
                permissions: BTreeSet::new(),
            }],
            "request-1".to_owned(),
            None,
            None,
        )
        .unwrap()
    }

    #[test]
    fn unauthenticated_and_cross_organization_access_are_denied() {
        let allowed = ResourceScope {
            registry_id: "registry-a".to_owned(),
            organization_id: "org-east".to_owned(),
            record_id: None,
        };
        assert_eq!(
            authorize(None, Permission::SourceWrite, &allowed),
            Err(AccessDenied::Unauthenticated)
        );

        let cross_org = ResourceScope {
            organization_id: "org-west".to_owned(),
            ..allowed
        };
        assert_eq!(
            authorize(Some(&context()), Permission::SourceWrite, &cross_org),
            Err(AccessDenied::OutOfScope)
        );
    }

    #[test]
    fn export_denied_without_explicit_export_permission() {
        let scope = ResourceScope {
            registry_id: "registry-a".to_owned(),
            organization_id: "org-east".to_owned(),
            record_id: None,
        };
        assert_eq!(
            authorize(Some(&context()), Permission::ExportRead, &scope),
            Err(AccessDenied::PermissionMissing)
        );
    }
}
