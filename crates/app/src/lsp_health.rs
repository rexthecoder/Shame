//! Which language servers this build would start, and whether they are
//! actually installed on this Mac. The editor falls back to lexical
//! highlighting when a server binary is missing, which looks like "LSP is
//! broken"; the Settings list turns that into a concrete install step.

use std::path::{Path, PathBuf};

/// Status of one configured language server.
#[derive(Clone, Debug)]
pub(crate) struct LspServerStatus {
    pub name: &'static str,
    pub language_id: &'static str,
    /// Executable name as spawned (may already be an absolute path).
    pub program: String,
    pub args: String,
    pub install: &'static str,
    pub path: Option<PathBuf>,
}

impl LspServerStatus {
    pub(crate) fn is_ready(&self) -> bool {
        self.path.is_some()
    }
}

/// Every configured server with its resolution result, sorted ready-first then
/// alphabetically so the useful rows sit at the top.
pub(crate) fn scan(project_root: Option<&Path>) -> Vec<LspServerStatus> {
    let mut rows: Vec<LspServerStatus> = editor_view::language::language_server_specs(project_root)
        .into_iter()
        .map(|spec| LspServerStatus {
            name: spec.display_name,
            language_id: spec.language_id,
            path: find_executable(&spec.program),
            program: spec.program,
            args: spec.args.join(" "),
            install: spec.install_hint,
        })
        .collect();
    rows.sort_by(|a, b| {
        b.is_ready()
            .cmp(&a.is_ready())
            .then_with(|| a.name.cmp(b.name))
    });
    rows
}

/// Resolve `program` the way the editor does when it spawns the server: an
/// absolute path is used as-is, otherwise the enriched GUI `PATH` (a macOS
/// `.app` inherits a minimal one) plus the usual install locations.
pub(crate) fn find_executable(program: &str) -> Option<PathBuf> {
    if program.is_empty() {
        return None;
    }
    let direct = Path::new(program);
    if direct.is_absolute() || program.contains('/') {
        return direct.is_file().then(|| direct.to_path_buf());
    }
    for dir in search_dirs() {
        let candidate = dir.join(program);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn search_dirs() -> Vec<PathBuf> {
    let home = std::env::var("HOME").ok().map(PathBuf::from);
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(home) = &home {
        for rel in [
            ".local/bin",
            ".cargo/bin",
            ".bun/bin",
            ".opencode/bin",
            ".pub-cache/bin",
            ".dotnet/tools",
            ".asdf/shims",
            ".volta/bin",
        ] {
            dirs.push(home.join(rel));
        }
    }
    for abs in [
        "/opt/homebrew/bin",
        "/opt/homebrew/sbin",
        "/usr/local/bin",
        "/usr/local/sbin",
        "/usr/bin",
        "/bin",
        "/usr/sbin",
        "/sbin",
    ] {
        dirs.push(PathBuf::from(abs));
    }
    if let Some(paths) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&paths));
    }
    dirs
}

/// A short human label for where a server came from, for the Settings row.
pub(crate) fn compact_path(path: &Path) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let text = path.display().to_string();
    if !home.is_empty() {
        if let Some(rest) = text.strip_prefix(&home) {
            return format!("~{rest}");
        }
    }
    text
}
