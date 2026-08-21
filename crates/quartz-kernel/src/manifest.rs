use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

use crate::{Error, Result};

pub const ABI_VERSION: u32 = 11;
pub const MANIFEST_SECTION: &str = "quartz:manifest";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub abi: u32,
    pub module: String,
    pub version: String,
    pub execution_mode: String,
    pub requested_host_capabilities: BTreeSet<HostCapability>,
    pub component: ComponentDeclaration,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ComponentDeclaration {
    pub id: String,
    pub config_schema: String,
    pub max_activation_steps: u32,
    pub inject: Vec<RequiredBinding>,
    pub provide: Vec<ProvidedBinding>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RequiredBinding {
    pub slot: u64,
    pub kind: BindingKind,
    pub namespace: String,
    pub interface: String,
    pub min_revision: u32,
    pub max_revision: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProvidedBinding {
    pub slot: u64,
    pub kind: BindingKind,
    pub namespace: String,
    pub interface: String,
    pub revision: u32,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BindingKind {
    Callable,
    Value,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum HostCapability {
    AppendEvent,
    ApplyPatch,
    EventCount,
    EventPayloadByte,
    EventOutputWrite,
    EventPayloadLen,
    Exchange,
    Invoke,
    OpenEventStream,
    OpenExchange,
    OpenJournal,
    Publish,
    PublishCallable,
    ReadEvent,
    ReadSnapshot,
    RegisterChild,
    Resolve,
    ResumeEvent,
    ResumeEventOutput,
    ResumeExchange,
    ResumeSnapshot,
    SetState,
    WorkspaceRead,
    WorkspaceWrite,
    WorkspacePublish,
    WorkspacePromote,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct InterfaceId {
    pub kind: BindingKind,
    pub namespace: String,
    pub interface: String,
    pub revision: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Requirement {
    pub kind: BindingKind,
    pub namespace: String,
    pub interface: String,
    pub min_revision: u32,
    pub max_revision: u32,
}

impl Manifest {
    pub fn validate(&self) -> Result<()> {
        if self.abi != ABI_VERSION {
            return Err(Error::AbiVersion {
                expected: ABI_VERSION,
                actual: self.abi,
            });
        }
        if self.execution_mode != "wasm-component" {
            return Err(Error::Manifest(
                "execution_mode must be wasm-component".into(),
            ));
        }
        if self.module.is_empty() || self.version.is_empty() || self.component.id.is_empty() {
            return Err(Error::Manifest(
                "module, version, and component id must be non-empty".into(),
            ));
        }
        if self.component.config_schema != "u64" {
            return Err(Error::Manifest(
                "the current ABI supports only the u64 config schema".into(),
            ));
        }
        if self.component.max_activation_steps == 0 {
            return Err(Error::Manifest(
                "max_activation_steps must be positive".into(),
            ));
        }

        let mut inject_slots = BTreeSet::new();
        let mut provide_slots = BTreeSet::new();
        let mut provided = BTreeSet::new();
        for requirement in &self.component.inject {
            if requirement.namespace.is_empty()
                || requirement.interface.is_empty()
                || requirement.min_revision > requirement.max_revision
            {
                return Err(Error::Manifest(
                    "invalid injected interface declaration".into(),
                ));
            }
            if !inject_slots.insert(requirement.slot) {
                return Err(Error::Manifest(format!(
                    "duplicate injected slot {}",
                    requirement.slot
                )));
            }
        }
        for provision in &self.component.provide {
            if provision.namespace.is_empty() || provision.interface.is_empty() {
                return Err(Error::Manifest(
                    "invalid provided interface declaration".into(),
                ));
            }
            if !provide_slots.insert(provision.slot) {
                return Err(Error::Manifest(format!(
                    "duplicate provided slot {}",
                    provision.slot
                )));
            }
            if !provided.insert(provision.interface_id()) {
                return Err(Error::Manifest("duplicate provided interface".into()));
            }
        }
        Ok(())
    }

    pub fn required_by_slot(&self) -> BTreeMap<u64, Requirement> {
        self.component
            .inject
            .iter()
            .map(|binding| (binding.slot, binding.requirement()))
            .collect()
    }

    pub fn provided_by_slot(&self) -> BTreeMap<u64, InterfaceId> {
        self.component
            .provide
            .iter()
            .map(|binding| (binding.slot, binding.interface_id()))
            .collect()
    }

    pub fn requests(&self, capability: HostCapability) -> bool {
        self.requested_host_capabilities.contains(&capability)
    }
}

impl RequiredBinding {
    pub fn requirement(&self) -> Requirement {
        Requirement {
            kind: self.kind,
            namespace: self.namespace.clone(),
            interface: self.interface.clone(),
            min_revision: self.min_revision,
            max_revision: self.max_revision,
        }
    }
}

impl ProvidedBinding {
    pub fn interface_id(&self) -> InterfaceId {
        InterfaceId {
            kind: self.kind,
            namespace: self.namespace.clone(),
            interface: self.interface.clone(),
            revision: self.revision,
        }
    }
}

impl Requirement {
    pub fn accepts(&self, provided: &InterfaceId) -> bool {
        self.kind == provided.kind
            && self.namespace == provided.namespace
            && self.interface == provided.interface
            && (self.min_revision..=self.max_revision).contains(&provided.revision)
    }

    pub fn name(&self) -> (&str, &str) {
        (&self.namespace, &self.interface)
    }
}
