//! Typed protocol-v2 component context and immutable proposal binding.
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentContext {
    pub scene: String,
    pub scope: PlacementScope,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PlacementScope {
    pub floor_id: String,
    pub region: [f64; 4],
    #[serde(default)]
    pub protected_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct ProposalBinding {
    pub proposal_id: String,
    pub proposal_revision: u64,
    pub base_scene_hash: String,
    pub prepare_token: String,
}

impl ProposalBinding {
    pub fn frame(
        &self,
        run_id: &str,
        request_id: &str,
        sequence: u64,
        action: &str,
        text: &str,
    ) -> serde_json::Value {
        json!({"version":2,"command":"feedback","run_id":run_id,"request_id":request_id,
               "feedback_id":format!("{run_id}.{sequence}"), "proposal_id":self.proposal_id,
               "proposal_revision":self.proposal_revision,"base_scene_hash":self.base_scene_hash,
               "prepare_token":self.prepare_token,"action":action,"text":text,"object_ids":[]})
    }
}
