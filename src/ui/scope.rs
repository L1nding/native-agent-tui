//! Confirmed thread relationships shared by conversation search and evidence views.
use crate::state::CoreSnapshot;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Scope {
    #[default]
    Thread,
    Subtree,
    Path,
    All,
}

impl Scope {
    pub fn next(self) -> Self {
        match self {
            Self::Thread => Self::Subtree,
            Self::Subtree => Self::Path,
            Self::Path => Self::All,
            Self::All => Self::Thread,
        }
    }

    pub fn threads(self, thread: &str, source: &CoreSnapshot) -> Vec<String> {
        if self == Self::All {
            return Vec::new();
        }
        let mut threads = vec![thread.to_owned()];
        match self {
            Self::Subtree => {
                for _ in 0..source.agents.len() {
                    let mut changed = false;
                    for agent in &source.agents {
                        if agent.info.confirmed
                            && threads.contains(&agent.info.parent_id)
                            && !threads.contains(&agent.info.id)
                        {
                            threads.push(agent.info.id.clone());
                            changed = true;
                        }
                    }
                    if !changed {
                        break;
                    }
                }
            }
            Self::Path => {
                for _ in 0..source.agents.len() {
                    let current = threads.last().unwrap();
                    let Some(agent) = source
                        .agents
                        .iter()
                        .find(|agent| agent.info.confirmed && &agent.info.id == current)
                    else {
                        break;
                    };
                    if threads.contains(&agent.info.parent_id) {
                        break;
                    }
                    threads.push(agent.info.parent_id.clone());
                }
            }
            _ => {}
        }
        threads
    }
}
