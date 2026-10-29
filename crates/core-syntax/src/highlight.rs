use std::ops::Range;

/// A coarse, editor-facing highlight category. Tree-sitter highlight
/// queries emit fine-grained dotted capture names (`function.method`,
/// `variable.builtin`, ...); we fold those down to one of these so
/// `editor-view` only has to pick a handful of colors, not learn every
/// capture name a grammar's query happens to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HighlightKind {
    Keyword,
    String,
    Comment,
    Number,
    Function,
    Type,
    BuiltinType,
    Constant,
    Property,
    Variable,
    Operator,
    Punctuation,
    Attribute,
    Other,
}

impl HighlightKind {
    pub fn from_capture_name(name: &str) -> Self {
        if name == "type.builtin" {
            return Self::BuiltinType;
        }
        if name == "variable.builtin" {
            return Self::Keyword;
        }
        let base = name.split('.').next().unwrap_or(name);
        match base {
            "keyword" => Self::Keyword,
            "string" => Self::String,
            "comment" => Self::Comment,
            "number" => Self::Number,
            "boolean" | "constant" => Self::Constant,
            "function" | "method" | "constructor" => Self::Function,
            "type" => Self::Type,
            "property" | "field" => Self::Property,
            "variable" | "parameter" => Self::Variable,
            "operator" => Self::Operator,
            "punctuation" => Self::Punctuation,
            "attribute" | "label" | "annotation" => Self::Attribute,
            _ => Self::Other,
        }
    }
}

/// One highlighted span, as byte offsets into the source that was parsed.
#[derive(Debug, Clone)]
pub struct Highlight {
    pub range: Range<usize>,
    pub kind: HighlightKind,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_known_capture_names() {
        assert_eq!(HighlightKind::from_capture_name("keyword"), HighlightKind::Keyword);
        assert_eq!(
            HighlightKind::from_capture_name("function.method"),
            HighlightKind::Function
        );
        assert_eq!(HighlightKind::from_capture_name("comment"), HighlightKind::Comment);
    }

    #[test]
    fn unknown_capture_names_fall_back_to_other() {
        assert_eq!(HighlightKind::from_capture_name("spooky.unknown"), HighlightKind::Other);
    }
}
