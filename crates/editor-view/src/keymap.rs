//! Keyboard shortcuts: a table of keystroke -> command id.
//!
//! Defaults live in [`DEFAULT_BINDINGS`]. Users override them in
//! `~/.config/shame/keymap.json`, an object whose keys are keystrokes in
//! GPUI's spelling and whose values are command ids, or `null` to unbind:
//!
//! ```json
//! {
//!   "cmd-k": "peek-definition",
//!   "cmd-y": null
//! }
//! ```
//!
//! `cmd` matches either ⌘ or Ctrl (the editor has always treated them the
//! same); `ctrl` on its own means Ctrl only. Command ids are shared with the
//! command palette, so any palette command can be bound too. Plain typing,
//! arrows, Enter, Backspace and Tab are not part of the keymap.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use gpui_kit::{Global, Keystroke};

/// `(keystroke, command id)`: the shortcuts the app ships with.
pub const DEFAULT_BINDINGS: &[(&str, &str)] = &[
    // Editor
    ("cmd-c", "copy"),
    ("cmd-x", "cut"),
    ("cmd-v", "paste"),
    ("cmd-a", "select-all"),
    ("cmd-s", "save"),
    ("cmd-z", "undo"),
    ("cmd-shift-z", "redo"),
    ("cmd-/", "toggle-comment"),
    ("cmd-shift-k", "delete-line"),
    ("cmd-d", "select-next-occurrence"),
    ("cmd-alt-up", "add-cursor-above"),
    ("cmd-alt-down", "add-cursor-below"),
    ("ctrl-space", "trigger-completion"),
    ("cmd-.", "code-actions"),
    ("cmd-shift-r", "refactor"),
    ("cmd-r", "rename"),
    ("f2", "rename"),
    ("cmd-y", "peek-definition"),
    ("alt-f12", "peek-definition"),
    ("cmd-shift-y", "find-references"),
    ("shift-f12", "find-references"),
    ("alt-shift-f", "format-doc"),
    // Files and navigation
    ("cmd-o", "open-folder"),
    ("cmd-n", "new-file"),
    ("cmd-w", "close-tab"),
    ("cmd-alt-s", "save-all"),
    ("cmd-p", "go-to-file"),
    ("cmd-shift-p", "command-palette"),
    ("cmd-t", "workspace-symbols"),
    ("cmd-shift-o", "symbol-outline"),
    ("cmd-g", "go-to-line"),
    ("cmd-shift-l", "go-to-line"),
    ("cmd-f", "find-in-file"),
    ("cmd-shift-f", "project-search"),
    ("cmd-l", "send-to-agent"),
    ("cmd-,", "open-settings"),
    // Panels
    ("cmd-b", "toggle-sidebar"),
    ("cmd-j", "toggle-terminal"),
    ("cmd-shift-m", "toggle-problems"),
    ("cmd-shift-g", "open-git"),
    ("cmd-shift-v", "markdown-cycle-view"),
    ("cmd-shift-d", "flutter-panel"),
    ("cmd-shift-t", "go-to-test"),
    // Reviewing a diff
    ("alt-down", "diff-next-change"),
    ("alt-up", "diff-prev-change"),
    ("alt-right", "diff-next-file"),
    ("alt-left", "diff-prev-file"),
    ("alt-enter", "diff-toggle-reviewed"),
];

/// A modifier-and-key combination, normalised so `cmd` means "⌘ or Ctrl".
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Chord {
    primary: bool,
    ctrl_only: bool,
    alt: bool,
    shift: bool,
    key: String,
}

impl Chord {
    fn parse(spec: &str) -> Result<Self, String> {
        let stroke = Keystroke::parse(spec.trim()).map_err(|_| format!("`{spec}` isn't a keystroke"))?;
        if stroke.key.is_empty() {
            return Err(format!("`{spec}` has no key"));
        }
        let m = stroke.modifiers;
        Ok(Self {
            primary: m.platform,
            ctrl_only: m.control && !m.platform,
            alt: m.alt,
            shift: m.shift,
            key: stroke.key.to_lowercase(),
        })
    }

    /// The chords an actual key press can match: as typed, and, when Ctrl
    /// is held without ⌘, as a `cmd` binding (so `cmd-b` also fires on Ctrl-B).
    fn candidates(stroke: &Keystroke) -> Vec<Self> {
        let m = &stroke.modifiers;
        let key = stroke.key.to_lowercase();
        let exact = Self {
            primary: m.platform,
            ctrl_only: m.control && !m.platform,
            alt: m.alt,
            shift: m.shift,
            key: key.clone(),
        };
        let mut out = vec![exact];
        if m.control && !m.platform {
            out.push(Self { primary: true, ctrl_only: false, alt: m.alt, shift: m.shift, key });
        }
        out
    }

    /// `⌥⇧⌘F`-style label for menus and the palette.
    fn label(&self) -> String {
        let mut s = String::new();
        if self.ctrl_only {
            s.push('\u{2303}');
        }
        if self.alt {
            s.push('\u{2325}');
        }
        if self.shift {
            s.push('\u{21e7}');
        }
        if self.primary {
            s.push('\u{2318}');
        }
        let key = match self.key.as_str() {
            "up" => "\u{2191}".to_string(),
            "down" => "\u{2193}".to_string(),
            "left" => "\u{2190}".to_string(),
            "right" => "\u{2192}".to_string(),
            "enter" => "\u{21a9}".to_string(),
            "space" => "Space".to_string(),
            "escape" => "Esc".to_string(),
            k => k.to_uppercase(),
        };
        s.push_str(&key);
        s
    }
}

/// The active keymap: defaults with the user's overrides applied.
#[derive(Debug, Clone)]
pub struct Keymap {
    bindings: BTreeMap<Chord, String>,
    /// GPUI-spelled keystroke for each binding, for building menu shortcuts.
    specs: BTreeMap<Chord, String>,
    /// Problems found in the user's file, shown to them; bad entries are skipped.
    pub errors: Vec<String>,
}

impl Global for Keymap {}

impl Default for Keymap {
    fn default() -> Self {
        let mut keymap = Self { bindings: BTreeMap::new(), specs: BTreeMap::new(), errors: Vec::new() };
        for (spec, command) in DEFAULT_BINDINGS {
            let chord = Chord::parse(spec).expect("default bindings are valid keystrokes");
            keymap.specs.insert(chord.clone(), (*spec).to_string());
            keymap.bindings.insert(chord, (*command).to_string());
        }
        keymap
    }
}

impl Keymap {
    /// Defaults plus the overrides in `json` (the contents of the user's
    /// keymap file). Invalid entries are reported in `errors` and skipped,
    /// so one typo never costs the rest of the shortcuts.
    pub fn with_overrides(json: &str) -> Self {
        let mut keymap = Self::default();
        let value: serde_json::Value = match serde_json::from_str(json) {
            Ok(v) => v,
            Err(e) => {
                keymap.errors.push(format!("keymap.json isn't valid JSON: {e}"));
                return keymap;
            }
        };
        let Some(entries) = value.as_object() else {
            keymap.errors.push("keymap.json should be an object of \"keystroke\": \"command\"".into());
            return keymap;
        };
        for (spec, command) in entries {
            let chord = match Chord::parse(spec) {
                Ok(c) => c,
                Err(e) => {
                    keymap.errors.push(e);
                    continue;
                }
            };
            match command {
                serde_json::Value::Null => {
                    keymap.bindings.remove(&chord);
                    keymap.specs.remove(&chord);
                }
                serde_json::Value::String(id) if is_known_command(id) => {
                    keymap.specs.insert(chord.clone(), spec.clone());
                    keymap.bindings.insert(chord, id.clone());
                }
                serde_json::Value::String(id) => keymap.errors.push(format!("`{spec}`: unknown command `{id}`")),
                _ => keymap.errors.push(format!("`{spec}`: expected a command id or null")),
            }
        }
        keymap
    }

    /// Load `path`, or the defaults when it doesn't exist.
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(json) => Self::with_overrides(&json),
            Err(_) => Self::default(),
        }
    }

    /// `~/.config/shame/keymap.json`.
    pub fn user_path() -> Option<PathBuf> {
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config/shame/keymap.json"))
    }

    /// The command bound to this key press, if any.
    pub fn command_for(&self, stroke: &Keystroke) -> Option<&str> {
        Chord::candidates(stroke)
            .iter()
            .find_map(|chord| self.bindings.get(chord))
            .map(String::as_str)
    }

    /// The label (`⇧⌘F`) of the first key bound to `command`, for display.
    pub fn label_for(&self, command: &str) -> Option<String> {
        self.bindings.iter().find(|(_, c)| c.as_str() == command).map(|(chord, _)| chord.label())
    }

    /// The GPUI keystroke spelling of every key bound to `command`.
    pub fn keystrokes_for(&self, command: &str) -> Vec<String> {
        self.bindings
            .iter()
            .filter(|(_, c)| c.as_str() == command)
            .filter_map(|(chord, _)| self.specs.get(chord).cloned())
            .collect()
    }
}

/// Commands the focused editor runs itself; the app leaves these alone so
/// they never fire twice.
pub const EDITOR_COMMANDS: &[&str] = &[
    "copy",
    "cut",
    "paste",
    "select-all",
    "save",
    "undo",
    "redo",
    "toggle-comment",
    "move-line-up",
    "move-line-down",
    "copy-line-up",
    "copy-line-down",
    "delete-line",
    "go-to-implementation",
    "go-to-type-definition",
    "select-next-occurrence",
    "add-cursor-above",
    "add-cursor-below",
    "trigger-completion",
    "code-actions",
    "refactor",
    "rename",
    "peek-definition",
    "find-references",
    "format-doc",
];

/// The command bound to `stroke` in the app's keymap (or the defaults
/// before one is installed).
pub fn resolve(cx: &gpui_kit::App, stroke: &Keystroke) -> Option<String> {
    static DEFAULT: std::sync::OnceLock<Keymap> = std::sync::OnceLock::new();
    let keymap = cx.try_global::<Keymap>().unwrap_or_else(|| DEFAULT.get_or_init(Keymap::default));
    keymap.command_for(stroke).map(str::to_string)
}

/// Commands the app or editor handles from the keyboard, beyond the
/// palette's own ids (which the app registers with [`register_commands`]).
fn is_known_command(id: &str) -> bool {
    DEFAULT_BINDINGS.iter().any(|(_, c)| *c == id)
        || EXTRA_COMMANDS.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|c| c == id)
}

static EXTRA_COMMANDS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Make more command ids bindable (the command palette's), so a keymap
/// entry like `"cmd-shift-h": "hot-reload"` is accepted.
pub fn register_commands<'a>(ids: impl IntoIterator<Item = &'a str>) {
    let mut extra = EXTRA_COMMANDS.lock().unwrap_or_else(|e| e.into_inner());
    for id in ids {
        if !extra.iter().any(|c| c == id) {
            extra.push(id.to_string());
        }
    }
}

/// Starter contents for a new keymap file: every default binding, so users
/// can see what to change.
pub fn template() -> String {
    let rows: Vec<String> = DEFAULT_BINDINGS
        .iter()
        .map(|(spec, command)| format!("  \"{spec}\": \"{command}\""))
        .collect();
    format!("{{\n{}\n}}\n", rows.join(",\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(spec: &str) -> Keystroke {
        Keystroke::parse(spec).unwrap()
    }

    #[test]
    fn defaults_resolve_and_cmd_also_matches_ctrl() {
        let keymap = Keymap::default();
        assert_eq!(keymap.command_for(&press("cmd-b")), Some("toggle-sidebar"));
        assert_eq!(keymap.command_for(&press("ctrl-b")), Some("toggle-sidebar"));
        assert_eq!(keymap.command_for(&press("cmd-shift-f")), Some("project-search"));
        assert_eq!(keymap.command_for(&press("alt-shift-f")), Some("format-doc"));
        assert_eq!(keymap.command_for(&press("b")), None);
    }

    #[test]
    fn ctrl_only_bindings_do_not_fire_on_cmd() {
        let keymap = Keymap::default();
        assert_eq!(keymap.command_for(&press("ctrl-space")), Some("trigger-completion"));
        assert_eq!(keymap.command_for(&press("cmd-space")), None);
    }

    #[test]
    fn overrides_rebind_add_and_unbind() {
        let keymap =
            Keymap::with_overrides(r#"{ "cmd-k": "peek-definition", "cmd-y": null, "cmd-b": "toggle-terminal" }"#);
        assert!(keymap.errors.is_empty(), "{:?}", keymap.errors);
        assert_eq!(keymap.command_for(&press("cmd-k")), Some("peek-definition"));
        assert_eq!(keymap.command_for(&press("cmd-y")), None);
        assert_eq!(keymap.command_for(&press("cmd-b")), Some("toggle-terminal"));
        // Untouched defaults stay.
        assert_eq!(keymap.command_for(&press("cmd-j")), Some("toggle-terminal"));
    }

    #[test]
    fn bad_entries_are_reported_and_skipped() {
        let keymap = Keymap::with_overrides(r#"{ "cmd-k": "no-such-command", "cmd-u": 3, "cmd-e": "rename" }"#);
        assert_eq!(keymap.errors.len(), 2, "{:?}", keymap.errors);
        assert_eq!(keymap.command_for(&press("cmd-k")), None);
        assert_eq!(keymap.command_for(&press("cmd-e")), Some("rename"));
    }

    #[test]
    fn invalid_json_keeps_the_defaults() {
        let keymap = Keymap::with_overrides("{ not json");
        assert_eq!(keymap.errors.len(), 1);
        assert_eq!(keymap.command_for(&press("cmd-b")), Some("toggle-sidebar"));
    }

    #[test]
    fn registered_palette_commands_become_bindable() {
        register_commands(["hot-reload"]);
        let keymap = Keymap::with_overrides(r#"{ "cmd-shift-h": "hot-reload" }"#);
        assert!(keymap.errors.is_empty(), "{:?}", keymap.errors);
        assert_eq!(keymap.command_for(&press("cmd-shift-h")), Some("hot-reload"));
    }

    #[test]
    fn labels_use_mac_symbols_in_standard_order() {
        let keymap = Keymap::default();
        assert_eq!(keymap.label_for("format-doc").as_deref(), Some("\u{2325}\u{21e7}F"));
        assert_eq!(keymap.label_for("save-all").as_deref(), Some("\u{2325}\u{2318}S"));
        assert_eq!(keymap.keystrokes_for("save"), vec!["cmd-s".to_string()]);
    }

    #[test]
    fn template_parses_back_to_the_defaults() {
        let keymap = Keymap::with_overrides(&template());
        assert!(keymap.errors.is_empty(), "{:?}", keymap.errors);
        assert_eq!(keymap.command_for(&press("cmd-.")), Some("code-actions"));
    }
}
