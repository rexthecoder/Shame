//! Workspace symbol index: top-level Dart declarations (classes, mixins,
//! extensions, enums, typedefs and functions) found by a line scan, so
//! "go to symbol in workspace" is instant and works without a running
//! language server.

use std::path::{Path, PathBuf};

const IGNORED_NAMES: &[&str] = &[".git", "target", "node_modules", "build", ".dart_tool", "Pods"];
const MAX_FILES: usize = 5_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceSymbol {
    pub name: String,
    /// Short kind label shown in the palette: `class`, `mixin`, `enum`, `ext`, `type`, `fn`.
    pub kind: &'static str,
    pub path: PathBuf,
    /// 0-indexed line of the declaration.
    pub line: u32,
}

/// Every top-level declaration in every `.dart` file under `root`.
pub fn index_workspace_symbols(root: &Path) -> Vec<WorkspaceSymbol> {
    let mut out = Vec::new();
    let mut files = 0;
    walk(root, &mut out, &mut files);
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

fn walk(dir: &Path, out: &mut Vec<WorkspaceSymbol>, files: &mut usize) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
        if name.starts_with('.') || IGNORED_NAMES.contains(&name) {
            continue;
        }
        if path.is_dir() {
            walk(&path, out, files);
        } else if path.extension().and_then(|e| e.to_str()) == Some("dart") {
            if *files >= MAX_FILES {
                return;
            }
            *files += 1;
            if let Ok(text) = std::fs::read_to_string(&path) {
                out.extend(symbols_in_text(&text, &path));
            }
        }
    }
}

/// Top-level declarations in one Dart source text.
pub fn symbols_in_text(text: &str, path: &Path) -> Vec<WorkspaceSymbol> {
    let mut out = Vec::new();
    for (line_ix, line) in text.lines().enumerate() {
        if let Some((name, kind)) = declaration(line) {
            out.push(WorkspaceSymbol {
                name,
                kind,
                path: path.to_path_buf(),
                line: line_ix as u32,
            });
        }
    }
    out
}

fn identifier(s: &str) -> Option<&str> {
    let end = s
        .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
        .unwrap_or(s.len());
    (end > 0 && !s.starts_with(|c: char| c.is_ascii_digit())).then(|| &s[..end])
}

/// Parse one column-0 line as a declaration, if it is one.
fn declaration(line: &str) -> Option<(String, &'static str)> {
    // Top-level only: members are indented, and continuation lines start with
    // whitespace, `}`, `@`, `/` or `)`.
    if line.is_empty() || line.starts_with(|c: char| c.is_whitespace() || "}@/)*".contains(c)) {
        return None;
    }

    let mut rest = line;
    let mut kind = None;
    // Strip class modifiers, then look for the declaring keyword.
    loop {
        let word = rest.split_whitespace().next()?;
        match word {
            "abstract" | "sealed" | "base" | "final" | "interface" | "mixin"
                if rest[word.len()..].trim_start().starts_with("class ")
                    || rest[word.len()..].trim_start().split_whitespace().next().is_some_and(|w| {
                        matches!(w, "class" | "abstract" | "sealed" | "base" | "final" | "interface")
                    }) =>
            {
                rest = rest[word.len()..].trim_start();
            }
            "class" => {
                kind = Some("class");
                rest = rest[word.len()..].trim_start();
                break;
            }
            "mixin" => {
                kind = Some("mixin");
                rest = rest[word.len()..].trim_start();
                break;
            }
            "enum" => {
                kind = Some("enum");
                rest = rest[word.len()..].trim_start();
                break;
            }
            "extension" => {
                kind = Some("ext");
                rest = rest[word.len()..].trim_start();
                // `extension on T` has no name.
                if rest.starts_with("on ") {
                    return None;
                }
                if let Some(stripped) = rest.strip_prefix("type ") {
                    rest = stripped.trim_start();
                }
                break;
            }
            "typedef" => {
                kind = Some("type");
                rest = rest[word.len()..].trim_start();
                break;
            }
            _ => break,
        }
    }
    if let Some(kind) = kind {
        return identifier(rest).map(|n| (n.to_string(), kind));
    }

    // Top-level function: `<return type> name(`, not a statement or directive.
    let first = line.split_whitespace().next()?;
    if matches!(
        first,
        "import" | "export" | "part" | "library" | "if" | "for" | "while" | "switch" | "return"
            | "else" | "new" | "const" | "var" | "late" | "static"
    ) {
        return None;
    }
    let paren = line.find('(')?;
    let head = line[..paren].trim_end();
    if head.contains('=') || head.contains(' ') == false {
        return None;
    }
    let name_start = head.rfind(|c: char| c.is_whitespace() || c == '>' || c == '?').map_or(0, |i| i + 1);
    let name = &head[name_start..];
    let name = identifier(name).filter(|n| n.len() == name.len())?;
    Some((name.to_string(), "fn"))
}

/// Case-insensitive substring filter; names starting with the query sort first.
pub fn filter_symbols<'a>(
    symbols: &'a [WorkspaceSymbol],
    query: &str,
    limit: usize,
) -> Vec<&'a WorkspaceSymbol> {
    let q = query.trim().to_lowercase();
    let mut hits: Vec<&WorkspaceSymbol> = symbols
        .iter()
        .filter(|s| q.is_empty() || s.name.to_lowercase().contains(&q))
        .collect();
    hits.sort_by_key(|s| !s.name.to_lowercase().starts_with(&q));
    hits.truncate(limit);
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(text: &str) -> Vec<(String, &'static str)> {
        symbols_in_text(text, Path::new("a.dart"))
            .into_iter()
            .map(|s| (s.name, s.kind))
            .collect()
    }

    #[test]
    fn finds_type_declarations_with_modifiers() {
        let text = "class A {}\nabstract class B extends A {}\nsealed class C {}\nmixin M on A {}\nenum E { x }\nextension X on String {}\nextension on int {}\ntypedef Cb = void Function();\nabstract interface class I {}\nmixin class MC {}\n";
        assert_eq!(
            names(text),
            vec![
                ("A".into(), "class"),
                ("B".into(), "class"),
                ("C".into(), "class"),
                ("M".into(), "mixin"),
                ("E".into(), "enum"),
                ("X".into(), "ext"),
                ("Cb".into(), "type"),
                ("I".into(), "class"),
                ("MC".into(), "class"),
            ]
        );
    }

    #[test]
    fn finds_top_level_functions_but_not_statements_or_members() {
        let text = "import 'a.dart';\nvoid main() {\n  print('x');\n}\nFuture<int> load(String p) async {}\nint get x => 1;\nString? maybe() => null;\n  void member() {}\nvar f = foo(1);\nif (x) y();\n";
        assert_eq!(
            names(text),
            vec![
                ("main".into(), "fn"),
                ("load".into(), "fn"),
                ("maybe".into(), "fn"),
            ]
        );
    }

    #[test]
    fn filter_prefers_prefix_matches_and_is_case_insensitive() {
        let sym = |n: &str| WorkspaceSymbol {
            name: n.into(),
            kind: "class",
            path: PathBuf::from("a.dart"),
            line: 0,
        };
        let all = vec![sym("MyWidget"), sym("Widget"), sym("Other")];
        let hits = filter_symbols(&all, "widget", 10);
        assert_eq!(hits.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), vec!["Widget", "MyWidget"]);
    }
}
