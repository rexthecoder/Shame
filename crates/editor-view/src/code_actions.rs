//! Code actions (the lightbulb): grouping, ordering and the cached set for
//! the cursor.
//!
//! Actions are prefetched shortly after the cursor settles, so the gutter
//! lightbulb only appears when something is actually on offer and `⌘.`
//! opens the menu without waiting on the server.

use core_lsp::lsp_types::{CodeActionOrCommand, CodeActionKind};

/// Menu section an action belongs to, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ActionGroup {
    QuickFix,
    Refactor,
    Source,
    Other,
}

impl ActionGroup {
    pub fn label(self) -> &'static str {
        match self {
            Self::QuickFix => "Quick Fix",
            Self::Refactor => "Refactor",
            Self::Source => "Source",
            Self::Other => "More Actions",
        }
    }
}

/// One menu row.
#[derive(Debug, Clone)]
pub(crate) struct ActionItem {
    pub title: String,
    pub group: ActionGroup,
    pub preferred: bool,
    pub action: CodeActionOrCommand,
}

/// Actions fetched for one cursor position of one text revision.
#[derive(Debug, Clone)]
pub(crate) struct CodeActionSet {
    pub revision: u64,
    pub head: usize,
    pub line: usize,
    pub items: Vec<ActionItem>,
}

impl CodeActionSet {
    pub fn has_quick_fix(&self) -> bool {
        self.items.iter().any(|i| i.group == ActionGroup::QuickFix)
    }
}

/// The open menu: its own copy of the items, so a later prefetch for the
/// same spot can't reshuffle rows under the user's selection.
pub(crate) struct CodeActionMenu {
    pub items: Vec<ActionItem>,
    pub selected: usize,
}

fn group_of(action: &CodeActionOrCommand) -> ActionGroup {
    let CodeActionOrCommand::CodeAction(a) = action else {
        return ActionGroup::Other;
    };
    let kind = a.kind.as_ref().map(CodeActionKind::as_str).unwrap_or("");
    let is = |prefix: &str| kind == prefix || kind.starts_with(&format!("{prefix}."));
    if is("quickfix") {
        ActionGroup::QuickFix
    } else if is("refactor") {
        ActionGroup::Refactor
    } else if is("source") {
        ActionGroup::Source
    } else if a.diagnostics.as_ref().is_some_and(|d| !d.is_empty()) {
        // Untyped, but tied to a diagnostic: that's a fix.
        ActionGroup::QuickFix
    } else {
        ActionGroup::Other
    }
}

/// Drop disabled and duplicate-titled actions, then order them by group
/// (fixes first), preferred fixes first within a group, otherwise keeping
/// the server's own relevance order.
pub(crate) fn organize(actions: Vec<CodeActionOrCommand>) -> Vec<ActionItem> {
    let mut seen = std::collections::HashSet::new();
    let mut items: Vec<ActionItem> = actions
        .into_iter()
        .filter(|a| !matches!(a, CodeActionOrCommand::CodeAction(ca) if ca.disabled.is_some()))
        .filter_map(|action| {
            let (title, preferred) = match &action {
                CodeActionOrCommand::Command(c) => (c.title.clone(), false),
                CodeActionOrCommand::CodeAction(a) => (a.title.clone(), a.is_preferred.unwrap_or(false)),
            };
            if !seen.insert(title.to_lowercase()) {
                return None;
            }
            Some(ActionItem { title, group: group_of(&action), preferred, action })
        })
        .collect();
    items.sort_by_key(|i| (i.group, !i.preferred));
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_lsp::lsp_types::{CodeAction, CodeActionDisabled, Command};

    fn action(title: &str, kind: Option<&str>) -> CodeActionOrCommand {
        CodeActionOrCommand::CodeAction(CodeAction {
            title: title.into(),
            kind: kind.map(|k| CodeActionKind::from(k.to_string())),
            ..Default::default()
        })
    }

    fn titles(items: &[ActionItem]) -> Vec<&str> {
        items.iter().map(|i| i.title.as_str()).collect()
    }

    #[test]
    fn fixes_come_before_refactors_and_source_actions() {
        let items = organize(vec![
            action("Wrap with Padding", Some("refactor.flutter.wrap.padding")),
            action("Organize imports", Some("source.organizeImports")),
            action("Add missing import", Some("quickfix.import")),
        ]);
        assert_eq!(titles(&items), vec!["Add missing import", "Wrap with Padding", "Organize imports"]);
        assert_eq!(items[0].group, ActionGroup::QuickFix);
    }

    #[test]
    fn preferred_fix_leads_its_group_and_server_order_is_kept_otherwise() {
        let mut preferred = action("Create class", Some("quickfix"));
        if let CodeActionOrCommand::CodeAction(a) = &mut preferred {
            a.is_preferred = Some(true);
        }
        let items = organize(vec![
            action("Create method", Some("quickfix")),
            action("Create getter", Some("quickfix")),
            preferred,
        ]);
        assert_eq!(titles(&items), vec!["Create class", "Create method", "Create getter"]);
    }

    #[test]
    fn disabled_and_duplicate_actions_are_dropped() {
        let mut disabled = action("Extract widget", Some("refactor.extract"));
        if let CodeActionOrCommand::CodeAction(a) = &mut disabled {
            a.disabled = Some(CodeActionDisabled { reason: "no selection".into() });
        }
        let items = organize(vec![
            disabled,
            action("Wrap with Center", Some("refactor")),
            action("wrap with center", Some("refactor")),
        ]);
        assert_eq!(titles(&items), vec!["Wrap with Center"]);
    }

    #[test]
    fn bare_commands_go_last() {
        let command = CodeActionOrCommand::Command(Command {
            title: "Sort members".into(),
            command: "dart.sortMembers".into(),
            arguments: None,
        });
        let items = organize(vec![command, action("Convert to block body", Some("refactor.rewrite"))]);
        assert_eq!(titles(&items), vec!["Convert to block body", "Sort members"]);
        assert_eq!(items[1].group, ActionGroup::Other);
    }
}
