//! Registry-owned identities and source assertions.

use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceAssertion {
    pub field_path: String,
    /// The original value exactly as provided by the client, including script and partial dates.
    pub original_value: String,
    pub language_tag: Option<String>,
    pub script: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentifierAssertion {
    pub authority: String,
    pub identifier_type: String,
    pub value: String,
    pub authoritative: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRecordCommand {
    pub registry_id: String,
    pub organization_id: String,
    pub source_system_id: String,
    pub source_record_key: String,
    /// Opaque until a source-specific ordering contract exists.
    pub source_version: Option<String>,
    pub assertions: Vec<SourceAssertion>,
    pub identifiers: Vec<IdentifierAssertion>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonView {
    pub public_id: String,
    pub resource_version: i64,
    pub source_record_ids: Vec<String>,
    pub assertions: Vec<SourceAssertionView>,
    pub aliases: Vec<String>,
    pub identifiers: Vec<IdentifierView>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentifierView {
    pub authority: String,
    pub identifier_type: String,
    pub value: String,
    pub authoritative: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceAssertionView {
    pub source_record_id: String,
    pub source_system_id: String,
    pub source_version: Option<String>,
    pub receive_version: i64,
    pub field_path: String,
    pub original_value: String,
    pub language_tag: Option<String>,
    pub script: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestionReceipt {
    pub person_public_id: String,
    pub source_record_id: String,
    pub source_receive_version: i64,
    pub outcome: IngestionOutcome,
    pub replayed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestionOutcome {
    Accepted,
    SourceConflict,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryValidationError {
    InvalidField,
    TooManyAssertions,
    DuplicateAssertion,
    DuplicateIdentifier,
}

impl SourceRecordCommand {
    pub fn validate(&self) -> Result<(), RegistryValidationError> {
        for value in [
            &self.registry_id,
            &self.organization_id,
            &self.source_system_id,
            &self.source_record_key,
        ] {
            if value.trim().is_empty() || value.len() > 256 {
                return Err(RegistryValidationError::InvalidField);
            }
        }
        if self
            .source_version
            .as_ref()
            .is_some_and(|value| value.is_empty() || value.len() > 512)
        {
            return Err(RegistryValidationError::InvalidField);
        }
        if self.assertions.is_empty() || self.assertions.len() > 256 {
            return Err(RegistryValidationError::TooManyAssertions);
        }
        let mut paths = BTreeSet::new();
        for assertion in &self.assertions {
            if assertion.field_path.trim().is_empty()
                || assertion.field_path.len() > 256
                || assertion.original_value.len() > 16_384
                || assertion
                    .language_tag
                    .as_ref()
                    .is_some_and(|v| v.len() > 64)
                || assertion.script.as_ref().is_some_and(|v| v.len() > 32)
            {
                return Err(RegistryValidationError::InvalidField);
            }
            if !paths.insert(&assertion.field_path) {
                return Err(RegistryValidationError::DuplicateAssertion);
            }
        }
        if self.identifiers.len() > 64 {
            return Err(RegistryValidationError::InvalidField);
        }
        let mut identifiers = BTreeSet::new();
        for identifier in &self.identifiers {
            if identifier.authority.trim().is_empty()
                || identifier.authority.len() > 2048
                || identifier.identifier_type.trim().is_empty()
                || identifier.identifier_type.len() > 128
                || identifier.value.is_empty()
                || identifier.value.len() > 512
            {
                return Err(RegistryValidationError::InvalidField);
            }
            if !identifiers.insert((
                &identifier.authority,
                &identifier.identifier_type,
                &identifier.value,
            )) {
                return Err(RegistryValidationError::DuplicateIdentifier);
            }
        }
        Ok(())
    }
}
