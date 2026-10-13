//! Typed domain projections and confirmations. Execution facts remain Core-owned.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionId {
    Modeling,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtensionSnapshot {
    Modeling(crate::domain::DomainSnapshot),
}

impl ExtensionSnapshot {
    pub fn id(&self) -> ExtensionId {
        match self {
            Self::Modeling(_) => ExtensionId::Modeling,
        }
    }

    pub fn modeling(&self) -> Option<&crate::domain::DomainSnapshot> {
        match self {
            Self::Modeling(snapshot) => Some(snapshot),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum ExtensionConfirmation {
    Modeling(crate::domain::ProposalBinding),
}

impl ExtensionConfirmation {
    pub fn id(&self) -> ExtensionId {
        match self {
            Self::Modeling(_) => ExtensionId::Modeling,
        }
    }
}

impl std::fmt::Debug for ExtensionConfirmation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Modeling(binding) => f
                .debug_struct("ModelingConfirmation")
                .field("proposal_id", &binding.proposal_id)
                .field("revision", &binding.revision)
                .finish_non_exhaustive(),
        }
    }
}
