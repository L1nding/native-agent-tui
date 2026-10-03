//! Public module boundaries for the native agent TUI.

pub mod agents;
pub mod app_server;
pub mod client;
pub mod compatibility;
pub mod config;
pub mod diagnostics;
pub mod gate;
pub mod headless;
pub mod history;
pub mod interactions;
pub mod journal;
pub mod json_events;
pub mod observation;
mod owned_process;
pub mod protocol;
pub mod scheduler;
mod shell_check;
pub mod skills;
pub mod state;
pub mod timeline;
pub mod transport;
pub mod ui;

pub use client::{ClientHandle, Command, ExitReport};
