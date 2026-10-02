//! A single stdout owner tails committed state; slow output never holds Core.
use std::fs::File;
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use thiserror::Error;
use tokio::sync::watch;

use crate::journal::{write_record, CommittedReader, JournalError, JournalSettings, RecordKind};

const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const FINISH_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum OutputError {
    #[error("JSONL output failed; inspect the session journal for committed state")]
    Write,
    #[error("JSONL output remained blocked for five seconds; execution was stopped")]
    Stalled,
    #[error("JSONL writer closed without confirming final output")]
    Closed,
    #[error("JSONL journal ended without a durable session close; execution is uncertain")]
    Incomplete,
    #[error(transparent)]
    Journal(#[from] JournalError),
}

#[derive(Debug, Clone)]
pub struct OutputStatus {
    pub delivered_seq: Option<u64>,
    pub writing_since: Option<Instant>,
    pub error: Option<OutputError>,
    pub done: bool,
}

pub struct LiveOutput {
    pub status: watch::Receiver<OutputStatus>,
    producer_closed: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    write_timeout: Duration,
}

impl LiveOutput {
    pub fn stdout(
        settings: &JournalSettings,
        cwd: &Path,
        session: &str,
    ) -> Result<Self, OutputError> {
        Self::start(settings, cwd, session, stdout_file()?, WRITE_TIMEOUT)
    }

    /// Writer injection is for deterministic consumers and fault tests.
    pub fn start<W: Write + Send + 'static>(
        settings: &JournalSettings,
        cwd: &Path,
        session: &str,
        writer: W,
        write_timeout: Duration,
    ) -> Result<Self, OutputError> {
        let mut source = CommittedReader::open(settings, cwd, session)?;
        let (updates, status) = watch::channel(OutputStatus {
            delivered_seq: None,
            writing_since: None,
            error: None,
            done: false,
        });
        let producer_closed = Arc::new(AtomicBool::new(false));
        let cancel = Arc::new(AtomicBool::new(false));
        let closed = producer_closed.clone();
        let cancelled = cancel.clone();
        let worker = thread::Builder::new()
            .name("jsonl-output".into())
            .spawn(move || {
                let mut writer = TrackedWriter {
                    inner: writer,
                    updates: updates.clone(),
                    cancel: cancelled.clone(),
                };
                let result = (|| -> Result<(), OutputError> {
                    loop {
                        if cancelled.load(Ordering::Acquire) {
                            return Err(OutputError::Closed);
                        }
                        source
                            .drain(|record| {
                                write_record(&mut writer, record)?;
                                // Flush each line so observers receive evidence while a turn is active.
                                writer.flush().map_err(|_| JournalError::Output)?;
                                updates.send_modify(|status| {
                                    status.delivered_seq = Some(record.event_seq)
                                });
                                Ok(())
                            })
                            .map_err(|error| {
                                if error == JournalError::Output {
                                    OutputError::Write
                                } else {
                                    error.into()
                                }
                            })?;
                        let producer_done = closed.load(Ordering::Acquire);
                        if producer_done {
                            // A slow write may have held an older prefix while Core closed.
                            // Recapture after the producer's join before emitting the final snapshot.
                            source
                                .drain(|record| {
                                    write_record(&mut writer, record)?;
                                    writer.flush().map_err(|_| JournalError::Output)?;
                                    updates.send_modify(|status| {
                                        status.delivered_seq = Some(record.event_seq)
                                    });
                                    Ok(())
                                })
                                .map_err(|error| {
                                    if error == JournalError::Output {
                                        OutputError::Write
                                    } else {
                                        error.into()
                                    }
                                })?;
                        }
                        let latest = source.latest().expect("validated committed prefix");
                        if latest.state()?.session_closed || producer_done {
                            let mut final_snapshot = latest.clone();
                            final_snapshot.kind = RecordKind::Snapshot;
                            write_record(&mut writer, &final_snapshot)
                                .map_err(|_| OutputError::Write)?;
                            writer.flush().map_err(|_| OutputError::Write)?;
                            return if latest.state()?.session_closed {
                                Ok(())
                            } else {
                                Err(OutputError::Incomplete)
                            };
                        }
                        thread::sleep(Duration::from_millis(10));
                    }
                })();
                updates.send_modify(|status| {
                    status.writing_since = None;
                    status.error = result.err();
                    status.done = true;
                });
            })
            .map_err(|_| OutputError::Closed)?;
        Ok(Self {
            status,
            producer_closed,
            cancel,
            worker: Some(worker),
            write_timeout,
        })
    }

    pub fn failure(&self) -> Option<OutputError> {
        let status = self.status.borrow();
        status.error.clone().or_else(|| {
            status
                .writing_since
                .filter(|since| since.elapsed() >= self.write_timeout)
                .map(|_| OutputError::Stalled)
        })
    }

    pub async fn ready(&mut self) -> Result<(), OutputError> {
        tokio::time::timeout(self.write_timeout, async {
            loop {
                if let Some(error) = self.failure() {
                    return Err(error);
                }
                if self.status.borrow().delivered_seq.is_some() {
                    return Ok(());
                }
                self.status
                    .changed()
                    .await
                    .map_err(|_| OutputError::Closed)?;
            }
        })
        .await
        .map_err(|_| OutputError::Stalled)?
    }

    pub async fn finish(&mut self) -> Result<(), OutputError> {
        self.producer_closed.store(true, Ordering::Release);
        let result = tokio::time::timeout(FINISH_TIMEOUT, async {
            loop {
                if let Some(error) = self.failure() {
                    return Err(error);
                }
                if self.status.borrow().done {
                    return Ok(());
                }
                tokio::select! {
                    result = self.status.changed() => { result.map_err(|_| OutputError::Closed)?; }
                    _ = tokio::time::sleep(Duration::from_millis(20)) => {}
                }
            }
        })
        .await
        .unwrap_or(Err(OutputError::Stalled));
        if result.is_err() {
            self.stop();
        }
        // Cancellation may race a new native write. Repeat until the owned thread exits.
        let joined = tokio::time::timeout(Duration::from_secs(1), async {
            while self
                .worker
                .as_ref()
                .is_some_and(|worker| !worker.is_finished())
            {
                if result.is_err() {
                    self.stop();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            if let Some(worker) = self.worker.take() {
                worker.join().map_err(|_| OutputError::Closed)?;
            }
            Ok(())
        })
        .await
        .unwrap_or(Err(OutputError::Closed));
        result.and(joined)
    }

    pub fn stop(&self) {
        self.cancel.store(true, Ordering::Release);
        #[cfg(windows)]
        if let Some(worker) = &self.worker {
            use std::os::windows::io::AsRawHandle;
            // Only this thread issues writes through our duplicated stdout handle.
            unsafe { windows_sys::Win32::System::IO::CancelSynchronousIo(worker.as_raw_handle()) };
        }
    }
}

impl Drop for LiveOutput {
    fn drop(&mut self) {
        self.stop();
    }
}

struct TrackedWriter<W> {
    inner: W,
    updates: watch::Sender<OutputStatus>,
    cancel: Arc<AtomicBool>,
}
impl<W: Write> Write for TrackedWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.cancel.load(Ordering::Acquire) {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        self.updates
            .send_modify(|status| status.writing_since = Some(Instant::now()));
        let result = self.inner.write(&bytes[..bytes.len().min(8192)]);
        self.updates
            .send_modify(|status| status.writing_since = None);
        result
    }
    fn flush(&mut self) -> io::Result<()> {
        if self.cancel.load(Ordering::Acquire) {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        self.updates
            .send_modify(|status| status.writing_since = Some(Instant::now()));
        let result = self.inner.flush();
        self.updates
            .send_modify(|status| status.writing_since = None);
        result
    }
}

#[cfg(windows)]
fn stdout_file() -> Result<File, OutputError> {
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::Foundation::{
        DuplicateHandle, DUPLICATE_SAME_ACCESS, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::System::Console::{GetStdHandle, STD_OUTPUT_HANDLE};
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    // A private handle avoids holding Rust's global stdout mutex across a blocked pipe.
    unsafe {
        let stdout = GetStdHandle(STD_OUTPUT_HANDLE);
        if stdout.is_null() || stdout == INVALID_HANDLE_VALUE {
            return Err(OutputError::Write);
        }
        let process = GetCurrentProcess();
        let mut owned = std::ptr::null_mut();
        if DuplicateHandle(
            process,
            stdout,
            process,
            &mut owned,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        ) == 0
        {
            return Err(OutputError::Write);
        }
        Ok(File::from_raw_handle(owned))
    }
}

#[cfg(unix)]
fn stdout_file() -> Result<File, OutputError> {
    use std::os::fd::AsFd;
    std::io::stdout()
        .as_fd()
        .try_clone_to_owned()
        .map(File::from)
        .map_err(|_| OutputError::Write)
}

#[cfg(not(any(unix, windows)))]
fn stdout_file() -> Result<File, OutputError> {
    Err(OutputError::Write)
}

#[cfg(test)]
mod tests;
