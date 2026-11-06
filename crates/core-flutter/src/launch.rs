use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// A parsed `.vscode/launch.json` file.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchFile {
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub configurations: Vec<LaunchConfig>,
}

/// Optional `codeLens` block inside a VS Code Dart/Flutter launch configuration.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchCodeLens {
    #[serde(rename = "for", default)]
    pub targets: Vec<String>,
    #[serde(default)]
    pub title: Option<String>,
}

/// A single entry in `.vscode/launch.json` (`configurations[*]`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchConfig {
    pub name: String,
    #[serde(rename = "type", default)]
    pub config_type: String,
    #[serde(default)]
    pub request: String,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub program: Option<String>,
    #[serde(default)]
    pub flutter_mode: Option<String>,
    #[serde(default)]
    pub device_id: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub tool_args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub template_for: Option<String>,
    #[serde(default)]
    pub code_lens: Option<LaunchCodeLens>,
}

impl LaunchConfig {
    /// Returns `true` when this configuration is a test template / CodeLens
    /// (e.g. `"templateFor": "test"` or has a `"codeLens"` block) rather than
    /// a standalone app launch target.
    pub fn is_test_template(&self) -> bool {
        self.template_for
            .as_deref()
            .is_some_and(|t| t.eq_ignore_ascii_case("test"))
            || self.code_lens.is_some()
    }

    /// Title to display on inline test CodeLens buttons (`codeLens.title` or `name`).
    pub fn code_lens_title(&self) -> &str {
        self.code_lens
            .as_ref()
            .and_then(|cl| cl.title.as_deref())
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.name)
    }

    /// Whether this test template applies to whole-file test runs (`run-test-file`).
    pub fn applies_to_test_file(&self) -> bool {
        match &self.code_lens {
            Some(cl) if !cl.targets.is_empty() => {
                cl.targets.iter().any(|t| t == "run-test-file")
            }
            _ => self.is_test_template(),
        }
    }

    /// Whether this test template applies to individual test cases (`run-test`).
    pub fn applies_to_single_test(&self) -> bool {
        match &self.code_lens {
            Some(cl) if !cl.targets.is_empty() => cl.targets.iter().any(|t| t == "run-test"),
            _ => self.is_test_template(),
        }
    }

    /// Resolve `cwd` with `${workspaceFolder}` expanded, falling back to `workspace_root`.
    pub fn resolved_cwd(&self, workspace_root: &Path) -> PathBuf {
        match &self.cwd {
            Some(raw) if !raw.trim().is_empty() => {
                let expanded = expand_workspace_vars(raw, workspace_root);
                let path = PathBuf::from(&expanded);
                if path.is_absolute() {
                    path
                } else {
                    workspace_root.join(path)
                }
            }
            _ => workspace_root.to_path_buf(),
        }
    }

    /// Short human-readable relative directory label (e.g. `apps/alpha`) for UI display.
    pub fn relative_cwd_label(&self, workspace_root: &Path) -> Option<String> {
        let resolved = self.resolved_cwd(workspace_root);
        if resolved == workspace_root {
            return None;
        }
        resolved
            .strip_prefix(workspace_root)
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
            .filter(|s| !s.is_empty())
    }

    /// Extract the `--flavor <name>` value if present in `args` or `tool_args`.
    pub fn flavor(&self) -> Option<String> {
        let all: Vec<&str> = self
            .tool_args
            .iter()
            .chain(self.args.iter())
            .map(String::as_str)
            .collect();
        for (i, &arg) in all.iter().enumerate() {
            if arg == "--flavor" {
                if let Some(&val) = all.get(i + 1) {
                    return Some(val.to_string());
                }
            } else if let Some(val) = arg.strip_prefix("--flavor=") {
                return Some(val.to_string());
            }
        }
        None
    }

    /// Build the complete list of extra CLI arguments to pass to `flutter run --machine`
    /// or `flutter test`, expanding `${workspaceFolder}` in each argument.
    pub fn resolved_cli_args(&self, workspace_root: &Path) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(mode) = &self.flutter_mode {
            match mode.as_str() {
                "profile" => out.push("--profile".to_string()),
                "release" => out.push("--release".to_string()),
                _ => {}
            }
        }
        if let Some(prog) = &self.program {
            if !prog.trim().is_empty() {
                out.push("-t".to_string());
                out.push(expand_workspace_vars(prog, workspace_root));
            }
        }
        for arg in &self.tool_args {
            out.push(expand_workspace_vars(arg, workspace_root));
        }
        for arg in &self.args {
            out.push(expand_workspace_vars(arg, workspace_root));
        }
        out
    }

    /// Expand `${workspaceFolder}` in all environment variables.
    pub fn resolved_env(&self, workspace_root: &Path) -> HashMap<String, String> {
        self.env
            .iter()
            .map(|(k, v)| (k.clone(), expand_workspace_vars(v, workspace_root)))
            .collect()
    }
}

pub fn expand_workspace_vars(input: &str, workspace_root: &Path) -> String {
    let root_str = workspace_root.to_string_lossy();
    input
        .replace("${workspaceFolder}", &root_str)
        .replace("${workspaceRoot}", &root_str)
}

/// Strip `//` line comments, `/* ... */` block comments, and trailing commas
/// so standard VS Code JSONC (`launch.json`) parses cleanly with `serde_json`.
pub fn strip_jsonc(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    let mut in_string = false;

    while i < bytes.len() {
        let b = bytes[i];
        if in_string {
            out.push(b as char);
            if b == b'\\' && i + 1 < bytes.len() {
                i += 1;
                out.push(bytes[i] as char);
            } else if b == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }

        if b == b'"' {
            in_string = true;
            out.push('"');
            i += 1;
            continue;
        }

        // Line comment `// ...`
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }

        // Block comment `/* ... */`
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                if bytes[i] == b'\n' {
                    out.push('\n');
                }
                i += 1;
            }
            i = (i + 2).min(bytes.len());
            continue;
        }

        // Trailing comma before `]` or `}`
        if b == b',' {
            let mut look = i + 1;
            while look < bytes.len() && bytes[look].is_ascii_whitespace() {
                look += 1;
            }
            // Also skip comments between trailing comma and closing bracket
            while look + 1 < bytes.len() && bytes[look] == b'/' && bytes[look + 1] == b'/' {
                while look < bytes.len() && bytes[look] != b'\n' {
                    look += 1;
                }
                while look < bytes.len() && bytes[look].is_ascii_whitespace() {
                    look += 1;
                }
            }
            if look < bytes.len() && (bytes[look] == b']' || bytes[look] == b'}') {
                i += 1;
                continue;
            }
        }

        out.push(b as char);
        i += 1;
    }

    out
}

/// Load and parse `<workspace_root>/.vscode/launch.json` if it exists.
pub fn load_launch_file(workspace_root: &Path) -> Option<LaunchFile> {
    let path = workspace_root.join(".vscode/launch.json");
    let raw = std::fs::read_to_string(path).ok()?;
    parse_launch_json(&raw).ok()
}

pub fn parse_launch_json(raw: &str) -> Result<LaunchFile, serde_json::Error> {
    let cleaned = strip_jsonc(raw);
    serde_json::from_str(&cleaned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_monorepo_launch_json_with_test_templates_and_apps() {
        let sample = r#"{
            // VS Code launch file with comments
            "version": "0.2.0",
            "configurations": [
                {
                    "name": "Update goldens",
                    "type": "dart",
                    "request": "launch",
                    "args": [
                        "--update-goldens"
                    ],
                    "templateFor": "test",
                    "codeLens": {
                        "for": [
                            "run-test",
                            "run-test-file"
                        ],
                        "title": "Update goldens"
                    }
                },
                {
                    "name": "Coverage",
                    "type": "dart",
                    "request": "launch",
                    "args": [
                        "--coverage"
                    ],
                    "templateFor": "test",
                    "codeLens": {
                        "for": [
                            "run-test",
                            "run-test-file"
                        ],
                        "title": "Coverage"
                    }
                },
                {
                    "name": "[DEV] Alpha",
                    "cwd": "${workspaceFolder}/apps/alpha",
                    "type": "dart",
                    "request": "launch",
                    "args": [
                        "--flavor",
                        "dev"
                    ],
                    "toolArgs": [
                        "--dart-define-from-file=${workspaceFolder}/apps/alpha/.env.dev"
                    ]
                },
                {
                    "name": "[PROD] Alpha",
                    "cwd": "${workspaceFolder}/apps/alpha",
                    "type": "dart",
                    "request": "launch",
                    "args": [
                        "--flavor",
                        "prod"
                    ],
                    "toolArgs": [
                        "--dart-define-from-file=${workspaceFolder}/apps/alpha/.env.dev"
                    ]
                },
                {
                    "name": "[DEV] Beta",
                    "cwd": "${workspaceFolder}/apps/beta",
                    "type": "dart",
                    "request": "launch",
                    "args": [
                        "--flavor",
                        "dev"
                    ],
                    "toolArgs": [
                        "--dart-define-from-file=${workspaceFolder}/apps/beta/.env.dev"
                    ]
                },
                {
                    "name": "Design gallery",
                    "cwd": "${workspaceFolder}/packages/design_gallery",
                    "type": "dart",
                    "request": "launch",
                },
            ]
        }"#;

        let parsed = parse_launch_json(sample).expect("should parse launch.json");
        assert_eq!(parsed.version, "0.2.0");
        assert_eq!(parsed.configurations.len(), 6);

        let test_templates: Vec<_> = parsed
            .configurations
            .iter()
            .filter(|c| c.is_test_template())
            .collect();
        assert_eq!(test_templates.len(), 2);
        assert_eq!(test_templates[0].code_lens_title(), "Update goldens");
        assert!(test_templates[0].applies_to_single_test());
        assert!(test_templates[0].applies_to_test_file());

        let runnable: Vec<_> = parsed
            .configurations
            .iter()
            .filter(|c| !c.is_test_template())
            .collect();
        assert_eq!(runnable.len(), 4);
        assert_eq!(runnable[0].name, "[DEV] Alpha");
        assert_eq!(runnable[0].flavor().as_deref(), Some("dev"));

        let root = Path::new("/workspace/mono");
        assert_eq!(
            runnable[0].resolved_cwd(root),
            PathBuf::from("/workspace/mono/apps/alpha")
        );
        assert_eq!(
            runnable[0].relative_cwd_label(root).as_deref(),
            Some("apps/alpha")
        );
        assert_eq!(
            runnable[0].resolved_cli_args(root),
            vec![
                "--dart-define-from-file=/workspace/mono/apps/alpha/.env.dev".to_string(),
                "--flavor".to_string(),
                "dev".to_string(),
            ]
        );
    }
}

