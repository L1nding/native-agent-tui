use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use serde_json::{json, Value};
use thiserror::Error;

use crate::protocol::RpcId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalDecision {
    Accept,
    Decline,
}

impl ApprovalDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::Decline => "decline",
        }
    }
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestView {
    pub id: RpcId,
    pub thread_id: String,
    pub turn_id: String,
    pub summary: String,
    pub kind: RequestKind,
    pub allow_accept: bool,
    pub allow_decline: bool,
    pub responding: bool,
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
                "item/commandExecution/requestApproval" => RequestKind::CommandApproval,
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
        let decisions = params.get("availableDecisions").and_then(Value::as_array);
        let allows = |decision: &str| {
            decisions.is_none_or(|values| values.iter().any(|v| v.as_str() == Some(decision)))
        };
        Ok(Self {
            id,
            thread_id,
            turn_id,
            summary,
            kind,
            allow_accept: allows("accept"),
            allow_decline: allows("decline"),
            responding: false,
        })
    }

    pub fn approval_result(&self, decision: ApprovalDecision) -> Result<Value, InteractionError> {
        if self.responding {
            return Err(InteractionError::Resolved);
        }
        if matches!(self.kind, RequestKind::UserInput { .. })
            || (decision == ApprovalDecision::Accept && !self.allow_accept)
            || (decision == ApprovalDecision::Decline && !self.allow_decline)
        {
            return Err(InteractionError::DecisionUnavailable);
        }
        Ok(json!({"decision":decision.as_str()}))
    }

    pub fn input_result(
        &self,
        answers: &BTreeMap<String, Vec<String>>,
    ) -> Result<Value, InteractionError> {
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
        if answers.len() != questions.len()
            || questions.iter().any(|q| !answers.contains_key(&q.id))
        {
            return Err(InteractionError::Invalid(
                "answer every question using its ID".into(),
            ));
        }
        Ok(
            json!({"answers":answers.iter().map(|(id, answers)| (id.clone(), json!({"answers":answers}))).collect::<serde_json::Map<_,_>>()}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
