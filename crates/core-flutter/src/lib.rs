//! Process manager for `flutter run --machine` (devices, hot reload) (M3).
//!
//! Has no UI dependency by design, so the GPUI-based UI layer (`app`,
//! `editor-view`) can be replaced later without touching this crate.
//!
//! This is the backend only — a device picker, a Run/Stop button, and a
//! log panel are all UI work, deliberately not built here yet (see the
//! project's M3 notes in `NOTES.md` at the repo root for why that's
//! staying deferred for now). `pubspec::{add_package, pub_get}` and
//! `sdk::detect_sdk` round out the plan's M3 list ("pubspec.yaml
//! helpers: add package, pub get" and "SDK detection").
//!
//! Scope cuts:
//! - [`FlutterDaemon`] runs one project on one device at a time; nothing
//!   here manages multiple concurrent runs.
//! - The full spawn → hot reload → stop lifecycle was verified against
//!   the real `flutter` CLI by hand (piping commands through a FIFO),
//!   not as an automated test — a real `flutter run -d macos` takes
//!   10-15+ seconds and a full Xcode-buildable project, too slow and
//!   environment-fragile to run on every `cargo test`. What *is*
//!   automated is parsing real captured protocol lines
//!   (`parses_real_captured_daemon_lines`) and `flutter devices
//!   --machine` against the real CLI (`lists_real_devices_on_this_machine`).

mod daemon;
mod devices;
mod launch;
mod pubspec;
mod sdk;

pub use daemon::{DaemonOutput, FlutterDaemon};
pub use devices::{
    Device, IosSimulator, attach_simulator_window, boot_ios_simulator,
    capture_simulator_screenshot, list_devices, list_ios_simulators, simulator_press_home,
    tap_simulator_window,
};
pub use launch::{
    LaunchCodeLens, LaunchConfig, LaunchFile, expand_workspace_vars, load_launch_file,
    parse_launch_json,
};
pub use pubspec::{PubOutput, add_package, pub_get};
pub use sdk::{FlutterSdk, detect_sdk};
