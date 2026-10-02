use std::sync::Arc;

use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinHandle;

use crate::protocol::{decode_line, encode_line, Envelope, MAX_LINE_BYTES};

const QUEUE_BYTES: usize = 64 * 1024 * 1024;
const QUEUE_FRAMES: usize = 128;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum TransportError {
    #[error("transport is closed")]
    Closed,
    #[error("transport failure: {0}")]
    Failed(String),
    #[error("transport queue exceeded its memory budget")]
    Overloaded,
}

struct Queued<T> {
    value: T,
    _budget: OwnedSemaphorePermit,
}

/// Owns reader/writer tasks; only the Core consumes incoming envelopes.
/// Byte permits bound queued memory even when individual frames are large.
pub struct PipeTransport {
    outgoing: Option<mpsc::Sender<Queued<String>>>,
    incoming: mpsc::Receiver<Result<Queued<Envelope>, TransportError>>,
    writer: JoinHandle<()>,
    reader: JoinHandle<()>,
    write_budget: Arc<Semaphore>,
}

impl PipeTransport {
    pub fn new<R, W>(reader: R, writer: W) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        Self::with_limit(reader, writer, MAX_LINE_BYTES)
    }

    pub(crate) fn with_limit<R, W>(reader: R, mut writer: W, limit: usize) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (incoming_tx, incoming) = mpsc::channel(QUEUE_FRAMES);
        let (outgoing, mut writes) = mpsc::channel::<Queued<String>>(32);
        let write_budget = Arc::new(Semaphore::new(QUEUE_BYTES));
        let read_budget = Arc::new(Semaphore::new(QUEUE_BYTES));
        let writer_error = incoming_tx.clone();
        let writer = tokio::spawn(async move {
            while let Some(frame) = writes.recv().await {
                if let Err(error) = writer.write_all(frame.value.as_bytes()).await {
                    let _ = writer_error
                        .send(Err(TransportError::Failed(error.to_string())))
                        .await;
                    return;
                }
                if let Err(error) = writer.flush().await {
                    let _ = writer_error
                        .send(Err(TransportError::Failed(error.to_string())))
                        .await;
                    return;
                }
            }
            let _ = writer.shutdown().await;
        });
        let reader = tokio::spawn(async move {
            let mut reader = BufReader::new(reader);
            loop {
                let result = read_frame(&mut reader, limit).await;
                match result {
                    Ok(line) => {
                        let permits = line.len().max(1) as u32;
                        let budget = match read_budget.clone().try_acquire_many_owned(permits) {
                            Ok(budget) => budget,
                            Err(_) => {
                                let _ = incoming_tx.send(Err(TransportError::Overloaded)).await;
                                return;
                            }
                        };
                        let envelope = match decode_line(&line) {
                            Ok(envelope) => envelope,
                            Err(error) => {
                                let _ = incoming_tx
                                    .send(Err(TransportError::Failed(error.to_string())))
                                    .await;
                                return;
                            }
                        };
                        // Never block stdout behind UI, approvals, or a Gate.
                        if incoming_tx
                            .try_send(Ok(Queued {
                                value: envelope,
                                _budget: budget,
                            }))
                            .is_err()
                        {
                            let _ = incoming_tx.send(Err(TransportError::Overloaded)).await;
                            return;
                        }
                    }
                    Err(error) => {
                        let _ = incoming_tx.send(Err(error)).await;
                        return;
                    }
                }
            }
        });
        Self {
            outgoing: Some(outgoing),
            incoming,
            writer,
            reader,
            write_budget,
        }
    }

    pub fn send(&self, envelope: Envelope) -> Result<(), TransportError> {
        let line =
            encode_line(&envelope).map_err(|error| TransportError::Failed(error.to_string()))?;
        let budget = self
            .write_budget
            .clone()
            .try_acquire_many_owned(line.len().max(1) as u32)
            .map_err(|_| TransportError::Overloaded)?;
        self.outgoing
            .as_ref()
            .ok_or(TransportError::Closed)?
            .try_send(Queued {
                value: line,
                _budget: budget,
            })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => TransportError::Overloaded,
                mpsc::error::TrySendError::Closed(_) => TransportError::Closed,
            })
    }

    /// recv is cancellation-safe, so command selection cannot discard a partial frame.
    pub async fn recv(&mut self) -> Result<Envelope, TransportError> {
        self.incoming
            .recv()
            .await
            .ok_or(TransportError::Closed)?
            .map(|frame| frame.value)
    }

    pub async fn close_writer(&mut self) {
        self.outgoing.take();
        if tokio::time::timeout(std::time::Duration::from_secs(1), &mut self.writer)
            .await
            .is_err()
        {
            self.writer.abort();
        }
    }
}

impl Drop for PipeTransport {
    fn drop(&mut self) {
        self.reader.abort();
        self.writer.abort();
    }
}

async fn read_frame<R: AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
    limit: usize,
) -> Result<String, TransportError> {
    let mut frame = Vec::new();
    loop {
        let buffer = reader
            .fill_buf()
            .await
            .map_err(|error| TransportError::Failed(error.to_string()))?;
        if buffer.is_empty() {
            return Err(if frame.is_empty() {
                TransportError::Closed
            } else {
                TransportError::Failed("EOF inside an unterminated JSONL frame".into())
            });
        }
        let newline = buffer.iter().position(|byte| *byte == b'\n');
        let len = newline.map_or(buffer.len(), |position| position + 1);
        if frame.len() + len > limit + 1 {
            return Err(TransportError::Failed(format!(
                "JSONL frame exceeds {limit} bytes"
            )));
        }
        frame.extend_from_slice(&buffer[..len]);
        reader.consume(len);
        if newline.is_some() {
            frame.pop();
            if frame.last() == Some(&b'\r') {
                frame.pop();
            }
            return String::from_utf8(frame)
                .map_err(|error| TransportError::Failed(error.to_string()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::RpcId;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn preserves_partial_frames_order_and_numeric_string_ids() {
        let (client, mut server) = tokio::io::duplex(4096);
        let (read, write) = tokio::io::split(client);
        let mut transport = PipeTransport::new(read, write);
        server.write_all(br#"{"id":"one","result":"#).await.unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), transport.recv())
                .await
                .is_err()
        );
        server
            .write_all(b"true}\n{\"id\":2,\"result\":{}}\n")
            .await
            .unwrap();
        assert_eq!(
            transport.recv().await.unwrap().id,
            Some(RpcId::String("one".into()))
        );
        assert_eq!(transport.recv().await.unwrap().id, Some(RpcId::Number(2)));
    }

    #[tokio::test]
    async fn rejects_oversized_frame_before_newline_and_reports_eof() {
        let (client, mut server) = tokio::io::duplex(128);
        let (read, write) = tokio::io::split(client);
        let mut transport = PipeTransport::with_limit(read, write, 8);
        server.write_all(b"0123456789").await.unwrap();
        assert!(matches!(
            transport.recv().await,
            Err(TransportError::Failed(_))
        ));
        let (client, server) = tokio::io::duplex(128);
        let (read, write) = tokio::io::split(client);
        let mut transport = PipeTransport::new(read, write);
        drop(server);
        assert_eq!(transport.recv().await, Err(TransportError::Closed));
    }

    #[tokio::test]
    async fn writes_one_jsonl_frame() {
        let (client, mut server) = tokio::io::duplex(1024);
        let (read, write) = tokio::io::split(client);
        let transport = PipeTransport::new(read, write);
        transport
            .send(Envelope::notification("initialized", None))
            .unwrap();
        let mut reader = BufReader::new(&mut server);
        let line = read_frame(&mut reader, MAX_LINE_BYTES).await.unwrap();
        assert_eq!(line, r#"{"method":"initialized"}"#);
    }

    #[tokio::test]
    async fn writer_failure_is_reported_even_while_reader_is_idle() {
        use std::pin::Pin;
        use std::task::{Context, Poll};
        struct BrokenWriter;
        impl AsyncWrite for BrokenWriter {
            fn poll_write(
                self: Pin<&mut Self>,
                _: &mut Context<'_>,
                _: &[u8],
            ) -> Poll<std::io::Result<usize>> {
                Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "writer closed",
                )))
            }
            fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
                Poll::Ready(Ok(()))
            }
            fn poll_shutdown(
                self: Pin<&mut Self>,
                _: &mut Context<'_>,
            ) -> Poll<std::io::Result<()>> {
                Poll::Ready(Ok(()))
            }
        }
        let (reader, _peer) = tokio::io::duplex(128);
        let mut transport = PipeTransport::new(reader, BrokenWriter);
        transport
            .send(Envelope::notification("initialized", None))
            .unwrap();
        let result = tokio::time::timeout(std::time::Duration::from_secs(1), transport.recv())
            .await
            .unwrap();
        assert!(
            matches!(result, Err(TransportError::Failed(message)) if message.contains("writer closed"))
        );
    }
}
