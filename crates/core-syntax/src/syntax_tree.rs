use std::ops::Range;
use std::sync::OnceLock;

use tree_sitter::{
    InputEdit, Language, Node, Parser, Point, Query, QueryCursor, StreamingIterator, Tree,
};

use crate::highlight::{Highlight, HighlightKind};

/// A grammar `core-syntax` can parse. Everything else falls back to the
/// editor's lexical highlighter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SyntaxLanguage {
    Dart,
    Python,
    Json,
    Yaml,
    Toml,
    Markdown,
}

impl SyntaxLanguage {
    /// Map an LSP language id (`"dart"`, `"python"`, `"json"`, `"yaml"`,
    /// `"toml"`, `"markdown"`) to a grammar.
    pub fn from_language_id(id: &str) -> Option<Self> {
        match id {
            "dart" => Some(Self::Dart),
            "python" => Some(Self::Python),
            "json" | "jsonc" => Some(Self::Json),
            "yaml" | "yml" => Some(Self::Yaml),
            "toml" => Some(Self::Toml),
            "markdown" | "md" => Some(Self::Markdown),
            _ => None,
        }
    }

    fn language(self) -> Language {
        match self {
            Self::Dart => tree_sitter_dart::LANGUAGE.into(),
            Self::Python => tree_sitter_python::LANGUAGE.into(),
            Self::Json => tree_sitter_json::LANGUAGE.into(),
            Self::Yaml => tree_sitter_yaml::LANGUAGE.into(),
            Self::Toml => tree_sitter_toml_ng::LANGUAGE.into(),
            Self::Markdown => tree_sitter_md::LANGUAGE.into(),
        }
    }

    fn query(self) -> &'static Query {
        static DART: OnceLock<Query> = OnceLock::new();
        static PYTHON: OnceLock<Query> = OnceLock::new();
        static JSON: OnceLock<Query> = OnceLock::new();
        static YAML: OnceLock<Query> = OnceLock::new();
        static TOML: OnceLock<Query> = OnceLock::new();
        static MARKDOWN: OnceLock<Query> = OnceLock::new();
        match self {
            Self::Dart => DART.get_or_init(|| {
                Query::new(&self.language(), tree_sitter_dart::HIGHLIGHTS_QUERY)
                    .expect("tree-sitter-dart ships a highlights query valid for its own grammar")
            }),
            Self::Python => PYTHON.get_or_init(|| {
                let source = format!(
                    "{}\n{}",
                    tree_sitter_python::HIGHLIGHTS_QUERY,
                    include_str!("../queries/python-extra.scm")
                );
                Query::new(&self.language(), &source)
                    .expect("the Python highlights query is valid for tree-sitter-python")
            }),
            Self::Json => JSON.get_or_init(|| {
                Query::new(&self.language(), tree_sitter_json::HIGHLIGHTS_QUERY)
                    .expect("tree-sitter-json ships a highlights query valid for its own grammar")
            }),
            Self::Yaml => YAML.get_or_init(|| {
                Query::new(&self.language(), tree_sitter_yaml::HIGHLIGHTS_QUERY)
                    .expect("tree-sitter-yaml ships a highlights query valid for its own grammar")
            }),
            Self::Toml => TOML.get_or_init(|| {
                Query::new(&self.language(), tree_sitter_toml_ng::HIGHLIGHTS_QUERY)
                    .expect("tree-sitter-toml-ng ships a highlights query valid for its own grammar")
            }),
            Self::Markdown => MARKDOWN.get_or_init(|| {
                Query::new(&self.language(), tree_sitter_md::HIGHLIGHT_QUERY_BLOCK)
                    .expect("tree-sitter-md ships a block highlights query valid for its own grammar")
            }),
        }
    }

    fn parser(self) -> Parser {
        let mut parser = Parser::new();
        parser
            .set_language(&self.language())
            .expect("bundled grammars are compatible with this tree-sitter version");
        parser
    }
}

/// A fold candidate: a `{ ... }` block spanning more than one line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FoldRange {
    pub start_line: usize,
    pub end_line: usize,
    pub start_byte: usize,
    pub end_byte: usize,
}

/// A parsed syntax tree, re-parsed incrementally as the source edits.
pub struct SyntaxTree {
    language: SyntaxLanguage,
    parser: Parser,
    tree: Tree,
}

impl SyntaxTree {
    /// Parse `source` as Dart.
    pub fn parse(source: &str) -> Self {
        Self::parse_as(SyntaxLanguage::Dart, source)
    }

    pub fn parse_as(language: SyntaxLanguage, source: &str) -> Self {
        let mut parser = language.parser();
        let tree = parser
            .parse(source, None)
            .expect("tree-sitter only returns None if parsing was cancelled or timed out");
        let _ = language.query();
        Self {
            language,
            parser,
            tree,
        }
    }

    pub fn language(&self) -> SyntaxLanguage {
        self.language
    }

    /// Apply a text edit and re-parse, reusing the previous tree and parser so
    /// tree-sitter only re-derives the parts of the syntax tree that
    /// actually changed.
    pub fn reparse(&mut self, new_source: &str, edit: InputEdit) {
        self.tree.edit(&edit);
        if let Some(tree) = self.parser.parse(new_source, Some(&self.tree)) {
            self.tree = tree;
        }
    }

    pub fn root(&self) -> Node<'_> {
        self.tree.root_node()
    }

    /// Run the grammar's highlights query against `source` (which must be
    /// the same text this tree was parsed/reparsed from).
    pub fn highlights(&self, source: &str) -> Vec<Highlight> {
        self.highlights_in_range(source, 0..source.len())
    }

    /// Run the grammar's highlights query restricted to `byte_range` so
    /// virtualized editor rendering only queries the visible viewport lines.
    pub fn highlights_in_range(&self, source: &str, byte_range: Range<usize>) -> Vec<Highlight> {
        let language = self.language;
        let query = language.query();
        let capture_names = query.capture_names();
        let mut cursor = QueryCursor::new();
        cursor.set_byte_range(byte_range);
        let mut matches = cursor.matches(query, self.tree.root_node(), source.as_bytes());

        let mut highlights = Vec::new();
        // Pattern index per query capture, for Python's "later pattern wins".
        let mut patterns: Vec<usize> = Vec::new();
        while let Some(m) = matches.next() {
            for capture in m.captures() {
                patterns.push(m.pattern_index);
                let name = capture_names[capture.index as usize];
                let range = capture.node.start_byte()..capture.node.end_byte();
                let mut kind = HighlightKind::from_capture_name(name);
                // Python's query tags PascalCase identifiers `constructor`:
                // they are classes.
                if language == SyntaxLanguage::Python && name == "constructor" {
                    kind = HighlightKind::Type;
                }
                // In Dart, PascalCase function calls (`Divider(...)`, `StatelessWidget(...)`)
                // are widget/class constructor invocations and should highlight as `Type`.
                if language == SyntaxLanguage::Dart
                    && kind == HighlightKind::Function
                    && name == "function.call"
                {
                    if source[range.clone()]
                        .bytes()
                        .next()
                        .is_some_and(|b| b.is_ascii_uppercase())
                    {
                        kind = HighlightKind::Type;
                    }
                }
                highlights.push(Highlight {
                    range: range.clone(),
                    kind,
                });
                // Highlight `[identifier]` references inside `///` doc comments.
                if language == SyntaxLanguage::Dart && kind == HighlightKind::Comment {
                    let slice = &source[range.clone()];
                    if slice.trim_start().starts_with("///") {
                        let bytes = slice.as_bytes();
                        let mut i = 0;
                        while i < bytes.len() {
                            if bytes[i] == b'[' {
                                if let Some(close) = slice[i + 1..].find(']') {
                                    let end = i + 1 + close;
                                    if end > i + 1 {
                                        highlights.push(Highlight {
                                            range: (range.start + i)..(range.start + end + 1),
                                            kind: HighlightKind::Attribute,
                                        });
                                    }
                                    i = end + 1;
                                    continue;
                                }
                            }
                            i += 1;
                        }
                    }
                }
            }
        }
        if language == SyntaxLanguage::Python {
            // The editor paints spans in order, later ones on top: outer
            // spans first, and for the same span the most specific (last)
            // query pattern last. Python adds no extra spans, so `patterns`
            // lines up with `highlights`.
            let mut tagged: Vec<(usize, Highlight)> = patterns.into_iter().zip(highlights).collect();
            tagged.sort_by_key(|(p, h)| (h.range.start, std::cmp::Reverse(h.range.end), *p));
            return tagged.into_iter().map(|(_, h)| h).collect();
        }
        highlights.sort_by_key(|h| (h.range.start, h.kind != HighlightKind::Comment));
        highlights
    }

    /// Find the byte offset of the bracket matching the one at `offset`,
    /// skipping bracket characters that fall inside a comment or string
    /// (per the highlights query) so e.g. a `)` in a doc comment doesn't
    /// throw off matching in the real code around it.
    pub fn matching_bracket(&self, source: &str, offset: usize) -> Option<usize> {
        const PAIRS: &[(u8, u8)] = &[(b'(', b')'), (b'[', b']'), (b'{', b'}')];
        const WINDOW: usize = 24 * 1024;

        let bytes = source.as_bytes();
        let at = *bytes.get(offset)?;
        let is_open = PAIRS.iter().find(|&&(open, _)| open == at).copied();
        let is_close = PAIRS.iter().find(|&&(_, close)| close == at).copied();
        if is_open.is_none() && is_close.is_none() {
            return None;
        }

        let scan_range = if is_open.is_some() {
            offset..(offset + WINDOW).min(bytes.len())
        } else {
            offset.saturating_sub(WINDOW)..(offset + 1).min(bytes.len())
        };

        let mut skip: Vec<Range<usize>> = self
            .highlights_in_range(source, scan_range.clone())
            .into_iter()
            .filter(|h| matches!(h.kind, HighlightKind::Comment | HighlightKind::String))
            .map(|h| h.range)
            .collect();
        skip.sort_by_key(|r| r.start);
        let in_skip = |pos: usize| {
            let idx = skip.partition_point(|r| r.end <= pos);
            skip.get(idx).is_some_and(|r| r.start <= pos && pos < r.end)
        };

        if let Some((open, close)) = is_open {
            let mut depth = 1usize;
            for pos in (offset + 1)..scan_range.end {
                if in_skip(pos) {
                    continue;
                }
                if bytes[pos] == open {
                    depth += 1;
                } else if bytes[pos] == close {
                    depth -= 1;
                    if depth == 0 {
                        return Some(pos);
                    }
                }
            }
            return None;
        }

        if let Some((open, close)) = is_close {
            let mut depth = 1usize;
            for pos in (scan_range.start..offset).rev() {
                if in_skip(pos) {
                    continue;
                }
                if bytes[pos] == close {
                    depth += 1;
                } else if bytes[pos] == open {
                    depth -= 1;
                    if depth == 0 {
                        return Some(pos);
                    }
                }
            }
        }

        None
    }

    /// Every `{ ... }` block in the tree that spans more than one line,
    /// found generically (first child is an anonymous `{` token, last is
    /// an anonymous `}` token) rather than by naming specific Dart node
    /// kinds, so it keeps working across grammar versions.
    ///
    /// Python has no braces: a compound statement (`def`, `class`, `if`,
    /// `with`, ...) folds from its header line to the end of its `block`.
    pub fn fold_ranges(&self) -> Vec<FoldRange> {
        let mut ranges = Vec::new();
        match self.language {
            SyntaxLanguage::Dart | SyntaxLanguage::Json => {
                collect_fold_ranges(self.tree.root_node(), &mut ranges)
            }
            SyntaxLanguage::Python => collect_block_folds(self.tree.root_node(), &mut ranges),
            SyntaxLanguage::Yaml | SyntaxLanguage::Toml | SyntaxLanguage::Markdown => {}
        }
        ranges.sort_by_key(|r| r.start_byte);
        ranges
    }
}

/// The tree-sitter `Point` (row, column — both 0-indexed, column in bytes)
/// for a byte offset into `source`. Needed to build an [`InputEdit`] for
/// [`SyntaxTree::reparse`].
pub fn point_at(source: &str, byte: usize) -> Point {
    let byte = byte.min(source.len());
    let before = &source[..byte];
    let row = before.matches('\n').count();
    let column = match before.rfind('\n') {
        Some(newline) => byte - newline - 1,
        None => byte,
    };
    Point { row, column }
}

fn collect_fold_ranges(node: Node, out: &mut Vec<FoldRange>) {
    // A fold must span several lines, and so must every ancestor of it:
    // skipping single-line nodes prunes the vast majority of the tree.
    let (start_line, end_line) = (node.start_position().row, node.end_position().row);
    if end_line == start_line {
        return;
    }

    let count = node.child_count();
    if count > 0 {
        if let (Some(first), Some(last)) = (node.child(0), node.child(count - 1)) {
            if first.kind() == "{" && last.kind() == "}" {
                out.push(FoldRange {
                    start_line,
                    end_line,
                    start_byte: node.start_byte(),
                    end_byte: node.end_byte(),
                });
            }
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_fold_ranges(child, out);
    }
}

/// Indentation-based folds: every multi-line node with a `block` child
/// folds from its own first line to the block's last line. Decorated
/// definitions fold from the `def`/`class` line, so sticky scroll shows the
/// signature rather than the decorator.
fn collect_block_folds(node: Node, out: &mut Vec<FoldRange>) {
    let (start_line, end_line) = (node.start_position().row, node.end_position().row);
    if end_line == start_line {
        return;
    }
    let mut cursor = node.walk();
    if node.kind() != "decorated_definition" {
        if let Some(block) = node.children(&mut cursor).find(|c| c.kind() == "block") {
            let block_end = block.end_position().row;
            if block_end > start_line {
                out.push(FoldRange {
                    start_line,
                    end_line: block_end,
                    start_byte: node.start_byte(),
                    end_byte: block.end_byte(),
                });
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_block_folds(child, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "// A comment\nclass Foo {\n  int bar() {\n    return 1;\n  }\n}\n";

    const PY: &str = "import os\n\n@dataclass\nclass Model:\n    \"\"\"Doc.\"\"\"\n\n    def forward(self, x: int) -> int:\n        if x:\n            return x + 1\n        return 0\n";

    #[test]
    fn python_highlights_keywords_strings_and_self() {
        let tree = SyntaxTree::parse_as(SyntaxLanguage::Python, PY);
        let hs = tree.highlights(PY);
        // Later spans win in the editor, so check the last one per range.
        // Match whole words: `class` must not hit `dataclass`.
        let kind_of = |text: &str| {
            let at = PY
                .match_indices(text)
                .map(|(i, _)| i)
                .find(|&i| {
                    let before = PY[..i].chars().last();
                    !before.is_some_and(|c| c.is_alphanumeric() || c == '_')
                })
                .unwrap();
            hs.iter()
                .filter(|h| h.range == (at..at + text.len()))
                .last()
                .map(|h| h.kind)
        };
        assert_eq!(kind_of("def"), Some(HighlightKind::Keyword));
        assert_eq!(kind_of("class"), Some(HighlightKind::Keyword));
        assert_eq!(kind_of("self"), Some(HighlightKind::Keyword));
        assert_eq!(kind_of("Model"), Some(HighlightKind::Type));
        assert_eq!(kind_of("forward"), Some(HighlightKind::Function));
        assert_eq!(kind_of("int"), Some(HighlightKind::BuiltinType));
        assert_eq!(kind_of("dataclass"), Some(HighlightKind::Attribute));
        assert_eq!(kind_of("\"\"\"Doc.\"\"\""), Some(HighlightKind::String));
    }

    #[test]
    fn python_folds_follow_blocks() {
        let tree = SyntaxTree::parse_as(SyntaxLanguage::Python, PY);
        let folds: Vec<(usize, usize)> =
            tree.fold_ranges().iter().map(|f| (f.start_line, f.end_line)).collect();
        // class (line 3, not the decorator on 2), def, if.
        assert_eq!(folds, vec![(3, 9), (6, 9), (7, 8)]);
    }

    #[test]
    fn language_ids_map_to_grammars() {
        assert_eq!(SyntaxLanguage::from_language_id("python"), Some(SyntaxLanguage::Python));
        assert_eq!(SyntaxLanguage::from_language_id("dart"), Some(SyntaxLanguage::Dart));
        assert_eq!(SyntaxLanguage::from_language_id("json"), Some(SyntaxLanguage::Json));
        assert_eq!(SyntaxLanguage::from_language_id("jsonc"), Some(SyntaxLanguage::Json));
        assert_eq!(SyntaxLanguage::from_language_id("yaml"), Some(SyntaxLanguage::Yaml));
        assert_eq!(SyntaxLanguage::from_language_id("yml"), Some(SyntaxLanguage::Yaml));
        assert_eq!(SyntaxLanguage::from_language_id("toml"), Some(SyntaxLanguage::Toml));
        assert_eq!(SyntaxLanguage::from_language_id("markdown"), Some(SyntaxLanguage::Markdown));
        assert_eq!(SyntaxLanguage::from_language_id("md"), Some(SyntaxLanguage::Markdown));
        assert_eq!(SyntaxLanguage::from_language_id("rust"), None);
    }

    #[test]
    fn config_and_markdown_grammars_highlight_and_fold() {
        let json = "{\n  \"name\": \"shame\",\n  \"count\": 42,\n  \"ok\": true\n}\n";
        let json_tree = SyntaxTree::parse_as(SyntaxLanguage::Json, json);
        let json_hs = json_tree.highlights(json);
        assert!(json_hs.iter().any(|h| h.kind == HighlightKind::Property && &json[h.range.clone()] == "\"name\""));
        assert!(json_hs.iter().any(|h| h.kind == HighlightKind::Number && &json[h.range.clone()] == "42"));
        assert_eq!(json_tree.fold_ranges().len(), 1);

        let yaml = "name: shame\n# comment\ncount: 3\n";
        let yaml_tree = SyntaxTree::parse_as(SyntaxLanguage::Yaml, yaml);
        let yaml_hs = yaml_tree.highlights(yaml);
        assert!(yaml_hs.iter().any(|h| h.kind == HighlightKind::Comment));
        assert!(yaml_hs.iter().any(|h| h.kind == HighlightKind::Number));

        let toml = "[package]\nname = \"shame\"\nversion = 1\n";
        let toml_tree = SyntaxTree::parse_as(SyntaxLanguage::Toml, toml);
        let toml_hs = toml_tree.highlights(toml);
        assert!(toml_hs.iter().any(|h| h.kind == HighlightKind::String));
        assert!(toml_hs.iter().any(|h| h.kind == HighlightKind::Number));

        let md = "# Heading\n\n```rust\nfn main() {}\n```\n";
        let md_tree = SyntaxTree::parse_as(SyntaxLanguage::Markdown, md);
        let md_hs = md_tree.highlights(md);
        assert!(!md_hs.is_empty());
    }

    #[test]
    fn parses_without_panicking() {
        let tree = SyntaxTree::parse(SAMPLE);
        assert_eq!(tree.root().start_byte(), 0);
        assert_eq!(tree.root().end_byte(), SAMPLE.len());
    }

    #[test]
    fn highlights_find_the_keyword_and_the_comment() {
        let tree = SyntaxTree::parse(SAMPLE);
        let highlights = tree.highlights(SAMPLE);

        assert!(
            highlights
                .iter()
                .any(|h| h.kind == HighlightKind::Comment && SAMPLE[h.range.clone()].contains("A comment")),
            "expected a Comment highlight covering the leading comment"
        );
        assert!(
            highlights.iter().any(|h| h.kind == HighlightKind::Keyword && &SAMPLE[h.range.clone()] == "class"),
            "expected a Keyword highlight for `class`"
        );
    }

    #[test]
    fn matching_bracket_finds_the_outer_braces() {
        let tree = SyntaxTree::parse(SAMPLE);
        let open = SAMPLE.find('{').unwrap();
        let close = SAMPLE.rfind('}').unwrap();

        assert_eq!(tree.matching_bracket(SAMPLE, open), Some(close));
        assert_eq!(tree.matching_bracket(SAMPLE, close), Some(open));
    }

    #[test]
    fn fold_ranges_cover_multiline_blocks_only() {
        let tree = SyntaxTree::parse(SAMPLE);
        let folds = tree.fold_ranges();

        // `class Foo { ... }` and `int bar() { ... }` both span multiple
        // lines; an empty or single-line `{}` would not appear here.
        assert_eq!(folds.len(), 2);
        assert!(folds.iter().all(|f| f.end_line > f.start_line));
    }

    #[test]
    fn reparse_reflects_an_inserted_line() {
        let mut tree = SyntaxTree::parse(SAMPLE);
        let insert_at = SAMPLE.find("return 1;").unwrap();
        let inserted = "// inserted\n    ";
        let new_source = format!("{}{}{}", &SAMPLE[..insert_at], inserted, &SAMPLE[insert_at..]);

        let edit = InputEdit {
            start_byte: insert_at,
            old_end_byte: insert_at,
            new_end_byte: insert_at + inserted.len(),
            start_position: point_at(SAMPLE, insert_at),
            old_end_position: point_at(SAMPLE, insert_at),
            new_end_position: point_at(&new_source, insert_at + inserted.len()),
        };
        tree.reparse(&new_source, edit);

        assert_eq!(tree.root().end_byte(), new_source.len());
        let highlights = tree.highlights(&new_source);
        assert!(
            highlights
                .iter()
                .any(|h| h.kind == HighlightKind::Comment && new_source[h.range.clone()].contains("inserted")),
            "expected the newly inserted comment to be highlighted after reparse"
        );
    }

    /// The unpruned walk this function replaced, kept as an oracle.
    fn naive_fold_ranges(node: Node, out: &mut Vec<FoldRange>) {
        let mut cursor = node.walk();
        let children: Vec<Node> = node.children(&mut cursor).collect();
        if let (Some(first), Some(last)) = (children.first(), children.last()) {
            if first.kind() == "{" && last.kind() == "}" {
                let (start_line, end_line) = (node.start_position().row, node.end_position().row);
                if end_line > start_line {
                    out.push(FoldRange {
                        start_line,
                        end_line,
                        start_byte: node.start_byte(),
                        end_byte: node.end_byte(),
                    });
                }
            }
        }
        for child in children {
            naive_fold_ranges(child, out);
        }
    }

    #[test]
    fn pruned_fold_ranges_match_the_naive_walk() {
        let source = "class A {\n  void f() { return; }\n  int g() {\n    if (x) {\n      y();\n    }\n    return 1;\n  }\n}\nvoid main() {\n  var m = {1: 2};\n}\n";
        let tree = SyntaxTree::parse(source);
        let mut expected = Vec::new();
        naive_fold_ranges(tree.root(), &mut expected);
        expected.sort_by_key(|r| r.start_byte);
        assert!(!expected.is_empty());
        assert_eq!(tree.fold_ranges(), expected);
    }
}
