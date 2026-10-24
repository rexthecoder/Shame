use std::io;
use std::path::PathBuf;
use std::process::Command;

use serde::Deserialize;

/// What `flutter --version --machine` reports about the SDK on `PATH`
/// (or under `fvm` etc. — whatever `flutter` resolves to). Needed so
/// the rest of the app (the Dart LSP launch, the Flutter daemon) knows
/// which SDK it's actually talking to, rather than assuming a bare
/// `dart`/`flutter` on `PATH` is the right one.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct FlutterSdk {
    #[serde(rename = "flutterVersion")]
    pub flutter_version: String,
    #[serde(rename = "dartSdkVersion")]
    pub dart_sdk_version: String,
    pub channel: String,
    #[serde(rename = "flutterRoot")]
    pub flutter_root: String,
}

impl FlutterSdk {
    /// Where the Dart SDK bundled inside this Flutter SDK lives — in
    /// particular `<dart_sdk_dir>/bin/snapshots/analysis_server.dart.snapshot`,
    /// which is what `core-lsp` should launch instead of hoping a
    /// bare `dart` resolves to a compatible version.
    pub fn dart_sdk_dir(&self) -> PathBuf {
        PathBuf::from(&self.flutter_root).join("bin").join("cache").join("dart-sdk")
    }
}

/// Run `flutter --version --machine` and parse it. Returns `Err` if
/// `flutter` isn't on `PATH` at all (the caller can treat that as "no
/// SDK found" and prompt the user to set one, once there's a UI for
/// that — not built here, per the plan's UI-work deferral).
pub fn detect_sdk() -> io::Result<FlutterSdk> {
    let output = Command::new("flutter").args(["--version", "--machine"]).output()?;
    if !output.status.success() {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!("`flutter --version --machine` exited with {}", output.status),
        ));
    }
    serde_json::from_slice(&output.stdout).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The actual (trimmed) output of `flutter --version --machine` on
    /// this machine.
    #[test]
    fn deserializes_real_flutter_version_machine_output() {
        let json = r#"{
          "frameworkVersion": "3.47.2",
          "channel": "stable",
          "repositoryUrl": "https://github.com/flutter/flutter.git",
          "frameworkRevision": "d3b14c876900e553bc736ca19295fc09e3853e8e",
          "frameworkCommitDate": "2026-08-26 16:07:51 -0700",
          "engineRevision": "a804b261645ef8c13eb3d5c44a5c2fb0340c5539",
          "engineCommitDate": "2026-08-26 18:46:13.000Z",
          "engineContentHash": "1cf1c4773fb941c4c74a7f8bb144a8837596c0f4",
          "engineBuildDate": "2026-08-26 19:23:07.677",
          "dartSdkVersion": "3.13.2",
          "devToolsVersion": "2.60.0",
          "flutterVersion": "3.47.2",
          "flutterRoot": "/Users/bushadigitalltd/fvm/versions/3.47.2"
        }"#;
        let sdk: FlutterSdk = serde_json::from_str(json).expect("should deserialize");
        assert_eq!(sdk.flutter_version, "3.47.2");
        assert_eq!(sdk.dart_sdk_version, "3.13.2");
        assert_eq!(sdk.channel, "stable");
        assert_eq!(
            sdk.dart_sdk_dir(),
            PathBuf::from("/Users/bushadigitalltd/fvm/versions/3.47.2/bin/cache/dart-sdk")
        );
    }

    /// Spawns the real CLI. Skips (not fails) if `flutter` isn't on
    /// `PATH`.
    #[test]
    fn detects_the_real_sdk_on_this_machine() {
        if Command::new("flutter").arg("--version").output().is_err() {
            eprintln!("skipping: `flutter` not found on PATH");
            return;
        }
        let sdk = detect_sdk().expect("flutter --version --machine should succeed");
        assert!(!sdk.flutter_version.is_empty());
        assert!(!sdk.dart_sdk_version.is_empty());
        assert!(PathBuf::from(&sdk.flutter_root).is_dir(), "flutter_root should be a real directory");
    }
}
