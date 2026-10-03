//! One cancellable startup check, with a connection and process lifetime of its own.
use std::time::Duration;

use serde_json::Value;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::app_server::{self, AppServer, AppServerError, ShellPeer};
use crate::config::Config;
use crate::protocol::{Envelope, RpcId};
use crate::transport::PipeTransport;

pub(crate) const DEADLINE: Duration = Duration::from_secs(30);

pub(crate) enum Source {
    Peer(ShellPeer),
    #[cfg(test)]
    Pipe(PipeTransport),
    #[cfg(test)]
    ScriptedCleanup(PipeTransport, tokio::sync::oneshot::Receiver<bool>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    Passed,
    BackendRejected,
    InitializeRejected,
    ShellRejected,
    TransportUnavailable,
    ProtocolRejected,
    TimedOut,
    Cancelled,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Report {
    pub outcome: Outcome,
    pub cleanup_confirmed: bool,
}

pub(crate) struct ShellCheck {
    pub deadline: Instant,
    cancel: watch::Sender<bool>,
    task: JoinHandle<Report>,
}

impl ShellCheck {
    pub(crate) fn start(source: Source, config: Config) -> Self {
        let deadline = Instant::now() + DEADLINE;
        let (cancel, cancelled) = watch::channel(false);
        let task = tokio::spawn(run(source, config, cancelled, deadline));
        Self {
            deadline,
            cancel,
            task,
        }
    }

    pub(crate) fn cancel(&self) {
        self.cancel.send_replace(true);
    }

    // Awaiting the retained join handle is cancellation-safe in the Core select.
    pub(crate) async fn wait(&mut self) -> Report {
        (&mut self.task).await.unwrap_or(Report {
            outcome: Outcome::TransportUnavailable,
            cleanup_confirmed: false,
        })
    }
}

impl Drop for ShellCheck {
    fn drop(&mut self) {
        self.cancel();
    }
}

async fn run(
    source: Source,
    config: Config,
    mut cancelled: watch::Receiver<bool>,
    deadline: Instant,
) -> Report {
    #[cfg(test)]
    let mut cleanup_confirmation = None;
    let (mut pipe, mut server): (PipeTransport, Option<AppServer>) = match source {
        Source::Peer(peer) => match peer.launch(&config, &cancelled).await {
            Ok(Some(mut server)) => (
                server.pipe.take().expect("new peer owns its transport"),
                Some(server),
            ),
            Ok(None) => {
                return Report {
                    outcome: Outcome::Cancelled,
                    cleanup_confirmed: true,
                }
            }
            Err(error) => {
                return Report {
                    outcome: Outcome::BackendRejected,
                    cleanup_confirmed: !matches!(error, AppServerError::StartupCleanup),
                }
            }
        },
        #[cfg(test)]
        Source::Pipe(pipe) => (pipe, None),
        #[cfg(test)]
        Source::ScriptedCleanup(pipe, confirmation) => {
            cleanup_confirmation = Some(confirmation);
            (pipe, None)
        }
    };
    let outcome = if *cancelled.borrow() {
        Outcome::Cancelled
    } else {
        tokio::select! {
            biased;
            _ = cancelled.changed() => Outcome::Cancelled,
            result = tokio::time::timeout_at(deadline, exchange(&mut pipe, &config)) => {
                match result {
                    Ok(Ok(())) => Outcome::Passed,
                    Ok(Err(outcome)) => outcome,
                    Err(_) => Outcome::TimedOut,
                }
            }
        }
    };
    let cleanup_confirmed = if let Some(server) = &mut server {
        server.pipe = Some(pipe);
        matches!(
            tokio::time::timeout(Duration::from_secs(6), server.shutdown()).await,
            Ok(Ok(()))
        )
    } else {
        pipe.close_writer().await;
        #[cfg(test)]
        {
            match cleanup_confirmation {
                Some(confirmation) => matches!(
                    tokio::time::timeout(Duration::from_secs(6), confirmation).await,
                    Ok(Ok(true))
                ),
                None => true,
            }
        }
        #[cfg(not(test))]
        {
            true
        }
    };
    Report {
        outcome,
        cleanup_confirmed,
    }
}

async fn exchange(pipe: &mut PipeTransport, config: &Config) -> Result<(), Outcome> {
    let initialize_id = RpcId::Number(1);
    pipe.send(app_server::initialize(initialize_id.clone()))
        .map_err(|_| Outcome::TransportUnavailable)?;
    let result = response(pipe, &initialize_id).await?;
    crate::compatibility::verify_initialize(&result).map_err(|_| Outcome::InitializeRejected)?;
    pipe.send(Envelope::notification("initialized", None))
        .map_err(|_| Outcome::TransportUnavailable)?;
    let shell_id = RpcId::Number(2);
    pipe.send(app_server::preflight(shell_id.clone(), config))
        .map_err(|_| Outcome::TransportUnavailable)?;
    let result = response(pipe, &shell_id).await?;
    let exit_code = result["exitCode"]
        .as_i64()
        .ok_or(Outcome::ProtocolRejected)?;
    let stdout = result["stdout"].as_str().ok_or(Outcome::ProtocolRejected)?;
    if exit_code != 0 || !stdout.contains("native-agent-tui-shell-ok") {
        return Err(Outcome::ShellRejected);
    }
    Ok(())
}

async fn response(pipe: &mut PipeTransport, id: &RpcId) -> Result<Value, Outcome> {
    for _ in 0..128 {
        let envelope = pipe
            .recv()
            .await
            .map_err(|_| Outcome::TransportUnavailable)?;
        match (envelope.method, envelope.id) {
            (None, Some(response_id)) if &response_id == id && envelope.error.is_none() => {
                return envelope.result.ok_or(Outcome::ProtocolRejected);
            }
            (Some(_), None) => continue,
            (Some(_), Some(request_id)) => {
                let _ = pipe.send(Envelope::error_response(
                    request_id,
                    -32601,
                    "Shell preflight cannot answer interactive requests",
                ));
                return Err(Outcome::ProtocolRejected);
            }
            _ => return Err(Outcome::ProtocolRejected),
        }
    }
    Err(Outcome::ProtocolRejected)
}
