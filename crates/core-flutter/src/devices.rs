use std::io;
use std::process::Command;

use serde::Deserialize;

/// A device or emulator `flutter devices` can see, enough to populate a
/// device picker.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    #[serde(rename = "targetPlatform")]
    pub target_platform: String,
    pub emulator: bool,
    #[serde(rename = "isSupported")]
    pub is_supported: bool,
}

/// Run `flutter devices --machine` and parse its output. Unlike the
/// streaming daemon protocol (`FlutterDaemon`), this is a one-shot
/// command: it prints a single (pretty-printed, multi-line) JSON array
/// and exits, so we just wait for it and parse the whole thing.
pub fn list_devices() -> io::Result<Vec<Device>> {
    let output = Command::new("flutter").args(["devices", "--machine"]).output()?;
    if !output.status.success() {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!("`flutter devices --machine` exited with {}", output.status),
        ));
    }
    serde_json::from_slice(&output.stdout).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Not synthetic JSON — the actual (trimmed) output of `flutter
    /// devices --machine` on this machine: a simulator, macOS itself,
    /// and Chrome.
    #[test]
    fn deserializes_real_flutter_devices_output() {
        let json = r#"[
          {
            "name": "iPhone 16",
            "id": "4B6D332E-101F-4CBB-B288-4FEACF0A94BE",
            "isSupported": true,
            "targetPlatform": "ios",
            "emulator": true,
            "sdk": "com.apple.CoreSimulator.SimRuntime.iOS-18-5",
            "capabilities": {"hotReload": true, "hotRestart": true, "screenshot": true, "flutterExit": true, "hardwareRendering": false, "startPaused": true}
          },
          {
            "name": "macOS",
            "id": "macos",
            "isSupported": true,
            "targetPlatform": "darwin",
            "emulator": false,
            "sdk": "macOS 27.0.1 26A434 darwin-arm64",
            "capabilities": {"hotReload": true, "hotRestart": true, "screenshot": false, "flutterExit": true, "hardwareRendering": false, "startPaused": true}
          }
        ]"#;
        let devices: Vec<Device> = serde_json::from_str(json).expect("should deserialize");
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].name, "iPhone 16");
        assert!(devices[0].emulator);
        assert_eq!(devices[1].id, "macos");
        assert!(!devices[1].emulator);
    }

    /// Spawns the real `flutter devices --machine` and checks it finds
    /// at least one device — on this machine that's always `macos`
    /// itself (desktop target), so this is a genuine check against the
    /// real CLI, not just the parser test above. Skips (not fails) if
    /// `flutter` isn't on `PATH`.
    #[test]
    fn lists_real_devices_on_this_machine() {
        if Command::new("flutter").arg("--version").output().is_err() {
            eprintln!("skipping: `flutter` not found on PATH");
            return;
        }
        let devices = list_devices().expect("flutter devices --machine should succeed");
        assert!(!devices.is_empty(), "expected at least one device (e.g. macos itself)");
    }
}
