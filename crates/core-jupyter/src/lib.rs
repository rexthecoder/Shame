//! Jupyter notebooks: the `.ipynb` model, a Jupyter Server client (REST)
//! and kernel sessions over the server's websocket.
//!
//! Talking to `jupyter server` instead of a kernel's ZeroMQ sockets means
//! one code path for a local server Shame starts and a remote one the user
//! pastes a URL for (JupyterHub, Kaggle, Vertex Workbench, RunPod). No UI
//! here; the app renders notebooks and drives kernels through this crate.

pub mod kernel;
pub mod notebook;
pub mod server;

pub use kernel::{KernelEvent, KernelSession};
pub use notebook::{Cell, CellKind, Notebook, OutputPart};
pub use server::{KernelSpec, LocalServer, ServerInfo};
