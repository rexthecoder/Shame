//! Language-agnostic adapter trait (`LanguageAdapter`) and built-in language
//! implementations (Dart/Flutter, Rust, TypeScript/JavaScript, and a generic
//! fallback).
//!
//! Keeps `EditorView` and `lsp_integration` free of language- or framework-
//! specific hardcoding: each language provides its own LSP server command,
//! workspace root discovery, keywords, snippets, built-in completions, AST/text
//! code assists, and inline closing labels.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use core_buffer::Buffer;
use core_lsp::lsp_types::{
    CodeAction, CodeActionOrCommand, Position, Range, TextEdit, Uri, WorkspaceEdit,
};

use crate::lsp_integration::{CompletionEntry, CompletionKindBadge};

#[derive(Clone, Copy, Debug)]
pub struct SnippetSpec {
    pub trigger: &'static str,
    pub detail: &'static str,
    pub body: &'static str,
}

/// Pluggable language & LSP provider interface for `EditorView`.
pub trait LanguageAdapter: Send + Sync {
    /// LSP `languageId` (e.g. `"dart"`, `"rust"`, `"typescript"`).
    fn language_id(&self) -> &'static str;

    /// Whether this adapter handles `path` (usually by file extension).
    fn matches_path(&self, path: &Path) -> bool;

    /// Markers that identify the enclosing project/workspace root when walking
    /// upward from a file path (e.g. `["pubspec.yaml"]`, `["Cargo.toml"]`).
    fn root_markers(&self) -> &'static [&'static str] {
        &[".git"]
    }

    /// Walk upward from `file_path` to find the nearest project root containing
    /// any of `self.root_markers()`, falling back to `.git` or the file's directory.
    fn find_project_root(&self, file_path: &Path) -> Option<PathBuf> {
        for ancestor in file_path.ancestors().skip(1) {
            for marker in self.root_markers() {
                if ancestor.join(marker).exists() {
                    return Some(ancestor.to_path_buf());
                }
            }
        }
        for ancestor in file_path.ancestors().skip(1) {
            if ancestor.join(".git").exists() {
                return Some(ancestor.to_path_buf());
            }
        }
        file_path.parent().map(Path::to_path_buf)
    }

    /// Return `(program, args)` to spawn this language's LSP server, or `None`
    /// if this language does not use an external LSP process.
    fn lsp_command(&self, _project_root: Option<&Path>) -> Option<(String, Vec<String>)> {
        None
    }

    /// Language keywords for local completion fallback.
    fn keywords(&self) -> &'static [&'static str] {
        &[]
    }

    /// Language snippets for local completion fallback.
    fn snippets(&self) -> &'static [SnippetSpec] {
        &[]
    }

    /// Language- or framework-specific local completion entries (e.g. named
    /// arguments, standard library types, constructors).
    fn builtin_completions(&self, _prefix: &str, _after_dot: bool) -> Vec<CompletionEntry> {
        Vec::new()
    }

    /// Language-specific local code actions / assists (merged with LSP code actions).
    fn builtin_code_actions(
        &self,
        _text: &str,
        _buffer: &Buffer,
        _line: u32,
        _indent_width: usize,
        _uri: &Uri,
    ) -> Vec<CodeActionOrCommand> {
        Vec::new()
    }

    /// Optional end-of-line closing annotations (e.g. `// WidgetName` on closing
    /// multi-line constructor lines). Returns `0-indexed line -> label`.
    fn closing_labels(&self, _text: &str) -> HashMap<usize, String> {
        HashMap::new()
    }

    /// Detect if `line_text` in `file_path` is a test entry point (`void main(`,
    /// `test(`, `testWidgets(`, `group(`).
    /// Returns `Some((is_whole_file, Option<test_name>))` when the line should
    /// display inline test CodeLens buttons (`Run` + `launch.json` test templates).
    fn detect_test_target(
        &self,
        _file_path: Option<&Path>,
        _line_text: &str,
    ) -> Option<(bool, Option<String>)> {
        None
    }
}

/// Resolve the appropriate `LanguageAdapter` for a file path (or a generic
/// adapter when the buffer is untitled or has an unrecognized extension).
fn adapters() -> &'static [Arc<dyn LanguageAdapter>] {
    static REGISTRY: OnceLock<Vec<Arc<dyn LanguageAdapter>>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let mut list: Vec<Arc<dyn LanguageAdapter>> = vec![
            Arc::new(DartLanguageAdapter),
            Arc::new(RustLanguageAdapter),
            Arc::new(TypeScriptLanguageAdapter),
        ];
        for spec in BUILTIN_SIMPLE_LANGUAGES {
            list.push(Arc::new(SimpleLanguageAdapter(spec)));
        }
        list
    })
}

pub fn language_for_path(path: Option<&Path>) -> Arc<dyn LanguageAdapter> {
    let adapters = adapters();

    if let Some(p) = path {
        for adapter in adapters {
            if adapter.matches_path(p) {
                return adapter.clone();
            }
        }
    }
    static GENERIC: OnceLock<Arc<dyn LanguageAdapter>> = OnceLock::new();
    GENERIC
        .get_or_init(|| Arc::new(GenericLanguageAdapter))
        .clone()
}

pub struct SimpleLanguageSpec {
    pub language_id: &'static str,
    pub extensions: &'static [&'static str],
    pub exact_names: &'static [&'static str],
    pub root_markers: &'static [&'static str],
    pub lsp_bin: &'static str,
    pub lsp_args: &'static [&'static str],
    pub keywords: &'static [&'static str],
}

pub struct SimpleLanguageAdapter(pub &'static SimpleLanguageSpec);

impl LanguageAdapter for SimpleLanguageAdapter {
    fn language_id(&self) -> &'static str {
        self.0.language_id
    }

    fn matches_path(&self, path: &Path) -> bool {
        if let Some(ext) = path.extension().and_then(|e| e.to_str())
            && self
                .0
                .extensions
                .iter()
                .any(|cand| cand.eq_ignore_ascii_case(ext))
        {
            return true;
        }
        if let Some(name) = path.file_name().and_then(|n| n.to_str())
            && self.0.exact_names.contains(&name)
        {
            return true;
        }
        false
    }

    fn root_markers(&self) -> &'static [&'static str] {
        self.0.root_markers
    }

    fn lsp_command(&self, _project_root: Option<&Path>) -> Option<(String, Vec<String>)> {
        if self.0.lsp_bin.is_empty() {
            None
        } else {
            Some((
                self.0.lsp_bin.to_string(),
                self.0.lsp_args.iter().map(|s| (*s).to_string()).collect(),
            ))
        }
    }

    fn keywords(&self) -> &'static [&'static str] {
        self.0.keywords
    }
}

static BUILTIN_SIMPLE_LANGUAGES: &[SimpleLanguageSpec] = &[
    SimpleLanguageSpec {
        language_id: "python",
        extensions: &["py", "pyi"],
        exact_names: &[],
        root_markers: &["pyproject.toml", "setup.py", "requirements.txt"],
        lsp_bin: "pyright-langserver",
        lsp_args: &["--stdio"],
        keywords: &[
            "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del",
            "elif", "else", "except", "False", "finally", "for", "from", "global", "if", "import",
            "in", "is", "lambda", "None", "nonlocal", "not", "or", "pass", "raise", "return",
            "self", "True", "try", "while", "with", "yield",
        ],
    },
    SimpleLanguageSpec {
        language_id: "go",
        extensions: &["go"],
        exact_names: &[],
        root_markers: &["go.mod", "go.work"],
        lsp_bin: "gopls",
        lsp_args: &[],
        keywords: &[
            "break", "case", "chan", "const", "continue", "default", "defer", "else",
            "fallthrough", "for", "func", "go", "goto", "if", "import", "interface", "map",
            "package", "range", "return", "select", "struct", "switch", "type", "var", "error",
            "string", "int", "bool", "nil", "true", "false",
        ],
    },
    SimpleLanguageSpec {
        language_id: "swift",
        extensions: &["swift"],
        exact_names: &["Package.swift"],
        root_markers: &["Package.swift", "Podfile", "Runner.xcodeproj"],
        lsp_bin: "sourcekit-lsp",
        lsp_args: &[],
        keywords: &[
            "actor", "as", "async", "await", "break", "case", "catch", "class", "continue",
            "default", "defer", "do", "else", "enum", "extension", "fallthrough", "false",
            "fileprivate", "for", "func", "guard", "if", "import", "in", "init", "inout",
            "internal", "is", "let", "nil", "open", "operator", "private", "protocol", "public",
            "repeat", "rethrows", "return", "self", "Self", "some", "static", "struct", "subscript",
            "super", "switch", "throw", "throws", "true", "try", "typealias", "var", "where",
            "while",
        ],
    },
    SimpleLanguageSpec {
        language_id: "kotlin",
        extensions: &["kt", "kts"],
        exact_names: &[],
        root_markers: &["build.gradle.kts", "build.gradle", "settings.gradle.kts", "settings.gradle"],
        lsp_bin: "kotlin-language-server",
        lsp_args: &[],
        keywords: &[
            "abstract", "actual", "annotation", "as", "break", "by", "catch", "class", "companion",
            "const", "constructor", "continue", "coroutine", "crossinline", "data", "do", "else",
            "enum", "expect", "external", "false", "final", "finally", "for", "fun", "if",
            "import", "in", "infix", "init", "inline", "inner", "interface", "internal", "is",
            "lateinit", "noinline", "null", "object", "open", "operator", "out", "override",
            "package", "private", "protected", "public", "reified", "return", "sealed", "super",
            "suspend", "tailrec", "this", "throw", "true", "try", "typealias", "val", "var",
            "vararg", "when", "where", "while",
        ],
    },
    SimpleLanguageSpec {
        language_id: "cpp",
        extensions: &["c", "h", "cc", "cpp", "cxx", "hpp", "hh", "m", "mm"],
        exact_names: &[],
        root_markers: &["CMakeLists.txt", "compile_commands.json", "Makefile"],
        lsp_bin: "clangd",
        lsp_args: &[],
        keywords: &[
            "alignas", "alignof", "auto", "bool", "break", "case", "catch", "char", "class",
            "const", "constexpr", "continue", "default", "delete", "do", "double", "else", "enum",
            "explicit", "extern", "false", "final", "float", "for", "friend", "goto", "if",
            "inline", "int", "long", "mutable", "namespace", "new", "noexcept", "nullptr",
            "operator", "override", "private", "protected", "public", "return", "short", "signed",
            "sizeof", "static", "static_cast", "struct", "switch", "template", "this", "throw",
            "true", "try", "typedef", "typename", "union", "unsigned", "using", "virtual", "void",
            "volatile", "while",
        ],
    },
    SimpleLanguageSpec {
        language_id: "java",
        extensions: &["java"],
        exact_names: &[],
        root_markers: &["pom.xml", "build.gradle", "build.gradle.kts"],
        lsp_bin: "jdtls",
        lsp_args: &[],
        keywords: &[
            "abstract", "assert", "boolean", "break", "byte", "case", "catch", "char", "class",
            "continue", "default", "do", "double", "else", "enum", "extends", "false", "final",
            "finally", "float", "for", "if", "implements", "import", "instanceof", "int",
            "interface", "long", "native", "new", "null", "package", "private", "protected",
            "public", "record", "return", "short", "static", "super", "switch", "synchronized",
            "this", "throw", "throws", "transient", "true", "try", "var", "void", "volatile",
            "while",
        ],
    },
    SimpleLanguageSpec {
        language_id: "ruby",
        extensions: &["rb", "gemspec", "rake"],
        exact_names: &["Gemfile", "Podfile", "Fastfile", "Rakefile"],
        root_markers: &["Gemfile", "Podfile"],
        lsp_bin: "ruby-lsp",
        lsp_args: &[],
        keywords: &[
            "alias", "and", "begin", "break", "case", "class", "def", "defined?", "do", "else",
            "elsif", "end", "ensure", "false", "for", "if", "in", "module", "next", "nil", "not",
            "or", "redo", "rescue", "retry", "return", "self", "super", "then", "true", "undef",
            "unless", "until", "when", "while", "yield", "require", "include",
        ],
    },
    SimpleLanguageSpec {
        language_id: "zig",
        extensions: &["zig"],
        exact_names: &[],
        root_markers: &["build.zig", "build.zig.zon"],
        lsp_bin: "zls",
        lsp_args: &[],
        keywords: &[
            "addrspace", "align", "allowzero", "and", "anyframe", "anytype", "asm", "async",
            "await", "break", "callconv", "catch", "comptime", "const", "continue", "defer",
            "else", "enum", "errdefer", "error", "export", "extern", "false", "fn", "for", "if",
            "inline", "noalias", "nosuspend", "null", "opaque", "or", "orelse", "packed", "pub",
            "resume", "return", "struct", "suspend", "switch", "test", "threadlocal", "true",
            "try", "undefined", "union", "unreachable", "var", "volatile", "while",
        ],
    },
    SimpleLanguageSpec {
        language_id: "csharp",
        extensions: &["cs"],
        exact_names: &[],
        root_markers: &[".sln", ".csproj"],
        lsp_bin: "csharp-ls",
        lsp_args: &[],
        keywords: &[
            "abstract", "as", "async", "await", "base", "bool", "break", "byte", "case", "catch",
            "char", "checked", "class", "const", "continue", "decimal", "default", "delegate",
            "do", "double", "else", "enum", "event", "explicit", "extern", "false", "finally",
            "fixed", "float", "for", "foreach", "goto", "if", "implicit", "in", "int", "interface",
            "internal", "is", "lock", "long", "namespace", "new", "null", "object", "operator",
            "out", "override", "params", "private", "protected", "public", "readonly", "record",
            "ref", "return", "sealed", "short", "sizeof", "static", "string", "struct", "switch",
            "this", "throw", "true", "try", "typeof", "uint", "ulong", "unchecked", "unsafe",
            "ushort", "using", "var", "virtual", "void", "volatile", "while", "yield",
        ],
    },
    SimpleLanguageSpec {
        language_id: "php",
        extensions: &["php"],
        exact_names: &[],
        root_markers: &["composer.json"],
        lsp_bin: "intelephense",
        lsp_args: &["--stdio"],
        keywords: &[
            "abstract", "array", "as", "break", "callable", "case", "catch", "class", "clone",
            "const", "continue", "declare", "default", "do", "echo", "else", "elseif", "empty",
            "enum", "extends", "false", "final", "finally", "fn", "for", "foreach", "function",
            "global", "if", "implements", "include", "instanceof", "interface", "isset", "list",
            "match", "namespace", "new", "null", "private", "protected", "public", "readonly",
            "require", "return", "self", "static", "switch", "throw", "trait", "true", "try",
            "unset", "use", "var", "while", "yield",
        ],
    },
    SimpleLanguageSpec {
        language_id: "lua",
        extensions: &["lua"],
        exact_names: &[],
        root_markers: &[".luarc.json", ".git"],
        lsp_bin: "lua-language-server",
        lsp_args: &[],
        keywords: &[
            "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "goto",
            "if", "in", "local", "nil", "not", "or", "repeat", "return", "then", "true", "until",
            "while",
        ],
    },
    SimpleLanguageSpec {
        language_id: "html",
        extensions: &["html", "htm"],
        exact_names: &[],
        root_markers: &["package.json", "index.html"],
        lsp_bin: "vscode-html-language-server",
        lsp_args: &["--stdio"],
        keywords: &["div", "span", "section", "header", "footer", "main", "button", "input", "form"],
    },
    SimpleLanguageSpec {
        language_id: "css",
        extensions: &["css", "scss", "less"],
        exact_names: &[],
        root_markers: &["package.json"],
        lsp_bin: "vscode-css-language-server",
        lsp_args: &["--stdio"],
        keywords: &[
            "display", "flex", "grid", "position", "color", "background", "border", "padding",
            "margin", "width", "height", "font-size", "font-weight", "align-items",
            "justify-content",
        ],
    },
    SimpleLanguageSpec {
        language_id: "json",
        extensions: &["json", "jsonc", "arb"],
        exact_names: &[],
        root_markers: &["package.json", "pubspec.yaml"],
        lsp_bin: "vscode-json-language-server",
        lsp_args: &["--stdio"],
        keywords: &["true", "false", "null"],
    },
    SimpleLanguageSpec {
        language_id: "yaml",
        extensions: &["yaml", "yml"],
        exact_names: &[],
        root_markers: &["pubspec.yaml", ".git"],
        lsp_bin: "yaml-language-server",
        lsp_args: &["--stdio"],
        keywords: &["true", "false", "null", "dependencies", "dev_dependencies", "flutter", "environment"],
    },
    SimpleLanguageSpec {
        language_id: "toml",
        extensions: &["toml"],
        exact_names: &["Cargo.lock"],
        root_markers: &["Cargo.toml", "pyproject.toml"],
        lsp_bin: "taplo",
        lsp_args: &["lsp", "stdio"],
        keywords: &["package", "dependencies", "workspace", "features", "profile", "true", "false"],
    },
    SimpleLanguageSpec {
        language_id: "shellscript",
        extensions: &["sh", "bash", "zsh"],
        exact_names: &[".zshrc", ".bashrc", ".bash_profile"],
        root_markers: &[".git"],
        lsp_bin: "bash-language-server",
        lsp_args: &["start"],
        keywords: &[
            "if", "then", "else", "elif", "fi", "case", "esac", "for", "select", "while", "until",
            "do", "done", "in", "function", "time", "export", "local", "readonly", "return", "exit",
            "echo", "printf", "set",
        ],
    },
    SimpleLanguageSpec {
        language_id: "sql",
        extensions: &["sql"],
        exact_names: &[],
        root_markers: &[".git"],
        lsp_bin: "sql-language-server",
        lsp_args: &["up", "--method", "stdio"],
        keywords: &[
            "SELECT", "FROM", "WHERE", "INSERT", "INTO", "VALUES", "UPDATE", "SET", "DELETE",
            "CREATE", "TABLE", "ALTER", "DROP", "INDEX", "VIEW", "JOIN", "INNER", "LEFT", "RIGHT",
            "ON", "GROUP", "BY", "ORDER", "HAVING", "LIMIT", "OFFSET", "AS", "AND", "OR", "NOT",
            "NULL", "PRIMARY", "KEY", "FOREIGN", "REFERENCES", "UNIQUE", "DEFAULT", "BEGIN",
            "COMMIT", "ROLLBACK",
        ],
    },
    SimpleLanguageSpec {
        language_id: "markdown",
        extensions: &["md", "mdx", "markdown"],
        exact_names: &["README", "CHANGELOG"],
        root_markers: &[".git"],
        lsp_bin: "marksman",
        lsp_args: &["server"],
        keywords: &[],
    },
];

// ============================================================================
// 1. Generic Fallback Language Adapter
// ============================================================================

pub struct GenericLanguageAdapter;

impl LanguageAdapter for GenericLanguageAdapter {
    fn language_id(&self) -> &'static str {
        "plaintext"
    }

    fn matches_path(&self, _path: &Path) -> bool {
        true
    }
}

// ============================================================================
// 2. Rust Language Adapter (`rust-analyzer`)
// ============================================================================

pub struct RustLanguageAdapter;

impl LanguageAdapter for RustLanguageAdapter {
    fn language_id(&self) -> &'static str {
        "rust"
    }

    fn matches_path(&self, path: &Path) -> bool {
        path.extension().and_then(|e| e.to_str()) == Some("rs")
    }

    fn root_markers(&self) -> &'static [&'static str] {
        &["Cargo.toml"]
    }

    fn lsp_command(&self, _project_root: Option<&Path>) -> Option<(String, Vec<String>)> {
        Some(("rust-analyzer".to_string(), Vec::new()))
    }

    fn keywords(&self) -> &'static [&'static str] {
        &[
            "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
            "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod",
            "move", "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super",
            "trait", "true", "type", "unsafe", "use", "where", "while", "Option", "Some", "None",
            "Result", "Ok", "Err", "String", "Vec", "Box", "Arc",
        ]
    }

    fn snippets(&self) -> &'static [SnippetSpec] {
        &[
            SnippetSpec {
                trigger: "fn",
                detail: "fn name() {}",
                body: "fn ${1:name}(${2}) {\n    $0\n}",
            },
            SnippetSpec {
                trigger: "test",
                detail: "#[test] fn ...",
                body: "#[test]\nfn ${1:it_works}() {\n    $0\n}",
            },
        ]
    }
}

// ============================================================================
// 3. TypeScript / JavaScript Language Adapter (`typescript-language-server`)
// ============================================================================

pub struct TypeScriptLanguageAdapter;

impl LanguageAdapter for TypeScriptLanguageAdapter {
    fn language_id(&self) -> &'static str {
        "typescript"
    }

    fn matches_path(&self, path: &Path) -> bool {
        matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("ts" | "tsx" | "js" | "jsx")
        )
    }

    fn root_markers(&self) -> &'static [&'static str] {
        &["tsconfig.json", "package.json"]
    }

    fn lsp_command(&self, _project_root: Option<&Path>) -> Option<(String, Vec<String>)> {
        Some((
            "typescript-language-server".to_string(),
            vec!["--stdio".to_string()],
        ))
    }

    fn keywords(&self) -> &'static [&'static str] {
        &[
            "async", "await", "break", "case", "catch", "class", "const", "continue", "default",
            "export", "extends", "false", "finally", "for", "from", "function", "if", "implements",
            "import", "in", "instanceof", "interface", "let", "new", "null", "of", "private",
            "protected", "public", "readonly", "return", "static", "super", "switch", "this",
            "throw", "true", "try", "type", "typeof", "undefined", "var", "void", "while",
            "Promise", "Record", "Array", "Map", "Set",
        ]
    }
}

// ============================================================================
// 4. Dart & Flutter Language Adapter (`dart language-server`)
// ============================================================================

pub struct DartLanguageAdapter;

impl DartLanguageAdapter {
    fn resolve_dart_binary(project_root: Option<&Path>) -> String {
        if let Some(root) = project_root {
            let fvm_cache_dart = root.join(".fvm/flutter_sdk/bin/cache/dart-sdk/bin/dart");
            if fvm_cache_dart.is_file() {
                return fvm_cache_dart.to_string_lossy().into_owned();
            }
            let fvm_dart = root.join(".fvm/flutter_sdk/bin/dart");
            if fvm_dart.is_file() {
                return fvm_dart.to_string_lossy().into_owned();
            }
        }
        if let Ok(home) = std::env::var("HOME") {
            let home_path = Path::new(&home);
            for rel in [
                "fvm/default/bin/cache/dart-sdk/bin/dart",
                "fvm/default/bin/dart",
                "development/flutter/bin/cache/dart-sdk/bin/dart",
                "flutter/bin/cache/dart-sdk/bin/dart",
            ] {
                let p = home_path.join(rel);
                if p.is_file() {
                    return p.to_string_lossy().into_owned();
                }
            }
        }
        for abs in ["/opt/homebrew/bin/dart", "/usr/local/bin/dart"] {
            if Path::new(abs).is_file() {
                return abs.to_string();
            }
        }
        "dart".to_string()
    }
}

impl LanguageAdapter for DartLanguageAdapter {
    fn language_id(&self) -> &'static str {
        "dart"
    }

    fn matches_path(&self, path: &Path) -> bool {
        path.extension().and_then(|e| e.to_str()) == Some("dart")
    }

    fn root_markers(&self) -> &'static [&'static str] {
        &["pubspec.yaml", ".dart_tool/package_config.json"]
    }

    fn lsp_command(&self, project_root: Option<&Path>) -> Option<(String, Vec<String>)> {
        let bin = Self::resolve_dart_binary(project_root);
        Some((bin, vec!["language-server".to_string()]))
    }

    fn keywords(&self) -> &'static [&'static str] {
        &[
            "abstract", "async", "await", "break", "case", "catch", "class", "const", "continue",
            "covariant", "default", "deferred", "do", "dynamic", "else", "enum", "export",
            "extends", "extension", "external", "factory", "false", "final", "finally", "for",
            "Function", "get", "hide", "if", "implements", "import", "in", "interface", "is",
            "late", "library", "mixin", "new", "null", "on", "operator", "part", "required",
            "rethrow", "return", "sealed", "set", "show", "static", "super", "switch", "sync",
            "this", "throw", "true", "try", "typedef", "var", "void", "when", "while", "with",
            "yield", "Widget", "BuildContext", "StatelessWidget", "StatefulWidget", "State",
            "Future", "Stream", "List", "Map", "Set", "String", "int", "double", "bool",
            "override",
        ]
    }

    fn snippets(&self) -> &'static [SnippetSpec] {
        // LSP snippet syntax, written for column 0: lines after the first take the
        // indentation of the line they are inserted on. `${1:Name}` used twice is
        // one placeholder shown in both places (Tab copies what you typed).
        &[
            SnippetSpec {
                trigger: "stless",
                detail: "Flutter StatelessWidget",
                body: "class ${1:MyWidget} extends StatelessWidget {\n  const ${1:MyWidget}({super.key});\n\n  @override\n  Widget build(BuildContext context) {\n    return ${2:const Placeholder()};\n  }\n}",
            },
            SnippetSpec {
                trigger: "stful",
                detail: "Flutter StatefulWidget with its State",
                body: "class ${1:MyWidget} extends StatefulWidget {\n  const ${1:MyWidget}({super.key});\n\n  @override\n  State<${1:MyWidget}> createState() => _${1:MyWidget}State();\n}\n\nclass _${1:MyWidget}State extends State<${1:MyWidget}> {\n  @override\n  Widget build(BuildContext context) {\n    return ${2:const Placeholder()};\n  }\n}",
            },
            SnippetSpec {
                trigger: "build",
                detail: "@override Widget build(BuildContext context)",
                body: "@override\nWidget build(BuildContext context) {\n  return ${1:const SizedBox.shrink()};\n}",
            },
            SnippetSpec {
                trigger: "initstate",
                detail: "@override void initState()",
                body: "@override\nvoid initState() {\n  super.initState();\n  $0\n}",
            },
            SnippetSpec {
                trigger: "dispose",
                detail: "@override void dispose()",
                body: "@override\nvoid dispose() {\n  ${1:_controller.dispose();}\n  super.dispose();\n}",
            },
            SnippetSpec {
                trigger: "setstate",
                detail: "setState(() { ... })",
                body: "setState(() {\n  $0\n});",
            },
            SnippetSpec {
                trigger: "listb",
                detail: "ListView.builder",
                body: "ListView.builder(\n  itemCount: ${1:items.length},\n  itemBuilder: (context, index) {\n    return ${2:const SizedBox.shrink()};\n  },\n)",
            },
            SnippetSpec {
                trigger: "futureb",
                detail: "FutureBuilder",
                body: "FutureBuilder<${1:Object}>(\n  future: ${2:future},\n  builder: (context, snapshot) {\n    if (snapshot.connectionState != ConnectionState.done) {\n      return const Center(child: CircularProgressIndicator());\n    }\n    return ${3:const SizedBox.shrink()};\n  },\n)",
            },
            SnippetSpec {
                trigger: "streamb",
                detail: "StreamBuilder",
                body: "StreamBuilder<${1:Object}>(\n  stream: ${2:stream},\n  builder: (context, snapshot) {\n    if (!snapshot.hasData) {\n      return const Center(child: CircularProgressIndicator());\n    }\n    return ${3:const SizedBox.shrink()};\n  },\n)",
            },
            SnippetSpec {
                trigger: "column",
                detail: "Column(children: [...])",
                body: "Column(\n  children: [\n    $0\n  ],\n)",
            },
            SnippetSpec {
                trigger: "row",
                detail: "Row(children: [...])",
                body: "Row(\n  children: [\n    $0\n  ],\n)",
            },
            SnippetSpec {
                trigger: "mainapp",
                detail: "main() with a MaterialApp",
                body: "import 'package:flutter/material.dart';\n\nvoid main() {\n  runApp(const ${1:MyApp}());\n}\n\nclass ${1:MyApp} extends StatelessWidget {\n  const ${1:MyApp}({super.key});\n\n  @override\n  Widget build(BuildContext context) {\n    return MaterialApp(\n      title: '${2:App}',\n      home: Scaffold(\n        appBar: AppBar(title: const Text('${2:App}')),\n        body: const Center(child: Text('Hello')),\n      ),\n    );\n  }\n}",
            },
            SnippetSpec {
                trigger: "testw",
                detail: "testWidgets(...)",
                body: "testWidgets('${1:description}', (tester) async {\n  await tester.pumpWidget(${2:const MyApp()});\n  $0\n});",
            },
            SnippetSpec {
                trigger: "testd",
                detail: "test(...)",
                body: "test('${1:description}', () {\n  $0\n});",
            },
        ]
    }

    fn builtin_completions(&self, prefix: &str, after_dot: bool) -> Vec<CompletionEntry> {
        if after_dot {
            return Vec::new();
        }
        let prefix_lower = prefix.to_lowercase();
        let mut results = Vec::new();

        const NAMED_ARGS: &[(&str, &str, &str)] = &[
            ("child:", "child: ", "Widget?"),
            ("children:", "children: [\n  \n],", "List<Widget>"),
            ("padding:", "padding: const EdgeInsets.all(8.0),", "EdgeInsetsGeometry"),
            ("margin:", "margin: const EdgeInsets.all(8.0),", "EdgeInsetsGeometry"),
            ("mainAxisAlignment:", "mainAxisAlignment: MainAxisAlignment.center,", "MainAxisAlignment"),
            ("crossAxisAlignment:", "crossAxisAlignment: CrossAxisAlignment.start,", "CrossAxisAlignment"),
            ("mainAxisSize:", "mainAxisSize: MainAxisSize.min,", "MainAxisSize"),
            ("onPressed:", "onPressed: () {},", "VoidCallback?"),
            ("onTap:", "onTap: () {},", "GestureTapCallback?"),
            ("color:", "color: ", "Color?"),
            ("backgroundColor:", "backgroundColor: ", "Color?"),
            ("decoration:", "decoration: const BoxDecoration(),", "Decoration?"),
            ("style:", "style: const TextStyle(),", "TextStyle?"),
            ("width:", "width: ", "double?"),
            ("height:", "height: ", "double?"),
            ("alignment:", "alignment: Alignment.center,", "AlignmentGeometry"),
            ("builder:", "builder: (context) => ", "WidgetBuilder"),
            ("itemCount:", "itemCount: ", "int?"),
            ("itemBuilder:", "itemBuilder: (context, index) => ", "IndexedWidgetBuilder"),
            ("appBar:", "appBar: AppBar(),", "PreferredSizeWidget?"),
            ("body:", "body: ", "Widget?"),
            ("floatingActionButton:", "floatingActionButton: ", "Widget?"),
        ];
        for &(label, insert, ty) in NAMED_ARGS {
            if prefix_lower.is_empty() || label.to_lowercase().starts_with(&prefix_lower) {
                results.push(CompletionEntry {
                    snippet_stops: Vec::new(),
                    snippet_mirrors: Vec::new(),
                    label: label.to_string(),
                    insert_text: insert.to_string(),
                    detail: Some(ty.to_string()),
                    kind: CompletionKindBadge::Property,
                    additional_edits: Vec::new(),
                });
            }
        }

        const TYPES_AND_CONSTRUCTORS: &[(&str, &str, &str)] = &[
            ("Scaffold", "Scaffold(\n  body: \n)", "package:flutter/material.dart"),
            ("Container", "Container(\n  child: \n)", "package:flutter/widgets.dart"),
            ("Column", "Column(\n  children: [\n    \n  ],\n)", "package:flutter/widgets.dart"),
            ("Row", "Row(\n  children: [\n    \n  ],\n)", "package:flutter/widgets.dart"),
            ("Padding", "Padding(\n  padding: const EdgeInsets.all(8.0),\n  child: \n)", "package:flutter/widgets.dart"),
            ("Center", "Center(\n  child: \n)", "package:flutter/widgets.dart"),
            ("SizedBox", "SizedBox(\n  width: ,\n  height: ,\n)", "package:flutter/widgets.dart"),
            ("SizedBox.shrink", "const SizedBox.shrink()", "package:flutter/widgets.dart"),
            ("Expanded", "Expanded(\n  child: \n)", "package:flutter/widgets.dart"),
            ("Flexible", "Flexible(\n  child: \n)", "package:flutter/widgets.dart"),
            ("Text", "Text('')", "package:flutter/widgets.dart"),
            ("Icon", "Icon(Icons.)", "package:flutter/widgets.dart"),
            ("ElevatedButton", "ElevatedButton(\n  onPressed: () {},\n  child: const Text(''),\n)", "package:flutter/material.dart"),
            ("TextButton", "TextButton(\n  onPressed: () {},\n  child: const Text(''),\n)", "package:flutter/material.dart"),
            ("IconButton", "IconButton(\n  onPressed: () {},\n  icon: const Icon(Icons.add),\n)", "package:flutter/material.dart"),
            ("ListView.builder", "ListView.builder(\n  itemCount: 0,\n  itemBuilder: (context, index) {\n    return const SizedBox();\n  },\n)", "package:flutter/widgets.dart"),
            ("Stack", "Stack(\n  children: [\n    \n  ],\n)", "package:flutter/widgets.dart"),
            ("Positioned", "Positioned(\n  child: \n)", "package:flutter/widgets.dart"),
            ("SafeArea", "SafeArea(\n  child: \n)", "package:flutter/widgets.dart"),
            ("GestureDetector", "GestureDetector(\n  onTap: () {},\n  child: \n)", "package:flutter/widgets.dart"),
            ("InkWell", "InkWell(\n  onTap: () {},\n  child: \n)", "package:flutter/material.dart"),
            ("Card", "Card(\n  child: \n)", "package:flutter/material.dart"),
            ("Divider", "Divider()", "package:flutter/material.dart"),
            ("CircularProgressIndicator", "const CircularProgressIndicator()", "package:flutter/material.dart"),
            ("FutureBuilder", "FutureBuilder(\n  future: null,\n  builder: (context, snapshot) {\n    return const SizedBox();\n  },\n)", "package:flutter/widgets.dart"),
            ("StreamBuilder", "StreamBuilder(\n  stream: null,\n  builder: (context, snapshot) {\n    return const SizedBox();\n  },\n)", "package:flutter/widgets.dart"),
            ("EdgeInsets.all", "const EdgeInsets.all(8.0)", "package:flutter/painting.dart"),
            ("EdgeInsets.symmetric", "const EdgeInsets.symmetric(horizontal: 16.0, vertical: 8.0)", "package:flutter/painting.dart"),
            ("BoxDecoration", "BoxDecoration(\n  color: ,\n  borderRadius: BorderRadius.circular(8.0),\n)", "package:flutter/painting.dart"),
            ("TextStyle", "TextStyle(\n  fontSize: 14.0,\n  fontWeight: FontWeight.w500,\n)", "package:flutter/painting.dart"),
            ("MainAxisAlignment", "MainAxisAlignment.center", "package:flutter/rendering.dart"),
            ("CrossAxisAlignment", "CrossAxisAlignment.start", "package:flutter/rendering.dart"),
            ("Colors", "Colors.blue", "package:flutter/material.dart"),
            ("Icons", "Icons.check", "package:flutter/material.dart"),
            ("Theme.of", "Theme.of(context)", "package:flutter/material.dart"),
            ("MediaQuery.of", "MediaQuery.of(context)", "package:flutter/widgets.dart"),
            ("Navigator.of", "Navigator.of(context)", "package:flutter/widgets.dart"),
        ];
        if !prefix_lower.is_empty() {
            for &(label, insert, pkg) in TYPES_AND_CONSTRUCTORS {
                if label != prefix && label.to_lowercase().contains(&prefix_lower) {
                    results.push(CompletionEntry {
                        snippet_stops: Vec::new(),
                        snippet_mirrors: Vec::new(),
                        label: label.to_string(),
                        insert_text: insert.to_string(),
                        detail: Some(pkg.to_string()),
                        kind: CompletionKindBadge::Type,
                        additional_edits: Vec::new(),
                    });
                }
            }
        }

        results
    }

    fn builtin_code_actions(
        &self,
        text: &str,
        buffer: &Buffer,
        line: u32,
        indent_width: usize,
        uri: &Uri,
    ) -> Vec<CodeActionOrCommand> {
        let line_idx = line as usize;
        if line_idx >= buffer.line_count() {
            return Vec::new();
        }
        let line_start_byte = buffer.line_to_byte(line_idx);
        let raw_line = buffer.line(line_idx);
        let display_line = raw_line.trim_end_matches(['\n', '\r']);
        let trimmed = display_line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") || trimmed.starts_with("import ") {
            return Vec::new();
        }

        let (target_range, expr_text, indent_str) =
            find_call_expression_range(text, buffer, line_idx, line_start_byte, display_line);
        if expr_text.trim().is_empty() {
            return Vec::new();
        }

        let step = " ".repeat(indent_width);
        let inner_indent = format!("{indent_str}{step}");
        let child_indent = format!("{indent_str}{step}{step}");
        let reindented_single = expr_text
            .lines()
            .enumerate()
            .map(|(i, l)| if i == 0 { l.to_string() } else { format!("{step}{l}") })
            .collect::<Vec<_>>()
            .join("\n");
        let reindented_list = expr_text
            .lines()
            .enumerate()
            .map(|(i, l)| {
                if i == 0 {
                    l.to_string()
                } else {
                    format!("{step}{step}{l}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n");

        let make_action = |title: &str, new_text: String| -> CodeActionOrCommand {
            let mut changes = HashMap::new();
            changes.insert(
                uri.clone(),
                vec![TextEdit {
                    range: target_range,
                    new_text,
                }],
            );
            CodeActionOrCommand::CodeAction(CodeAction {
                title: title.to_string(),
                kind: None,
                diagnostics: None,
                edit: Some(WorkspaceEdit {
                    changes: Some(changes),
                    document_changes: None,
                    change_annotations: None,
                }),
                command: None,
                is_preferred: None,
                disabled: None,
                data: None,
            })
        };

        vec![
            make_action(
                "Wrap with Padding",
                format!(
                    "Padding(\n{inner_indent}padding: const EdgeInsets.all(8.0),\n{inner_indent}child: {reindented_single},\n{indent_str})"
                ),
            ),
            make_action(
                "Wrap with Column",
                format!(
                    "Column(\n{inner_indent}children: [\n{child_indent}{reindented_list},\n{inner_indent}],\n{indent_str})"
                ),
            ),
            make_action(
                "Wrap with Row",
                format!(
                    "Row(\n{inner_indent}children: [\n{child_indent}{reindented_list},\n{inner_indent}],\n{indent_str})"
                ),
            ),
            make_action(
                "Wrap with Center",
                format!("Center(\n{inner_indent}child: {reindented_single},\n{indent_str})"),
            ),
            make_action(
                "Wrap with SizedBox",
                format!("SizedBox(\n{inner_indent}child: {reindented_single},\n{indent_str})"),
            ),
            make_action(
                "Wrap with Expanded",
                format!("Expanded(\n{inner_indent}child: {reindented_single},\n{indent_str})"),
            ),
            make_action(
                "Wrap with Container",
                format!("Container(\n{inner_indent}child: {reindented_single},\n{indent_str})"),
            ),
            make_action(
                "Wrap with SafeArea",
                format!("SafeArea(\n{inner_indent}child: {reindented_single},\n{indent_str})"),
            ),
            make_action(
                "Wrap with StreamBuilder",
                format!(
                    "StreamBuilder<Object>(\n{inner_indent}stream: null,\n{inner_indent}builder: (context, snapshot) {{\n{child_indent}return {reindented_list};\n{inner_indent}}},\n{indent_str})"
                ),
            ),
        ]
    }

    fn closing_labels(&self, text: &str) -> HashMap<usize, String> {
        let mut labels_by_line: HashMap<usize, Vec<String>> = HashMap::new();
        let bytes = text.as_bytes();
        let mut stack: Vec<(usize, Option<String>, u8)> = Vec::new();
        let mut line = 0usize;
        let mut i = 0usize;
        let mut in_single = false;
        let mut in_double = false;
        let mut in_line_comment = false;

        while i < bytes.len() {
            let b = bytes[i];
            if b == b'\n' {
                line += 1;
                in_line_comment = false;
                i += 1;
                continue;
            }
            if in_line_comment {
                i += 1;
                continue;
            }
            if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                in_line_comment = true;
                i += 2;
                continue;
            }
            if (in_single || in_double) && b == b'\\' {
                i += 2;
                continue;
            }
            if !in_double && b == b'\'' {
                in_single = !in_single;
                i += 1;
                continue;
            }
            if !in_single && b == b'"' {
                in_double = !in_double;
                i += 1;
                continue;
            }
            if in_single || in_double {
                i += 1;
                continue;
            }

            if b == b'(' {
                let mut end = i;
                if end > 0 && bytes[end - 1] == b'>' {
                    let mut angle = 1i32;
                    end -= 1;
                    while end > 0 && angle > 0 {
                        end -= 1;
                        if bytes[end] == b'>' {
                            angle += 1;
                        } else if bytes[end] == b'<' {
                            angle -= 1;
                        }
                    }
                }
                let ident_end = end;
                let mut start = ident_end;
                while start > 0
                    && (bytes[start - 1].is_ascii_alphanumeric()
                        || bytes[start - 1] == b'_'
                        || bytes[start - 1] == b'.')
                {
                    start -= 1;
                }
                let raw_name = &text[start..ident_end];
                let first_seg = raw_name.split('.').next().unwrap_or("");
                let is_pascal_call = first_seg
                    .bytes()
                    .next()
                    .is_some_and(|c| c.is_ascii_uppercase())
                    && first_seg.bytes().any(|c| c.is_ascii_lowercase());
                stack.push((line, is_pascal_call.then(|| raw_name.to_string()), b'('));
            } else if b == b'[' {
                let before = text[..i].trim_end();
                let is_children = before.ends_with("children:");
                stack.push((line, is_children.then(|| "[]".to_string()), b'['));
            } else if b == b')' || b == b']' {
                let expected = if b == b')' { b'(' } else { b'[' };
                if let Some((open_line, maybe_name, open_ch)) = stack.pop() {
                    if open_ch == expected && line > open_line + 1 {
                        if let Some(name) = maybe_name {
                            labels_by_line.entry(line).or_default().push(name);
                        }
                    }
                }
            }
            i += 1;
        }

        labels_by_line
            .into_iter()
            .map(|(ln, mut names)| {
                names.reverse();
                (ln, format!("// {}", names.join(", ")))
            })
            .collect()
    }

    fn detect_test_target(
        &self,
        file_path: Option<&Path>,
        line_text: &str,
    ) -> Option<(bool, Option<String>)> {
        let is_test_file = file_path
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .is_some_and(|name| name.ends_with("_test.dart"));
        let trimmed = line_text.trim_start();
        if is_test_file
            && (trimmed.starts_with("void main(") || trimmed.starts_with("Future<void> main("))
        {
            return Some((true, None));
        }
        for prefix in ["testWidgets(", "test(", "group("] {
            if let Some(rest) = trimmed.strip_prefix(prefix) {
                let rest = rest.trim_start();
                let name = if let Some(after_q) = rest.strip_prefix('\'') {
                    after_q.split('\'').next().map(str::to_string)
                } else if let Some(after_q) = rest.strip_prefix('"') {
                    after_q.split('"').next().map(str::to_string)
                } else {
                    None
                };
                return Some((false, name));
            }
        }
        None
    }
}

fn find_call_expression_range(
    text: &str,
    buffer: &Buffer,
    line_idx: usize,
    line_start_byte: usize,
    display_line: &str,
) -> (Range, String, String) {
    let indent_len = display_line.len() - display_line.trim_start().len();
    let indent_str = display_line[..indent_len].to_string();
    let trimmed = &display_line[indent_len..];

    let mut call_rel_start: Option<usize> = None;
    let mut open_paren_rel: Option<usize> = None;
    let bytes = trimmed.as_bytes();
    let mut idx = 0;
    while idx < bytes.len() {
        if bytes[idx].is_ascii_uppercase()
            && (idx == 0 || !(bytes[idx - 1].is_ascii_alphanumeric() || bytes[idx - 1] == b'_'))
        {
            let start = idx;
            while idx < bytes.len()
                && (bytes[idx].is_ascii_alphanumeric()
                    || bytes[idx] == b'_'
                    || bytes[idx] == b'.'
                    || bytes[idx] == b'<'
                    || bytes[idx] == b'>')
            {
                idx += 1;
            }
            if idx < bytes.len() && bytes[idx] == b'(' {
                let before = &trimmed[..start];
                let adj_start = if let Some(stripped) = before.strip_suffix("const ") {
                    stripped.len()
                } else {
                    start
                };
                call_rel_start = Some(indent_len + adj_start);
                open_paren_rel = Some(indent_len + idx);
                break;
            }
        } else {
            idx += 1;
        }
    }

    if let (Some(w_rel), Some(p_rel)) = (call_rel_start, open_paren_rel) {
        let abs_start = line_start_byte + w_rel;
        let abs_paren = line_start_byte + p_rel;
        let all_bytes = text.as_bytes();
        let mut depth = 0i32;
        let mut in_single = false;
        let mut in_double = false;
        let mut k = abs_paren;
        while k < all_bytes.len() {
            let b = all_bytes[k];
            if b == b'\\' {
                k += 2;
                continue;
            }
            if !in_double && b == b'\'' {
                in_single = !in_single;
            } else if !in_single && b == b'"' {
                in_double = !in_double;
            } else if !in_single && !in_double {
                if b == b'(' {
                    depth += 1;
                } else if b == b')' {
                    depth -= 1;
                    if depth == 0 {
                        let abs_end = k + 1;
                        let end_line = buffer.byte_to_line(abs_end) as u32;
                        let end_col = (abs_end - buffer.line_to_byte(end_line as usize)) as u32;
                        let expr = text[abs_start..abs_end].to_string();
                        return (
                            Range {
                                start: Position {
                                    line: line_idx as u32,
                                    character: w_rel as u32,
                                },
                                end: Position {
                                    line: end_line,
                                    character: end_col,
                                },
                            },
                            expr,
                            indent_str,
                        );
                    }
                }
            }
            k += 1;
        }
    }

    let stripped_end = trimmed.trim_end_matches([',', ';']).trim_end();
    let start_col = indent_len as u32;
    let end_col = (indent_len + stripped_end.len()) as u32;
    (
        Range {
            start: Position {
                line: line_idx as u32,
                character: start_col,
            },
            end: Position {
                line: line_idx as u32,
                character: end_col,
            },
        },
        stripped_end.to_string(),
        indent_str,
    )
}

/// One language server this build knows how to start. Used by the Settings
/// diagnostics list so a missing binary reads as an install step instead of
/// silently degrading to lexical highlighting.
#[derive(Clone, Debug)]
pub struct LanguageServerSpec {
    pub language_id: &'static str,
    pub display_name: &'static str,
    /// Executable that would be spawned (`dart` resolves to the Flutter SDK's).
    pub program: String,
    pub args: Vec<String>,
    /// The shortest command that installs it on macOS.
    pub install_hint: &'static str,
}

fn language_display_name(language_id: &str) -> &'static str {
    match language_id {
        "dart" => "Dart / Flutter",
        "rust" => "Rust",
        "typescript" => "TypeScript / JavaScript",
        "python" => "Python",
        "go" => "Go",
        "swift" => "Swift",
        "kotlin" => "Kotlin",
        "cpp" => "C / C++",
        "java" => "Java",
        "ruby" => "Ruby",
        "zig" => "Zig",
        "csharp" => "C#",
        "php" => "PHP",
        "lua" => "Lua",
        "html" => "HTML",
        "css" => "CSS",
        "json" => "JSON",
        "yaml" => "YAML",
        "toml" => "TOML",
        "bash" => "Bash",
        "sql" => "SQL",
        "markdown" => "Markdown",
        _ => "Other",
    }
}

fn language_install_hint(language_id: &str) -> &'static str {
    match language_id {
        "dart" => "Install the Flutter SDK (ships the Dart language server)",
        "rust" => "rustup component add rust-analyzer",
        "typescript" | "html" | "css" | "json" => {
            "npm i -g typescript typescript-language-server vscode-langservers-extracted"
        }
        "python" => "npm i -g pyright",
        "go" => "go install golang.org/x/tools/gopls@latest",
        "swift" => "xcode-select --install",
        "kotlin" => "brew install kotlin-language-server",
        "cpp" => "xcode-select --install  (Apple clangd)",
        "java" => "brew install jdtls",
        "ruby" => "gem install ruby-lsp",
        "zig" => "brew install zls",
        "csharp" => "dotnet tool install -g csharp-ls",
        "php" => "npm i -g intelephense",
        "lua" => "brew install lua-language-server",
        "yaml" => "npm i -g yaml-language-server",
        "toml" => "brew install taplo",
        "bash" => "npm i -g bash-language-server",
        "sql" => "npm i -g sql-language-server",
        "markdown" => "brew install marksman",
        _ => "",
    }
}

/// Every language server this build can start, deduplicated by language and
/// sorted for display. Dart's program is resolved through the Flutter SDK the
/// same way the editor itself resolves it.
pub fn language_server_specs(project_root: Option<&Path>) -> Vec<LanguageServerSpec> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for adapter in adapters() {
        let id = adapter.language_id();
        let Some((program, args)) = adapter.lsp_command(project_root) else {
            continue;
        };
        if !seen.insert(id) {
            continue;
        }
        out.push(LanguageServerSpec {
            language_id: id,
            display_name: language_display_name(id),
            program,
            args,
            install_hint: language_install_hint(id),
        });
    }
    out.sort_by(|a, b| a.display_name.cmp(b.display_name));
    out
}

#[cfg(test)]
mod snippet_tests {
    use super::*;
    use crate::snippet::parse_snippet;

    #[test]
    fn every_dart_snippet_expands_with_a_place_to_land() {
        let adapter = DartLanguageAdapter;
        let snippets = adapter.snippets();
        assert!(snippets.len() >= 10);
        let mut triggers = std::collections::HashSet::new();
        for spec in snippets {
            assert!(triggers.insert(spec.trigger), "duplicate trigger {}", spec.trigger);
            let parsed = parse_snippet(spec.body);
            assert!(!parsed.stops.is_empty(), "{} has no tab stops", spec.trigger);
            assert!(!parsed.text.contains('$'), "{} left raw snippet syntax: {}", spec.trigger, parsed.text);
            let opens = parsed.text.matches(['(', '{', '[']).count();
            let closes = parsed.text.matches([')', '}', ']']).count();
            assert_eq!(opens, closes, "{} has unbalanced brackets", spec.trigger);
        }
    }

    #[test]
    fn the_stateful_widget_name_is_one_placeholder_shown_six_times() {
        let adapter = DartLanguageAdapter;
        let spec = adapter.snippets().iter().find(|s| s.trigger == "stful").unwrap();
        let parsed = parse_snippet(spec.body);
        // First stop is the widget name; its five other appearances are mirrors.
        assert_eq!(&parsed.text[parsed.stops[0].clone()], "MyWidget");
        assert_eq!(parsed.mirrors[0].len(), 5);
        for mirror in &parsed.mirrors[0] {
            assert_eq!(&parsed.text[mirror.clone()], "MyWidget");
        }
    }
}
