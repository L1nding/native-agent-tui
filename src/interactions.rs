use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RequestId(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingRequest {
    Approval { request_id: RequestId },
    UserInput { request_id: RequestId },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    UnknownRequest,
    AlreadyResolved,
}

#[derive(Debug, Default)]
pub struct RequestStore {
    pending: BTreeMap<RequestId, PendingRequest>,
}

impl RequestStore {
    pub fn insert(&mut self, request: PendingRequest) -> bool {
        let id = match &request {
            PendingRequest::Approval { request_id } | PendingRequest::UserInput { request_id } => {
                request_id.clone()
            }
        };
        self.pending.insert(id, request).is_none()
    }

    pub fn resolve(&mut self, request_id: &RequestId) -> Result<PendingRequest, ResolveError> {
        self.pending
            .remove(request_id)
            .ok_or(ResolveError::AlreadyResolved)
    }

    pub fn contains(&self, request_id: &RequestId) -> bool {
        self.pending.contains_key(request_id)
    }

    pub fn len(&self) -> usize {
        self.pending.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::{PendingRequest, RequestId, RequestStore, ResolveError};

    #[test]
    fn request_is_resolved_once() {
        let id = RequestId("r1".into());
        let mut store = RequestStore::default();
        assert!(store.insert(PendingRequest::Approval {
            request_id: id.clone(),
        }));
        assert!(store.contains(&id));
        assert!(store.resolve(&id).is_ok());
        assert_eq!(store.resolve(&id), Err(ResolveError::AlreadyResolved));
    }
}
