//! Own JSON-RPC LSP client for `dart language-server` (M2).
//!
//! Has no UI dependency by design, so the GPUI-based UI layer (`app`,
//! `editor-view`) can be replaced later without touching this crate. Uses
//! `lsp_types` for the protocol's types (per the project plan: write the
//! client ourselves, but don't hand-roll LSP's own type definitions).

mod client;
mod transport;

pub use client::{LspClient, ServerMessage, wait_for_response};
pub use lsp_types;
pub use transport::{read_message, write_message};
