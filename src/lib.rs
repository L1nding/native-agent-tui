//! Public module boundaries for the native agent TUI.

pub mod client;
pub mod config;
pub mod diagnostics;
pub mod gate;
pub mod interactions;
pub mod protocol;
pub mod rpc;
pub mod scheduler;
pub mod state;
pub mod transport;
pub mod ui;

pub use client::{ClientHandle, Command, ExitReport, StopMode};
