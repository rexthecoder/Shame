//! Flutter's widget inspector, reached through the daemon (`app.callServiceExtension`): switch the
//! running app into "select widget" mode, then ask which widget was selected and where in the
//! source it was created. Selecting a widget by tapping it on the (mirrored) simulator screen and
//! jumping to its code is built from these pieces.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// Where a widget was created in source code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WidgetLocation {
    /// The widget class, e.g. `Text`.
    pub name: String,
    pub path: PathBuf,
    /// 1-based, as the inspector reports them.
    pub line: u32,
    pub column: u32,
    /// Created by the open project (as opposed to Flutter itself or a package).
    pub local: bool,
}

/// `file:///Users/me/My%20App/lib/main.dart` → `/Users/me/My App/lib/main.dart`.
pub fn file_uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() + 1 && i + 3 <= bytes.len() {
            if let Ok(byte) =
                u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?, 16)
            {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).ok().map(PathBuf::from)
}

/// `/Users/me/My App` → `file:///Users/me/My%20App`.
pub fn path_to_file_uri(path: &Path) -> String {
    let mut out = String::from("file://");
    for b in path.to_string_lossy().bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The widget selected in the app, from the daemon's reply to `getSelectedSummaryWidget`.
/// `None` when nothing is selected or the widget carries no creation location (for example
/// when the app was not built in debug mode).
pub fn parse_selected_widget(response: &Value) -> Option<WidgetLocation> {
    // The daemon wraps the extension's reply: `{"result": <node>, "type": ..., "method": ...}`.
    let node = response.get("result")?;
    let location = node.get("creationLocation")?;
    Some(WidgetLocation {
        name: location
            .get("name")
            .or_else(|| node.get("description"))
            .and_then(Value::as_str)?
            .to_string(),
        path: file_uri_to_path(location.get("file")?.as_str()?)?,
        line: location.get("line")?.as_u64()? as u32,
        column: location.get("column").and_then(Value::as_u64).unwrap_or(1) as u32,
        local: node
            .get("createdByLocalProject")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The daemon's real reply (trimmed) after tapping `const Text('Hello from the demo')` in a
    /// freshly created app: a `Text` at lib/main.dart:28:19, created by the local project.
    const REAL_REPLY: &str = r#"{"result": {"description": "Text", "type": "_ElementDiagnosticableTreeNode", "style": "dense", "hasChildren": true, "allowWrap": false, "valueId": "inspector-0", "locationId": 0, "creationLocation": {"file": "file:///private/tmp/inspect_demo/lib/main.dart", "line": 28, "column": 19, "name": "Text"}, "createdByLocalProject": true, "children": [{"description": "RichText", "creationLocation": {"file": "file:///Users/me/fvm/versions/3.47.2/packages/flutter/lib/src/widgets/text.dart", "line": 780, "column": 16, "name": "RichText"}}], "widgetRuntimeType": "Text", "stateful": false}, "type": "_extensionType", "method": "ext.flutter.inspector.getSelectedSummaryWidget"}"#;

    #[test]
    fn reads_the_location_of_the_selected_widget() {
        let reply: Value = serde_json::from_str(REAL_REPLY).unwrap();
        assert_eq!(
            parse_selected_widget(&reply),
            Some(WidgetLocation {
                name: "Text".into(),
                path: PathBuf::from("/private/tmp/inspect_demo/lib/main.dart"),
                line: 28,
                column: 19,
                local: true,
            })
        );
    }

    #[test]
    fn nothing_selected_or_no_location_is_none() {
        for reply in [
            r#"{"result": null, "type": "_extensionType"}"#,
            r#"{"result": {"description": "Text"}, "type": "_extensionType"}"#,
            r#"{"type": "_extensionType"}"#,
        ] {
            assert_eq!(
                parse_selected_widget(&serde_json::from_str(reply).unwrap()),
                None,
                "{reply}"
            );
        }
    }

    #[test]
    fn a_framework_widget_is_not_local() {
        let reply = serde_json::json!({"result": {"description": "Scaffold", "creationLocation":
            {"file": "file:///sdk/packages/flutter/lib/src/material/scaffold.dart", "line": 1, "column": 2, "name": "Scaffold"}}});
        assert!(!parse_selected_widget(&reply).unwrap().local);
    }

    #[test]
    fn file_uris_round_trip_with_spaces_and_unicode() {
        let path = PathBuf::from("/Users/me/My App/lib/é.dart");
        let uri = path_to_file_uri(&path);
        assert_eq!(uri, "file:///Users/me/My%20App/lib/%C3%A9.dart");
        assert_eq!(file_uri_to_path(&uri), Some(path));
        assert_eq!(file_uri_to_path("package:foo/foo.dart"), None);
        assert_eq!(file_uri_to_path("file:///a%2"), Some(PathBuf::from("/a%2")));
    }
}
