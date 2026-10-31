//! The running app's widget tree, as the Flutter inspector reports it
//! (`ext.flutter.inspector.getRootWidgetSummaryTree`): only the widgets your
//! project created, nested the way they are in the build. Each node knows where
//! in the source it was written, so a click can jump there.

use std::collections::HashSet;
use std::path::PathBuf;

use serde_json::Value;

use crate::inspector::{WidgetLocation, file_uri_to_path};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WidgetNode {
    /// The widget, e.g. `Scaffold` or `Text`.
    pub name: String,
    /// The inspector's handle for this widget (used to select it in the app).
    pub value_id: Option<String>,
    pub location: Option<WidgetLocation>,
    pub stateful: bool,
    pub children: Vec<WidgetNode>,
}

/// One visible row of the tree, flattened for display.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeRow {
    /// Index path from the root (`"0.2.1"`): stable while the tree does not change.
    pub key: String,
    pub depth: usize,
    pub name: String,
    pub has_children: bool,
    pub expanded: bool,
    pub stateful: bool,
    pub location: Option<WidgetLocation>,
    pub value_id: Option<String>,
}

/// Parse the daemon's reply to `getRootWidgetSummaryTree`
/// (`{"result": <node>, ...}`) into a tree. `None` if there is no tree.
pub fn parse_widget_tree(response: &Value) -> Option<WidgetNode> {
    parse_node(response.get("result")?)
}

fn parse_node(node: &Value) -> Option<WidgetNode> {
    let name = node
        .get("widgetRuntimeType")
        .or_else(|| node.get("description"))
        .and_then(Value::as_str)?
        .to_string();
    let location = node.get("creationLocation").and_then(|loc| {
        Some(WidgetLocation {
            name: loc.get("name").and_then(Value::as_str).unwrap_or(&name).to_string(),
            path: file_uri_to_path(loc.get("file")?.as_str()?)?,
            line: loc.get("line")?.as_u64()? as u32,
            column: loc.get("column").and_then(Value::as_u64).unwrap_or(1) as u32,
            local: node.get("createdByLocalProject").and_then(Value::as_bool).unwrap_or(false),
        })
    });
    let children = node
        .get("children")
        .and_then(Value::as_array)
        .map(|kids| kids.iter().filter_map(parse_node).collect())
        .unwrap_or_default();
    Some(WidgetNode {
        name,
        value_id: node.get("valueId").and_then(Value::as_str).map(str::to_string),
        location,
        stateful: node.get("stateful").and_then(Value::as_bool).unwrap_or(false),
        children,
    })
}

/// The keys of every node that has children and is at most `depth` levels
/// down: the starting point for a freshly loaded tree.
pub fn default_expanded(root: &WidgetNode, depth: usize) -> HashSet<String> {
    fn walk(node: &WidgetNode, key: String, level: usize, depth: usize, out: &mut HashSet<String>) {
        if level >= depth || node.children.is_empty() {
            return;
        }
        for (ix, child) in node.children.iter().enumerate() {
            walk(child, format!("{key}.{ix}"), level + 1, depth, out);
        }
        out.insert(key);
    }
    let mut out = HashSet::new();
    walk(root, "0".to_string(), 0, depth, &mut out);
    out
}

/// The rows to show, given which nodes are expanded (by key).
pub fn flatten(root: &WidgetNode, expanded: &HashSet<String>) -> Vec<TreeRow> {
    fn walk(node: &WidgetNode, key: String, depth: usize, expanded: &HashSet<String>, out: &mut Vec<TreeRow>) {
        let is_open = expanded.contains(&key);
        out.push(TreeRow {
            key: key.clone(),
            depth,
            name: node.name.clone(),
            has_children: !node.children.is_empty(),
            expanded: is_open,
            stateful: node.stateful,
            location: node.location.clone(),
            value_id: node.value_id.clone(),
        });
        if is_open {
            for (ix, child) in node.children.iter().enumerate() {
                walk(child, format!("{key}.{ix}"), depth + 1, expanded, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, "0".to_string(), 0, expanded, &mut out);
    out
}

/// The key of the deepest node written at `path:line:column`, with the keys of
/// all its ancestors (so they can be expanded to reveal it).
pub fn find_by_location(root: &WidgetNode, path: &PathBuf, line: u32, column: u32) -> Option<Vec<String>> {
    fn walk(node: &WidgetNode, key: String, target: (&PathBuf, u32, u32), trail: &mut Vec<String>) -> bool {
        trail.push(key.clone());
        for (ix, child) in node.children.iter().enumerate() {
            if walk(child, format!("{key}.{ix}"), target, trail) {
                return true;
            }
        }
        let here = node
            .location
            .as_ref()
            .is_some_and(|l| &l.path == target.0 && l.line == target.1 && l.column == target.2);
        if here {
            return true;
        }
        trail.pop();
        false
    }
    let mut trail = Vec::new();
    walk(root, "0".to_string(), (path, line, column), &mut trail).then_some(trail)
}

/// The node with this key, if it still exists.
pub fn node_at<'a>(root: &'a WidgetNode, key: &str) -> Option<&'a WidgetNode> {
    let mut parts = key.split('.');
    if parts.next()? != "0" {
        return None;
    }
    let mut node = root;
    for part in parts {
        node = node.children.get(part.parse::<usize>().ok()?)?;
    }
    Some(node)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape of the daemon's real reply (the same node shape as a selected
    /// widget, nested under `children`).
    fn reply() -> Value {
        let node = |name: &str, line: u32, children: Vec<Value>, stateful: bool| {
            serde_json::json!({
                "description": name, "widgetRuntimeType": name, "valueId": format!("inspector-{name}"),
                "stateful": stateful, "hasChildren": !children.is_empty(), "children": children,
                "createdByLocalProject": true,
                "creationLocation": {"file": "file:///app/lib/main.dart", "line": line, "column": 12, "name": name}
            })
        };
        serde_json::json!({
            "result": node("MyApp", 5, vec![
                node("MaterialApp", 10, vec![
                    node("Scaffold", 14, vec![
                        node("AppBar", 15, vec![], false),
                        node("Counter", 20, vec![node("Text", 31, vec![], false)], true),
                    ], false),
                ], false),
            ], false),
            "type": "_extensionType",
            "method": "ext.flutter.inspector.getRootWidgetSummaryTree"
        })
    }

    #[test]
    fn parses_nested_widgets_with_their_source_locations() {
        let tree = parse_widget_tree(&reply()).unwrap();
        assert_eq!(tree.name, "MyApp");
        let scaffold = &tree.children[0].children[0];
        assert_eq!(scaffold.name, "Scaffold");
        assert_eq!(scaffold.children.len(), 2);
        let counter = &scaffold.children[1];
        assert!(counter.stateful);
        let loc = counter.location.as_ref().unwrap();
        assert_eq!((loc.path.to_str().unwrap(), loc.line, loc.column, loc.local), ("/app/lib/main.dart", 20, 12, true));
        assert_eq!(counter.value_id.as_deref(), Some("inspector-Counter"));
    }

    #[test]
    fn no_tree_means_none() {
        assert_eq!(parse_widget_tree(&serde_json::json!({"result": null})), None);
        assert_eq!(parse_widget_tree(&serde_json::json!({"type": "x"})), None);
    }

    #[test]
    fn flattening_shows_only_expanded_branches() {
        let tree = parse_widget_tree(&reply()).unwrap();
        let rows = flatten(&tree, &HashSet::new());
        assert_eq!(rows.len(), 1, "collapsed: just the root");
        assert!(rows[0].has_children && !rows[0].expanded);

        let open: HashSet<String> = ["0", "0.0"].into_iter().map(String::from).collect();
        let rows = flatten(&tree, &open);
        let shape: Vec<(&str, usize)> = rows.iter().map(|r| (r.name.as_str(), r.depth)).collect();
        assert_eq!(shape, vec![("MyApp", 0), ("MaterialApp", 1), ("Scaffold", 2)]);

        let all = default_expanded(&tree, 10);
        let rows = flatten(&tree, &all);
        assert_eq!(rows.len(), 6);
        assert_eq!(rows[5].key, "0.0.0.1.0", "Text: root > MaterialApp > Scaffold > Counter > Text");
    }

    #[test]
    fn the_default_expansion_is_limited_by_depth() {
        let tree = parse_widget_tree(&reply()).unwrap();
        let two = default_expanded(&tree, 2);
        assert!(two.contains("0") && two.contains("0.0"));
        assert!(!two.contains("0.0.0"), "deeper levels start collapsed");
    }

    #[test]
    fn a_tapped_widget_is_found_with_its_ancestors() {
        let tree = parse_widget_tree(&reply()).unwrap();
        let found = find_by_location(&tree, &PathBuf::from("/app/lib/main.dart"), 31, 12).unwrap();
        assert_eq!(found, vec!["0", "0.0", "0.0.0", "0.0.0.1", "0.0.0.1.0"]);
        assert_eq!(find_by_location(&tree, &PathBuf::from("/app/lib/main.dart"), 99, 1), None);
        assert_eq!(find_by_location(&tree, &PathBuf::from("/other.dart"), 31, 12), None);
    }

    #[test]
    fn nodes_are_looked_up_by_key() {
        let tree = parse_widget_tree(&reply()).unwrap();
        assert_eq!(node_at(&tree, "0.0.0.1").unwrap().name, "Counter");
        assert_eq!(node_at(&tree, "0").unwrap().name, "MyApp");
        assert!(node_at(&tree, "0.5").is_none());
        assert!(node_at(&tree, "garbage").is_none());
    }
}
