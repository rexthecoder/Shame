//! Own DAP client for `dart debug_adapter` / `flutter debug_adapter` (M4).
//!
//! Has no UI dependency by design, so the GPUI-based UI layer (`app`,
//! `editor-view`) can be replaced later without touching this crate.
//! Mirrors `core-lsp`'s shape (spawn a process, send JSON over
//! `Content-Length`-framed stdio, read messages off a channel) since
//! DAP's base framing is literally the same protocol LSP uses — but
//! duplicated rather than shared, and with its own `DapMessage` enum,
//! since DAP's message shapes (`seq`/`request_seq`/`success`, per-sender
//! seq numbers) are different enough from LSP's JSON-RPC envelope that
//! sharing the wrapper types would mean DAP-shaped fields leaking into
//! a crate meant to stay LSP-generic.
//!
//! Breakpoints, stepping, variable inspection, and a debug console are
//! all UI work and deliberately not built here yet.

mod client;
mod transport;

pub use client::{Breakpoint, DapClient, DapMessage, wait_for_event, wait_for_response};
