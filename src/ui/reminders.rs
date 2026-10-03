//! Local acknowledgement of silence reminders; execution facts stay in Core.
use crate::observation::{
    ActivityIdentity, ActivitySnapshot, AttentionLevel, ExecutionState, ObservationSnapshot,
};

#[derive(Default)]
pub(super) struct Reminders {
    acknowledged: Vec<Acknowledgement>,
}

struct Acknowledgement {
    session: String,
    epoch: String,
    activity: String,
    identity: ActivityIdentity,
    progress: u64,
    level: AttentionLevel,
}

impl Acknowledgement {
    fn matches(&self, activity: &ActivitySnapshot) -> bool {
        eligible(activity)
            && self.session == activity.session_id
            && self.epoch == activity.clock_epoch
            && self.activity == activity.activity_id
            && self.identity == activity.identity
            && self.progress == activity.progress_seq
            && self.level == activity.attention.level
    }
}

fn eligible(activity: &ActivitySnapshot) -> bool {
    !activity.attention.requires_action
        && matches!(
            activity.execution_state,
            ExecutionState::Running | ExecutionState::Waiting
        )
        && matches!(
            activity.attention.level,
            AttentionLevel::Quiet | AttentionLevel::AttentionNeeded
        )
}

impl Reminders {
    pub(super) fn is_acknowledged(&self, activity: &ActivitySnapshot) -> bool {
        self.acknowledged.iter().any(|ack| ack.matches(activity))
    }

    pub(super) fn sync(&mut self, observation: &ObservationSnapshot) {
        // Retain at most one token per current activity, bounded by Core's activity budget.
        self.acknowledged.retain(|ack| {
            observation
                .activities
                .iter()
                .any(|activity| ack.matches(activity))
        });
    }

    /// Returns None when no silence reminder can be acknowledged, otherwise the new state.
    pub(super) fn toggle_for_agent(
        &mut self,
        observation: &ObservationSnapshot,
        agent: &str,
    ) -> Option<bool> {
        self.sync(observation);
        let activities: Vec<_> = observation
            .activities
            .iter()
            .filter(|activity| activity.identity.agent_id == agent && eligible(activity))
            .collect();
        if activities.is_empty() {
            return None;
        }
        if activities
            .iter()
            .all(|activity| self.is_acknowledged(activity))
        {
            self.acknowledged
                .retain(|ack| ack.identity.agent_id != agent);
            return Some(false);
        }
        for activity in activities {
            if !self.is_acknowledged(activity) {
                self.acknowledged.push(Acknowledgement {
                    session: activity.session_id.clone(),
                    epoch: activity.clock_epoch.clone(),
                    activity: activity.activity_id.clone(),
                    identity: activity.identity.clone(),
                    progress: activity.progress_seq,
                    level: activity.attention.level,
                });
            }
        }
        Some(true)
    }
}
