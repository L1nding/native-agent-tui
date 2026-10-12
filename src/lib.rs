//! Public module boundaries for the native agent TUI.

pub mod agents;
pub mod app_server;
pub mod backend;
pub mod client;
pub mod config;
pub mod conversations;
pub mod diagnostics;
pub mod gate;
pub mod headless;
pub mod history;
pub mod interactions;
pub mod journal;
pub mod json_events;
pub mod models;
pub mod observation;
pub mod outbox;
mod owned_process;
pub mod permission;
pub mod preferences;
pub mod protocol;
pub mod scheduler;
pub mod skills;
pub mod state;
pub mod timeline;
pub mod tool_details;
pub mod transport;
pub mod ui;

pub use client::{ClientHandle, Command, ExitReport};
