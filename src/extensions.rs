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
pub struct ModelingConfirmation {
    pub proposal: crate::domain::ProposalBinding,
    pub review_set_sha256: Option<String>,
}

impl From<crate::domain::ProposalBinding> for ModelingConfirmation {
    fn from(proposal: crate::domain::ProposalBinding) -> Self {
        Self {
            proposal,
            review_set_sha256: None,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum ExtensionConfirmation {
    Modeling(ModelingConfirmation),
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
                .field("proposal_id", &binding.proposal.proposal_id)
                .field("revision", &binding.proposal.revision)
                .finish_non_exhaustive(),
        }
    }
}
