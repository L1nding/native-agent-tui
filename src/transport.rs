use std::sync::{Arc, Mutex};

use crate::protocol::Envelope;
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TransportError {
    #[error("transport is closed")]
    Closed,
    #[error("transport failure: {0}")]
    Failed(String),
}

/// The single-writer boundary for app-server input and output.
pub trait TransportAdapter: Send + Sync + 'static {
    fn send(&self, envelope: Envelope) -> Result<(), TransportError>;
    fn close(&self) -> Result<(), TransportError>;
}

#[derive(Debug, Default, Clone)]
pub struct ScriptedTransport {
    sent: Arc<Mutex<Vec<Envelope>>>,
    closed: Arc<Mutex<bool>>,
}

impl ScriptedTransport {
    pub fn sent(&self) -> Vec<Envelope> {
        self.sent.lock().expect("transport mutex poisoned").clone()
    }

    pub fn is_closed(&self) -> bool {
        *self.closed.lock().expect("transport mutex poisoned")
    }
}

impl TransportAdapter for ScriptedTransport {
    fn send(&self, envelope: Envelope) -> Result<(), TransportError> {
        if self.is_closed() {
            return Err(TransportError::Closed);
        }
        self.sent
            .lock()
            .map_err(|_| TransportError::Failed("transport mutex poisoned".into()))?
            .push(envelope);
        Ok(())
    }

    fn close(&self) -> Result<(), TransportError> {
        *self
            .closed
            .lock()
            .map_err(|_| TransportError::Failed("transport mutex poisoned".into()))? = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{ScriptedTransport, TransportAdapter, TransportError};
    use crate::protocol::{Envelope, RpcId};

    #[test]
    fn scripted_transport_records_order_and_rejects_writes_after_close() {
        let transport = ScriptedTransport::default();
        transport
            .send(Envelope::request(RpcId::Number(1), "initialize", None))
            .unwrap();
        transport.close().unwrap();
        assert_eq!(transport.sent().len(), 1);
        assert_eq!(
            transport.send(Envelope::notification("initialized", None)),
            Err(TransportError::Closed)
        );
    }
}
