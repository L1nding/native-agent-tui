use crate::protocol::RpcId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingRpc {
    pub id: RpcId,
    pub method: String,
}
