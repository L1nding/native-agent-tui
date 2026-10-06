//! Execution backend selection and protocol adapters.
//!
//! The Core talks to one small JSON-RPC envelope seam. Backend-specific
//! processes and protocol translation live below this module so the UI never
//! needs to inspect backend wire messages.

pub(crate) mod acp;
pub mod acp_protocol;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    Codex,
    DeepSeekAcp,
}

impl BackendKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::DeepSeekAcp => "deepseek-acp",
        }
    }

    pub fn is_acp(self) -> bool {
        matches!(self, Self::DeepSeekAcp)
    }
}

impl std::str::FromStr for BackendKind {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "codex" => Ok(Self::Codex),
            "deepseek-acp" => Ok(Self::DeepSeekAcp),
            _ => Err("expected codex or deepseek-acp"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::BackendKind;

    #[test]
    fn backend_names_are_stable_cli_values() {
        assert_eq!("codex".parse::<BackendKind>().unwrap(), BackendKind::Codex);
        assert_eq!(
            "deepseek-acp".parse::<BackendKind>().unwrap(),
            BackendKind::DeepSeekAcp
        );
        assert!("other".parse::<BackendKind>().is_err());
    }
}
