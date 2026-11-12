//! Python support: the language adapter, project interpreter detection and
//! language-server discovery.
//!
//! The app is often launched from Finder with a bare `PATH`, so tools are
//! looked up in the project's environment and the usual install folders
//! (Homebrew, conda, `~/.local/bin`, npm globals) as well as `PATH`.

use std::path::{Path, PathBuf};

use crate::language::{LanguageAdapter, SnippetSpec};

/// How the project's interpreter was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PythonEnvKind {
    /// A virtual environment folder in the project (`.venv`, `venv`, `env`).
    Venv,
    /// A `.venv` managed by uv (the project has `uv.lock`).
    Uv,
    /// A conda environment, by name.
    Conda(String),
    /// `$VIRTUAL_ENV` from the environment the app was started in.
    Active,
    /// A Python on `PATH` or in a standard install folder.
    System,
}

/// The interpreter a Python project runs with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PythonEnv {
    /// The `python` executable.
    pub python: PathBuf,
    pub kind: PythonEnvKind,
}

impl PythonEnv {
    /// Short label for the UI: `.venv`, `uv · .venv`, `conda · base`, `python3`.
    pub fn label(&self) -> String {
        let venv_dir = || {
            self.python
                .parent()
                .and_then(Path::parent)
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "venv".into())
        };
        match &self.kind {
            PythonEnvKind::Venv | PythonEnvKind::Active => venv_dir(),
            PythonEnvKind::Uv => format!("uv · {}", venv_dir()),
            PythonEnvKind::Conda(name) => format!("conda · {name}"),
            PythonEnvKind::System => self
                .python
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "python".into()),
        }
    }

    /// The environment's `bin` folder, where its tools (`pytest`, `pip`,
    /// `jupyter`) live.
    pub fn bin_dir(&self) -> Option<&Path> {
        self.python.parent()
    }
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Conda installs in their usual places.
fn conda_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(exe) = std::env::var_os("CONDA_EXE").map(PathBuf::from) {
        if let Some(root) = exe.parent().and_then(Path::parent) {
            roots.push(root.to_path_buf());
        }
    }
    if let Some(h) = home() {
        for name in ["anaconda3", "miniconda3", "miniforge3", "mambaforge"] {
            roots.push(h.join(name));
        }
    }
    for root in ["/opt/anaconda3", "/opt/miniconda3", "/opt/homebrew/Caskroom/miniconda/base"] {
        roots.push(PathBuf::from(root));
    }
    let mut seen = std::collections::HashSet::new();
    roots.retain(|r| seen.insert(r.clone()));
    roots
}

/// Folders searched for executables, after the project's own environment.
fn tool_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    if let Some(h) = home() {
        dirs.push(h.join(".local/bin"));
        dirs.push(h.join(".cargo/bin"));
        dirs.push(h.join(".npm-global/bin"));
        dirs.push(h.join(".bun/bin"));
    }
    dirs.push(PathBuf::from("/opt/homebrew/bin"));
    dirs.push(PathBuf::from("/usr/local/bin"));
    for root in conda_roots() {
        dirs.push(root.join("bin"));
    }
    dirs.push(PathBuf::from("/usr/bin"));
    let mut seen = std::collections::HashSet::new();
    dirs.retain(|d| seen.insert(d.clone()));
    dirs
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Find `name` in `env`'s `bin` folder first, then the usual tool folders.
pub fn find_tool(name: &str, env: Option<&PythonEnv>) -> Option<PathBuf> {
    if let Some(bin) = env.and_then(PythonEnv::bin_dir) {
        let candidate = bin.join(name);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    tool_dirs()
        .into_iter()
        .map(|d| d.join(name))
        .find(|p| is_executable(p))
}

/// The conda environment named in `environment.yml`, if the file exists.
fn conda_env_name(root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(root.join("environment.yml"))
        .or_else(|_| std::fs::read_to_string(root.join("environment.yaml")))
        .ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix("name:"))
        .map(|n| n.trim().trim_matches(['"', '\'']).to_string())
        .filter(|n| !n.is_empty())
}

/// The interpreter for the project at `root`: a venv in the project, the
/// conda env its `environment.yml` names, an activated `$VIRTUAL_ENV` or
/// `$CONDA_PREFIX`, else the first `python3` found.
pub fn detect_env(root: Option<&Path>) -> Option<PythonEnv> {
    if let Some(root) = root {
        for dir in [".venv", "venv", "env", ".env"] {
            let python = root.join(dir).join("bin/python");
            if is_executable(&python) && root.join(dir).join("pyvenv.cfg").exists() {
                let kind = if root.join("uv.lock").exists() {
                    PythonEnvKind::Uv
                } else {
                    PythonEnvKind::Venv
                };
                return Some(PythonEnv { python, kind });
            }
        }
        if let Some(name) = conda_env_name(root) {
            for conda in conda_roots() {
                let python = if name == "base" {
                    conda.join("bin/python")
                } else {
                    conda.join("envs").join(&name).join("bin/python")
                };
                if is_executable(&python) {
                    return Some(PythonEnv { python, kind: PythonEnvKind::Conda(name) });
                }
            }
        }
    }
    if let Some(venv) = std::env::var_os("VIRTUAL_ENV").map(PathBuf::from) {
        let python = venv.join("bin/python");
        if is_executable(&python) {
            return Some(PythonEnv { python, kind: PythonEnvKind::Active });
        }
    }
    if let Some(prefix) = std::env::var_os("CONDA_PREFIX").map(PathBuf::from) {
        let python = prefix.join("bin/python");
        if is_executable(&python) {
            let name = if conda_roots().contains(&prefix) {
                "base".to_string()
            } else {
                prefix.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
            };
            return Some(PythonEnv { python, kind: PythonEnvKind::Conda(name) });
        }
    }
    let python = find_tool("python3", None)?;
    // A conda base install's python is a conda env, worth naming as such.
    if conda_roots().iter().any(|r| python.starts_with(r)) {
        return Some(PythonEnv { python, kind: PythonEnvKind::Conda("base".into()) });
    }
    Some(PythonEnv { python, kind: PythonEnvKind::System })
}

/// The Python project root for `file`: the nearest folder with a Python
/// project marker, else the nearest `.git`, else the file's folder.
pub fn project_root_for(file: &Path) -> Option<PathBuf> {
    PythonLanguageAdapter.find_project_root(file)
}

/// Language servers, best first. The first one installed wins.
const SERVERS: &[(&str, &[&str])] = &[
    ("basedpyright-langserver", &["--stdio"]),
    ("pyright-langserver", &["--stdio"]),
    ("pylsp", &[]),
    ("ruff", &["server"]),
];

/// The language server to start for a project at `root`.
pub fn language_server(root: Option<&Path>) -> Option<(PathBuf, Vec<String>)> {
    let env = detect_env(root);
    SERVERS.iter().find_map(|(bin, args)| {
        find_tool(bin, env.as_ref()).map(|p| (p, args.iter().map(|a| (*a).to_string()).collect()))
    })
}

/// Settings for a `workspace/configuration` item in a Python project,
/// pointing the server at the project's interpreter. `None` means "your
/// defaults".
pub fn configuration_for(section: &str, scope: Option<&Path>) -> Option<serde_json::Value> {
    if !matches!(section, "python" | "pylsp") {
        return None;
    }
    let root = scope.and_then(|p| if p.is_dir() { Some(p.to_path_buf()) } else { project_root_for(p) });
    let env = detect_env(root.as_deref())?;
    let python = env.python.to_string_lossy().into_owned();
    Some(match section {
        // pyright and basedpyright.
        "python" => serde_json::json!({ "pythonPath": python }),
        _ => serde_json::json!({ "plugins": { "jedi": { "environment": python } } }),
    })
}

pub struct PythonLanguageAdapter;

const KEYWORDS: &[&str] = &[
    "and", "as", "assert", "async", "await", "break", "case", "class", "continue", "def", "del",
    "elif", "else", "except", "False", "finally", "for", "from", "global", "if", "import", "in",
    "is", "lambda", "match", "None", "nonlocal", "not", "or", "pass", "raise", "return", "self",
    "True", "try", "while", "with", "yield",
];

const SNIPPETS: &[SnippetSpec] = &[
    SnippetSpec {
        trigger: "main",
        detail: "if __name__ == \"__main__\"",
        body: "if __name__ == \"__main__\":\n    ${1:main()}",
    },
    SnippetSpec {
        trigger: "def",
        detail: "function",
        body: "def ${1:name}(${2}):\n    ${3:pass}",
    },
    SnippetSpec {
        trigger: "class",
        detail: "class",
        body: "class ${1:Name}:\n    def __init__(self${2}):\n        ${3:pass}",
    },
    SnippetSpec {
        trigger: "dataclass",
        detail: "@dataclass class",
        body: "@dataclass\nclass ${1:Name}:\n    ${2:field}: ${3:int}",
    },
    SnippetSpec {
        trigger: "test",
        detail: "pytest test function",
        body: "def test_${1:name}():\n    ${2:assert True}",
    },
    SnippetSpec {
        trigger: "module",
        detail: "torch nn.Module",
        body: "class ${1:Model}(nn.Module):\n    def __init__(self):\n        super().__init__()\n        ${2:self.layer = nn.Linear(1, 1)}\n\n    def forward(self, x):\n        ${3:return self.layer(x)}",
    },
];

impl LanguageAdapter for PythonLanguageAdapter {
    fn language_id(&self) -> &'static str {
        "python"
    }

    fn matches_path(&self, path: &Path) -> bool {
        path.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "py" | "pyi" | "pyw"))
    }

    fn root_markers(&self) -> &'static [&'static str] {
        &[
            "pyproject.toml",
            "uv.lock",
            "setup.py",
            "setup.cfg",
            "requirements.txt",
            "environment.yml",
            "Pipfile",
            ".venv",
        ]
    }

    fn lsp_command(&self, project_root: Option<&Path>) -> Option<(String, Vec<String>)> {
        language_server(project_root).map(|(p, args)| (p.to_string_lossy().into_owned(), args))
    }

    fn keywords(&self) -> &'static [&'static str] {
        KEYWORDS
    }

    fn snippets(&self) -> &'static [SnippetSpec] {
        SNIPPETS
    }

    fn detect_test_target(
        &self,
        file_path: Option<&Path>,
        line_text: &str,
    ) -> Option<(bool, Option<String>)> {
        let name = file_path?.file_name()?.to_str()?;
        if !(name.starts_with("test_") || name.ends_with("_test.py")) {
            return None;
        }
        let trimmed = line_text.trim_start();
        let rest = trimmed.strip_prefix("async def ").or_else(|| trimmed.strip_prefix("def "))?;
        let fn_name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
        fn_name.starts_with("test").then_some((false, Some(fn_name)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("shame-py-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn fake_venv(root: &Path, dir: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let bin = root.join(dir).join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(root.join(dir).join("pyvenv.cfg"), "home = /usr/bin\n").unwrap();
        let python = bin.join("python");
        std::fs::write(&python, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&python, std::fs::Permissions::from_mode(0o755)).unwrap();
        python
    }

    #[test]
    fn a_project_venv_wins_and_uv_is_named() {
        let root = tempdir("venv");
        let python = fake_venv(&root, ".venv");
        let env = detect_env(Some(&root)).unwrap();
        assert_eq!(env.python, python);
        assert_eq!(env.kind, PythonEnvKind::Venv);
        assert_eq!(env.label(), ".venv");

        std::fs::write(root.join("uv.lock"), "").unwrap();
        let env = detect_env(Some(&root)).unwrap();
        assert_eq!(env.kind, PythonEnvKind::Uv);
        assert_eq!(env.label(), "uv · .venv");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn configuration_points_pyright_at_the_venv() {
        let root = tempdir("cfg");
        let python = fake_venv(&root, ".venv");
        std::fs::write(root.join("pyproject.toml"), "").unwrap();
        let file = root.join("main.py");
        let cfg = configuration_for("python", Some(&file)).unwrap();
        assert_eq!(cfg["pythonPath"], python.to_string_lossy().as_ref());
        assert!(configuration_for("python.analysis", Some(&file)).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn conda_env_name_reads_environment_yml() {
        let root = tempdir("conda");
        std::fs::write(root.join("environment.yml"), "name: ml-env\ndependencies:\n  - python\n").unwrap();
        assert_eq!(conda_env_name(&root).as_deref(), Some("ml-env"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn python_buffers_get_the_python_grammar() {
        use core_syntax::SyntaxLanguage;
        let lang = crate::language::language_for_path(Some(Path::new("/p/model.py")));
        assert_eq!(lang.language_id(), "python");
        assert_eq!(
            crate::syntax_language(lang.language_id(), Some(Path::new("/p/model.py"))),
            Some(SyntaxLanguage::Python)
        );
        assert_eq!(crate::syntax_language("rust", Some(Path::new("/p/a.rs"))), None);
    }

    #[test]
    fn pytest_functions_are_test_targets() {
        let a = PythonLanguageAdapter;
        let file = Path::new("/p/tests/test_model.py");
        assert_eq!(
            a.detect_test_target(Some(file), "def test_forward():"),
            Some((false, Some("test_forward".into())))
        );
        assert_eq!(a.detect_test_target(Some(file), "def helper():"), None);
        assert_eq!(a.detect_test_target(Some(Path::new("/p/model.py")), "def test_x():"), None);
    }
}
