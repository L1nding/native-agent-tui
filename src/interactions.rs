use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use serde_json::{json, Value};
use thiserror::Error;

use crate::protocol::RpcId;

/// Fixed, redacted reasons for actions taken by a noninteractive consumer.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "action", rename_all = "camelCase")]
pub enum HeadlessAction {
    DeclineApproval {
        request_id: RpcId,
        thread_id: String,
        turn_id: String,
    },
    InterruptForInput {
        request_id: RpcId,
        thread_id: String,
        turn_id: String,
    },
    InterruptForApproval {
        request_id: RpcId,
        thread_id: String,
        turn_id: String,
    },
    StopForOutput,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalDecision {
    Accept,
    Decline,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct InputOption {
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct InputQuestion {
    pub id: String,
    pub header: String,
    pub question: String,
    #[serde(default, rename = "isSecret")]
    pub is_secret: bool,
    #[serde(default)]
    pub options: Option<Vec<InputOption>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestKind {
    CommandApproval,
    FileApproval,
    UserInput { questions: Vec<InputQuestion> },
}

/// Identifies one accepted delivery, including reuse of an RPC ID in the same turn.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RequestRef {
    pub id: RpcId,
    pub thread_id: String,
    pub turn_id: String,
    pub received_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetailField {
    pub label: &'static str,
    pub text: String,
}

/// Live request context. Never included in the journal or diagnostic projection.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RequestDetails {
    pub item_id: Option<String>,
    pub started_at_ms: Option<i64>,
    pub is_blocking: Option<bool>,
    pub auto_resolution_ms: Option<u64>,
    pub fields: Vec<DetailField>,
    pub available_decisions: Option<Vec<String>>,
    pub file_preview: Option<FilePreview>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePreview {
    pub text: String,
    pub source_seq: u64,
    pub truncated: bool,
    pub unavailable: bool,
}

pub(crate) mod files;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestView {
    pub id: RpcId,
    pub thread_id: String,
    pub turn_id: String,
    pub summary: String,
    pub kind: RequestKind,
    pub allow_accept: bool,
    pub allow_decline: bool,
    pub allow_cancel: bool,
    pub responding: bool,
    pub received_seq: u64,
    pub details: RequestDetails,
}

#[derive(Debug, Error)]
pub enum InteractionError {
    #[error("unsupported request: {0}")]
    Unsupported(String),
    #[error("invalid request parameters: {0}")]
    Invalid(String),
    #[error("request has already been answered or resolved")]
    Resolved,
    #[error("this decision is not available for the request")]
    DecisionUnavailable,
}

impl RequestView {
    pub fn reference(&self) -> RequestRef {
        RequestRef {
            id: self.id.clone(),
            thread_id: self.thread_id.clone(),
            turn_id: self.turn_id.clone(),
            received_seq: self.received_seq,
        }
    }

    pub fn matches(&self, reference: &RequestRef) -> bool {
        self.id == reference.id
            && self.thread_id == reference.thread_id
            && self.turn_id == reference.turn_id
            && self.received_seq == reference.received_seq
    }

    pub fn decode(id: RpcId, method: &str, params: &Value) -> Result<Self, InteractionError> {
        if params.to_string().len() > 32 * 1024 {
            return Err(InteractionError::Invalid("request exceeds 32 KiB".into()));
        }
        let string = |key: &str| {
            params
                .get(key)
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(str::to_owned)
                .ok_or_else(|| InteractionError::Invalid(format!("missing {key}")))
        };
        let thread_id = string("threadId")?;
        let turn_id = string("turnId")?;
        let kind =
            match method {
                "item/commandExecution/requestApproval" => {
                    if params.get("kind").is_some_and(|kind| {
                        !matches!(kind.as_str(), Some("command" | "writeStdin"))
                    }) {
                        return Err(InteractionError::Invalid(
                            "unsupported command approval kind".into(),
                        ));
                    }
                    RequestKind::CommandApproval
                }
                "item/fileChange/requestApproval" => RequestKind::FileApproval,
                "item/tool/requestUserInput" => {
                    let questions: Vec<InputQuestion> =
                        serde_json::from_value(params.get("questions").cloned().ok_or_else(
                            || InteractionError::Invalid("missing questions".into()),
                        )?)
                        .map_err(|error| InteractionError::Invalid(error.to_string()))?;
                    if questions.is_empty() || questions.len() > 16 {
                        return Err(InteractionError::Invalid("expected 1-16 questions".into()));
                    }
                    let mut ids = BTreeSet::new();
                    if questions
                        .iter()
                        .any(|question| question.id.trim().is_empty() || !ids.insert(&question.id))
                    {
                        return Err(InteractionError::Invalid(
                            "question IDs must be nonempty and unique".into(),
                        ));
                    }
                    RequestKind::UserInput { questions }
                }
                _ => return Err(InteractionError::Unsupported(method.into())),
            };
        let summary = match &kind {
            RequestKind::CommandApproval => format!(
                "{}\n{}",
                params["command"].as_str().unwrap_or("Command approval"),
                params["reason"].as_str().unwrap_or("")
            ),
            RequestKind::FileApproval => params["reason"]
                .as_str()
                .unwrap_or("Approve file changes")
                .to_owned(),
            RequestKind::UserInput { questions } => questions[0].question.clone(),
        };
        let decisions = match params.get("availableDecisions") {
            None | Some(Value::Null) => None,
            Some(Value::Array(values)) => Some(values),
            _ => {
                return Err(InteractionError::Invalid(
                    "invalid availableDecisions".into(),
                ))
            }
        };
        let allows = |decision: &str| {
            decisions.is_none_or(|values| values.iter().any(|v| v.as_str() == Some(decision)))
        };
        let mut details = RequestDetails::decode(params, decisions)?;
        if matches!(kind, RequestKind::CommandApproval)
            && !details
                .fields
                .iter()
                .any(|field| field.label == "Operation")
        {
            details.fields.insert(
                0,
                DetailField {
                    label: "Operation",
                    text: "command (protocol default)".into(),
                },
            );
        }
        Ok(Self {
            id,
            thread_id,
            turn_id,
            summary,
            kind,
            allow_accept: allows("accept"),
            allow_decline: allows("decline"),
            allow_cancel: allows("cancel"),
            responding: false,
            received_seq: 0, // Core stamps this when accepting a request.
            details,
        })
    }

    pub fn approval_result(&self, decision: ApprovalDecision) -> Result<Value, InteractionError> {
        if self.responding {
            return Err(InteractionError::Resolved);
        }
        if matches!(self.kind, RequestKind::UserInput { .. })
            || (decision == ApprovalDecision::Accept && !self.allow_accept)
            || (decision == ApprovalDecision::Decline && !self.allow_decline)
            || (decision == ApprovalDecision::Cancel && !self.allow_cancel)
        {
            return Err(InteractionError::DecisionUnavailable);
        }
        let decision = match decision {
            ApprovalDecision::Accept => "accept",
            ApprovalDecision::Decline => "decline",
            ApprovalDecision::Cancel => "cancel",
        };
        Ok(json!({"decision": decision}))
    }

    pub fn input_result(
        &self,
        answers: &BTreeMap<String, Vec<String>>,
    ) -> Result<Value, InteractionError> {
        self.validate_input_answers(answers, true)?;
        Ok(
            json!({"answers":answers.iter().map(|(id, answers)| (id.clone(), json!({"answers":answers}))).collect::<serde_json::Map<_,_>>()}),
        )
    }

    /// Checks partial drafts as well as final answers without exposing protocol JSON to UI.
    pub fn validate_input_answers(
        &self,
        answers: &BTreeMap<String, Vec<String>>,
        complete: bool,
    ) -> Result<(), InteractionError> {
        if self.responding {
            return Err(InteractionError::Resolved);
        }
        let RequestKind::UserInput { questions } = &self.kind else {
            return Err(InteractionError::DecisionUnavailable);
        };
        if serde_json::to_vec(answers)
            .map_err(|_| InteractionError::Invalid("could not encode answers".into()))?
            .len()
            > 32 * 1024
        {
            return Err(InteractionError::Invalid("answers exceed 32 KiB".into()));
        }
        if (complete && answers.len() != questions.len())
            || answers
                .keys()
                .any(|id| !questions.iter().any(|q| &q.id == id))
        {
            return Err(InteractionError::Invalid(
                "answer every question using its ID".into(),
            ));
        }
        Ok(())
    }
}

impl RequestDetails {
    fn decode(params: &Value, decisions: Option<&Vec<Value>>) -> Result<Self, InteractionError> {
        let optional_string = |key: &str| match params.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(text)) => Ok(Some(text.clone())),
            _ => Err(InteractionError::Invalid(format!("invalid {key}"))),
        };
        let started_at_ms = match params.get("startedAtMs") {
            None | Some(Value::Null) => None,
            Some(value) => Some(
                value
                    .as_i64()
                    .ok_or_else(|| InteractionError::Invalid("invalid startedAtMs".into()))?,
            ),
        };
        let is_blocking = match params.get("isBlocking") {
            None | Some(Value::Null) => None,
            Some(value) => Some(
                value
                    .as_bool()
                    .ok_or_else(|| InteractionError::Invalid("invalid isBlocking".into()))?,
            ),
        };
        let auto_resolution_ms = match params.get("autoResolutionMs") {
            None | Some(Value::Null) => None,
            Some(value) => Some(
                value
                    .as_u64()
                    .ok_or_else(|| InteractionError::Invalid("invalid autoResolutionMs".into()))?,
            ),
        };
        let mut fields = Vec::new();
        for (key, label) in [
            ("kind", "Operation"),
            ("command", "Command"),
            ("cwd", "Command cwd"),
            ("reason", "Reason"),
            ("approvalId", "Approval callback"),
            ("environmentId", "Environment"),
            ("grantRoot", "Proposed session write root (not applied)"),
        ] {
            if let Some(text) = optional_string(key)? {
                fields.push(DetailField { label, text });
            }
        }
        // Decode display excerpts at the protocol edge; views never inspect JSON.
        // These proposals do not grant permission or enable a new decision.
        for (key, label) in [
            ("commandActions", "Server command actions"),
            ("networkApprovalContext", "Network approval context"),
            (
                "additionalPermissions",
                "Additional permission proposal (not applied)",
            ),
            (
                "proposedExecpolicyAmendment",
                "Execution policy proposal (not applied)",
            ),
            (
                "proposedNetworkPolicyAmendments",
                "Network policy proposals (not applied)",
            ),
        ] {
            if let Some(value) = params.get(key).filter(|value| !value.is_null()) {
                fields.push(DetailField {
                    label,
                    text: value.to_string(),
                });
            }
        }
        Ok(Self {
            item_id: optional_string("itemId")?,
            started_at_ms,
            is_blocking,
            auto_resolution_ms,
            fields,
            available_decisions: decisions.map(|values| {
                values
                    .iter()
                    .map(|value| {
                        value
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| value.to_string())
                    })
                    .collect()
            }),
            file_preview: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_context_keeps_proposals_explicit_and_rejects_invalid_identity_field_types() {
        let params = json!({"threadId":"t","turnId":"u","itemId":"i","kind":"writeStdin","startedAtMs":-1,"command":null,"cwd":null,"approvalId":"callback","additionalPermissions":{"network":{"enabled":true}},"availableDecisions":["acceptForSession",{"acceptWithExecpolicyAmendment":{"execpolicy_amendment":["echo"]}}]});
        let request = RequestView::decode(
            RpcId::Number(1),
            "item/commandExecution/requestApproval",
            &params,
        )
        .unwrap();
        assert_eq!(request.details.started_at_ms, Some(-1));
        assert!(request
            .details
            .fields
            .iter()
            .any(|field| field.label == "Operation" && field.text == "writeStdin"));
        assert!(request
            .details
            .fields
            .iter()
            .any(|field| field.label.contains("not applied") && field.text.contains("network")));
        assert!(request.approval_result(ApprovalDecision::Accept).is_err());
        assert!(request.approval_result(ApprovalDecision::Decline).is_err());
        for (key, value) in [
            ("kind", json!("newUnknownOperation")),
            ("itemId", json!(123)),
            ("startedAtMs", json!("bad")),
            ("availableDecisions", json!({})),
            ("cwd", json!([])),
        ] {
            let mut invalid = params.clone();
            invalid[key] = value;
            assert!(RequestView::decode(
                RpcId::Number(1),
                "item/commandExecution/requestApproval",
                &invalid
            )
            .is_err());
        }
    }

    #[test]
    fn approvals_respect_available_decisions_and_are_not_user_input() {
        let request = RequestView::decode(
            RpcId::Number(1),
            "item/commandExecution/requestApproval",
            &json!({"threadId":"t","turnId":"u","availableDecisions":["decline"]}),
        )
        .unwrap();
        assert!(request.approval_result(ApprovalDecision::Accept).is_err());
        assert_eq!(
            request.approval_result(ApprovalDecision::Decline).unwrap(),
            json!({"decision":"decline"})
        );
        assert!(request.approval_result(ApprovalDecision::Cancel).is_err());
    }

    #[test]
    fn cancel_respects_the_advertised_decisions_and_submission_state() {
        for method in [
            "item/commandExecution/requestApproval",
            "item/fileChange/requestApproval",
        ] {
            let mut request = RequestView::decode(
                RpcId::Number(1),
                method,
                &json!({"threadId":"t","turnId":"u","availableDecisions":["accept","cancel"]}),
            )
            .unwrap();
            assert!(request.allow_cancel);
            assert!(!request.allow_decline);
            assert_eq!(
                request.approval_result(ApprovalDecision::Cancel).unwrap(),
                json!({"decision":"cancel"})
            );
            assert!(request.approval_result(ApprovalDecision::Decline).is_err());
            request.responding = true;
            assert!(request.approval_result(ApprovalDecision::Cancel).is_err());
            for decisions in [
                json!([]),
                json!(["acceptForSession"]),
                json!([{"acceptWithExecpolicyAmendment":{"execpolicy_amendment":["cmd"]}}]),
            ] {
                let request = RequestView::decode(
                    RpcId::Number(1),
                    method,
                    &json!({"threadId":"t","turnId":"u","availableDecisions":decisions}),
                )
                .unwrap();
                assert!(!request.allow_cancel);
                assert!(request.approval_result(ApprovalDecision::Cancel).is_err());
            }
        }
        let input = RequestView::decode(RpcId::Number(1), "item/tool/requestUserInput",
            &json!({"threadId":"t","turnId":"u","questions":[{"id":"a","header":"H","question":"Q"}]})).unwrap();
        assert!(input.approval_result(ApprovalDecision::Cancel).is_err());
    }

    #[test]
    fn input_hints_are_optional_typed_server_facts() {
        let params = json!({"threadId":"t","turnId":"u","isBlocking":false,"autoResolutionMs":0,
            "questions":[{"id":"a","header":"H","question":"Q"}]});
        let decode = |params: &Value| {
            RequestView::decode(RpcId::Number(1), "item/tool/requestUserInput", params)
        };
        let request = decode(&params).unwrap();
        assert_eq!(request.details.is_blocking, Some(false));
        assert_eq!(request.details.auto_resolution_ms, Some(0));
        let mut missing = params.clone();
        missing.as_object_mut().unwrap().remove("isBlocking");
        missing["autoResolutionMs"] = Value::Null;
        let request = decode(&missing).unwrap();
        assert_eq!(request.details.is_blocking, None);
        assert_eq!(request.details.auto_resolution_ms, None);
        for (field, value) in [
            ("isBlocking", json!("false")),
            ("isBlocking", json!(0)),
            ("autoResolutionMs", json!(-1)),
            ("autoResolutionMs", json!(0.5)),
            ("autoResolutionMs", json!("100")),
        ] {
            let mut invalid = params.clone();
            invalid[field] = value;
            assert!(decode(&invalid).is_err());
        }
    }

    #[test]
    fn answers_use_each_question_id_and_private_content_is_not_summarized() {
        let request = RequestView::decode(RpcId::String("input".into()), "item/tool/requestUserInput",
            &json!({"threadId":"t","turnId":"u","questions":[{"id":"a","header":"Choice","question":"Pick","isSecret":true}]})).unwrap();
        assert!(request.input_result(&BTreeMap::new()).is_err());
        let answers = BTreeMap::from([("a".into(), vec!["private".into()])]);
        assert_eq!(
            request.input_result(&answers).unwrap(),
            json!({"answers":{"a":{"answers":["private"]}}})
        );
    }

    #[test]
    fn rejects_duplicate_question_ids_and_oversized_answers() {
        let params = json!({"threadId":"t","turnId":"u","questions":[{"id":"a","header":"First","question":"Pick"},{"id":"a","header":"Second","question":"Pick"}]});
        assert!(
            RequestView::decode(RpcId::Number(1), "item/tool/requestUserInput", &params).is_err()
        );
        let params = json!({"threadId":"t","turnId":"u","questions":[{"id":"a","header":"First","question":"Pick"}]});
        let request =
            RequestView::decode(RpcId::Number(1), "item/tool/requestUserInput", &params).unwrap();
        assert!(request
            .input_result(&BTreeMap::from([("a".into(), vec!["x".repeat(32 * 1024)])]))
            .is_err());
    }
}
