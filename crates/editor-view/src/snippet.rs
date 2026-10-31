//! LSP snippet syntax (`$1`, `${1:placeholder}`, `$0`) and tab-stop sessions,
//! including mirrored placeholders: write `${1:Name}` twice and typing over the
//! first copy updates the second when you press Tab.

use std::ops::Range;

/// A snippet expanded to plain text plus its tab stops, in visiting order
/// (ascending number, `$0` last). Ranges are byte offsets into `text`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParsedSnippet {
    pub text: String,
    pub stops: Vec<Range<usize>>,
    /// For each stop, the other places the same number appears; they receive
    /// the same text when you leave the stop.
    pub mirrors: Vec<Vec<Range<usize>>>,
}

/// Expand `raw`. Supports `$N`, `${N}`, `${N:default}` (defaults may nest
/// other placeholders, which are flattened to their text) and the escapes
/// `\$`, `\}` and `\\`. A repeated number is a mirror of its first occurrence.
/// When stops exist but there is no `$0`, the end of the text is the final stop.
pub fn parse_snippet(raw: &str) -> ParsedSnippet {
    let chars: Vec<char> = raw.chars().collect();
    let mut text = String::with_capacity(raw.len());
    // (number, first occurrence, further occurrences)
    let mut numbered: Vec<(u32, Range<usize>, Vec<Range<usize>>)> = Vec::new();
    let mut i = 0;

    let record = |n: u32, range: Range<usize>, numbered: &mut Vec<(u32, Range<usize>, Vec<Range<usize>>)>| {
        match numbered.iter_mut().find(|(m, _, _)| *m == n) {
            Some((_, _, mirrors)) => mirrors.push(range),
            None => numbered.push((n, range, Vec::new())),
        }
    };

    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() && matches!(chars[i + 1], '$' | '}' | '\\') {
            text.push(chars[i + 1]);
            i += 2;
        } else if c == '$' && i + 1 < chars.len() && chars[i + 1].is_ascii_digit() {
            let (n, next) = read_number(&chars, i + 1);
            record(n, text.len()..text.len(), &mut numbered);
            i = next;
        } else if c == '$' && i + 1 < chars.len() && chars[i + 1] == '{' && chars.get(i + 2).is_some_and(|d| d.is_ascii_digit()) {
            let (n, mut next) = read_number(&chars, i + 2);
            let start = text.len();
            if chars.get(next) == Some(&':') {
                next += 1;
                let mut depth = 1;
                let mut inner = String::new();
                while next < chars.len() {
                    match chars[next] {
                        '\\' if next + 1 < chars.len() => {
                            inner.push(chars[next]);
                            inner.push(chars[next + 1]);
                            next += 2;
                            continue;
                        }
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    inner.push(chars[next]);
                    next += 1;
                }
                text.push_str(&parse_snippet(&inner).text);
            }
            if chars.get(next) == Some(&'}') {
                next += 1;
            }
            record(n, start..text.len(), &mut numbered);
            i = next;
        } else {
            text.push(c);
            i += 1;
        }
    }

    numbered.sort_by_key(|(n, _, _)| if *n == 0 { u32::MAX } else { *n });
    let has_final = numbered.iter().any(|(n, _, _)| *n == 0);
    let mut stops: Vec<Range<usize>> = numbered.iter().map(|(_, r, _)| r.clone()).collect();
    let mut mirrors: Vec<Vec<Range<usize>>> = numbered.into_iter().map(|(_, _, m)| m).collect();
    if !stops.is_empty() && !has_final {
        stops.push(text.len()..text.len());
        mirrors.push(Vec::new());
    }
    ParsedSnippet { text, stops, mirrors }
}

fn read_number(chars: &[char], mut at: usize) -> (u32, usize) {
    let mut n: u32 = 0;
    while at < chars.len() && chars[at].is_ascii_digit() {
        n = n.saturating_mul(10).saturating_add(chars[at].to_digit(10).unwrap_or(0));
        at += 1;
    }
    (n, at)
}

/// A snippet whose text spans several lines, re-indented so every line after
/// the first starts at `indent` (the leading whitespace of the line it is
/// inserted into). Stop and mirror offsets move with the inserted whitespace.
pub fn indent_snippet(snippet: &ParsedSnippet, indent: &str) -> ParsedSnippet {
    if indent.is_empty() || !snippet.text.contains('\n') {
        return snippet.clone();
    }
    // Byte offsets of the inserted indents, in the original text's coordinates.
    let inserts: Vec<usize> = snippet.text.match_indices('\n').map(|(i, _)| i + 1).collect();
    let mut text = String::with_capacity(snippet.text.len() + inserts.len() * indent.len());
    let mut last = 0;
    for &at in &inserts {
        text.push_str(&snippet.text[last..at]);
        // Blank lines stay blank rather than gaining trailing spaces.
        if !snippet.text[at..].starts_with('\n') {
            text.push_str(indent);
        }
        last = at;
    }
    text.push_str(&snippet.text[last..]);
    let moved = |offset: usize| -> usize {
        offset
            + inserts
                .iter()
                .filter(|&&at| at <= offset && !snippet.text[at..].starts_with('\n'))
                .count()
                * indent.len()
    };
    let shift = |r: &Range<usize>| moved(r.start)..moved(r.end);
    ParsedSnippet {
        text,
        stops: snippet.stops.iter().map(shift).collect(),
        mirrors: snippet.mirrors.iter().map(|ms| ms.iter().map(shift).collect()).collect(),
    }
}

/// Move `range` by `delta` if it starts at or after `from`.
fn shift_range(range: &mut Range<usize>, from: usize, delta: isize) {
    if range.start >= from {
        range.start = (range.start as isize + delta).max(0) as usize;
        range.end = (range.end as isize + delta).max(0) as usize;
    }
}

/// An in-progress snippet: the tab stops not yet visited (and their mirrors),
/// as absolute buffer offsets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SnippetSession {
    pub stops: Vec<Range<usize>>,
    pub mirrors: Vec<Vec<Range<usize>>>,
    /// Buffer length when the current stop was activated; the difference to
    /// the length now is how much was typed into it.
    pub len_at_active: usize,
    /// The stop currently selected, used to detect that the cursor has since left it.
    pub active: Option<Range<usize>>,
    /// Mirrors of the current stop, to be filled in when it is left.
    pub active_mirrors: Vec<Range<usize>>,
}

impl SnippetSession {
    pub(crate) fn new(stops: Vec<Range<usize>>, mirrors: Vec<Vec<Range<usize>>>, len: usize) -> Self {
        let mut mirrors = mirrors;
        mirrors.resize(stops.len(), Vec::new());
        Self { stops, mirrors, len_at_active: len, active: None, active_mirrors: Vec::new() }
    }

    /// Whether `head` is still inside the active placeholder, allowing for
    /// text typed into it (the buffer grew by `current_len - len_at_active`).
    pub(crate) fn cursor_in_active(&self, head: usize, current_len: usize) -> bool {
        match &self.active {
            None => true,
            Some(active) => {
                let grew = current_len.saturating_sub(self.len_at_active);
                head >= active.start && head <= active.end + grew
            }
        }
    }

    /// Leaving the active stop: account for what was typed into it (shifting
    /// everything positioned after it) and return where that text now is, with
    /// the mirrors that should receive it. `None` on the very first activation.
    pub(crate) fn begin_advance(&mut self, current_len: usize) -> Option<(Range<usize>, Vec<Range<usize>>)> {
        let active = self.active.clone()?;
        let delta = current_len as isize - self.len_at_active as isize;
        for stop in &mut self.stops {
            shift_range(stop, active.end, delta);
        }
        for list in &mut self.mirrors {
            for mirror in list {
                shift_range(mirror, active.end, delta);
            }
        }
        for mirror in &mut self.active_mirrors {
            shift_range(mirror, active.end, delta);
        }
        let now = active.start..((active.end as isize + delta).max(active.start as isize) as usize);
        self.active = Some(now.clone());
        self.len_at_active = current_len;
        Some((now, self.active_mirrors.clone()))
    }

    /// `edit` (a mirror, now holding text of `new_len` bytes) was rewritten:
    /// shift everything positioned after it. Apply mirror edits from the last
    /// position to the first so earlier ones stay valid.
    pub(crate) fn mirror_applied(&mut self, edit: &Range<usize>, new_len: usize) {
        let delta = new_len as isize - edit.len() as isize;
        for stop in &mut self.stops {
            shift_range(stop, edit.end, delta);
        }
        for list in &mut self.mirrors {
            for mirror in list {
                shift_range(mirror, edit.end, delta);
            }
        }
        if let Some(active) = &mut self.active {
            shift_range(active, edit.end, delta);
        }
        self.len_at_active = (self.len_at_active as isize + delta).max(0) as usize;
    }

    /// Take the next stop (after any mirror edits), or `None` when done.
    pub(crate) fn finish_advance(&mut self, current_len: usize) -> Option<Range<usize>> {
        self.len_at_active = current_len;
        if self.stops.is_empty() {
            self.active = None;
            self.active_mirrors.clear();
            return None;
        }
        let stop = self.stops.remove(0);
        self.active_mirrors = self.mirrors.remove(0);
        self.active = Some(stop.clone());
        Some(stop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_placeholders_and_orders_stops() {
        let s = parse_snippet("foo(${2:b}, ${1:a})$0");
        assert_eq!(s.text, "foo(b, a)");
        // stop 1 = "a", stop 2 = "b", then $0 at the end
        assert_eq!(s.stops, vec![7..8, 4..5, 9..9]);
        assert!(s.mirrors.iter().all(Vec::is_empty));
    }

    #[test]
    fn bare_and_braced_numbers_are_empty_stops() {
        let s = parse_snippet("a($1, ${2})");
        assert_eq!(s.text, "a(, )");
        assert_eq!(s.stops, vec![2..2, 4..4, 5..5]);
    }

    #[test]
    fn no_stops_means_plain_text() {
        let s = parse_snippet("print");
        assert_eq!(s, ParsedSnippet { text: "print".into(), stops: vec![], mirrors: vec![] });
    }

    #[test]
    fn escapes_and_nested_placeholders() {
        let s = parse_snippet(r"cost \$5 ${1:x ${2:y}}");
        assert_eq!(s.text, "cost $5 x y");
        assert_eq!(s.stops[0], 8..11);
    }

    #[test]
    fn a_repeated_number_is_a_mirror_of_the_first() {
        let s = parse_snippet("${1:a} + ${1:a}");
        assert_eq!(s.text, "a + a");
        assert_eq!(s.stops[0], 0..1);
        assert_eq!(s.mirrors[0], vec![4..5]);
        // A bare repeat is an empty mirror at its position.
        let s = parse_snippet("${1:Name}($1)");
        assert_eq!(s.text, "Name()");
        assert_eq!(s.mirrors[0], vec![5..5]);
    }

    #[test]
    fn indenting_continues_every_line_and_moves_the_stops() {
        let s = parse_snippet("if (${1:ok}) {\n  $0\n}");
        assert_eq!(s.text, "if (ok) {\n  \n}");
        let indented = indent_snippet(&s, "    ");
        assert_eq!(indented.text, "if (ok) {\n      \n    }");
        // `ok` is on the first line, so it does not move; `$0` sits after the new indent.
        assert_eq!(indented.stops[0], 4..6);
        let final_stop = indented.stops[1].start;
        assert_eq!(&indented.text[..final_stop], "if (ok) {\n      ");
        // Blank lines do not gain trailing spaces; no indent changes nothing.
        let blank = parse_snippet("a\n\nb");
        assert_eq!(indent_snippet(&blank, "  ").text, "a\n\n  b");
        assert_eq!(indent_snippet(&s, ""), s);
    }

    /// Drive a session over a plain string the way the editor does.
    fn advance(text: &mut String, session: &mut SnippetSession) -> Option<Range<usize>> {
        if let Some((typed, mirrors)) = session.begin_advance(text.len()) {
            let typed_text = text[typed].to_string();
            let mut mirrors = mirrors;
            mirrors.sort_by_key(|m| std::cmp::Reverse(m.start));
            for mirror in mirrors {
                text.replace_range(mirror.clone(), &typed_text);
                session.mirror_applied(&mirror, typed_text.len());
            }
        }
        session.finish_advance(text.len())
    }

    #[test]
    fn typing_into_a_placeholder_updates_its_mirrors_on_tab() {
        let parsed = parse_snippet("class ${1:A} {\n  ${1:A}();\n}");
        let mut text = parsed.text.clone();
        assert_eq!(text, "class A {\n  A();\n}");
        let mut session = SnippetSession::new(parsed.stops, parsed.mirrors, text.len());

        let first = advance(&mut text, &mut session).unwrap();
        assert_eq!(&text[first.clone()], "A");
        // The user types "Counter" over the selected name.
        text.replace_range(first, "Counter");

        advance(&mut text, &mut session);
        assert_eq!(text, "class Counter {\n  Counter();\n}");
    }

    #[test]
    fn a_stop_positioned_before_the_active_one_is_not_shifted() {
        // `b` (stop 2) sits before `a` (stop 1) in the text.
        let parsed = parse_snippet("foo(${2:b}, ${1:a})$0");
        let mut text = parsed.text.clone();
        let mut session = SnippetSession::new(parsed.stops, parsed.mirrors, text.len());

        let a = advance(&mut text, &mut session).unwrap();
        assert_eq!(&text[a.clone()], "a");
        text.replace_range(a, "alpha");

        let b = advance(&mut text, &mut session).unwrap();
        assert_eq!(&text[b.clone()], "b", "stop 2 did not move when text was typed after it");
        text.replace_range(b, "beta");

        let end = advance(&mut text, &mut session).unwrap();
        assert_eq!(text, "foo(beta, alpha)");
        assert_eq!(end, text.len()..text.len(), "$0 followed both edits to the end");
    }

    #[test]
    fn several_mirrors_all_update_without_corrupting_each_other() {
        let parsed = parse_snippet("${1:x} = ${1:x} + ${1:x};${2:y}");
        let mut text = parsed.text.clone();
        let mut session = SnippetSession::new(parsed.stops, parsed.mirrors, text.len());
        let first = advance(&mut text, &mut session).unwrap();
        text.replace_range(first, "total");
        let second = advance(&mut text, &mut session).unwrap();
        assert_eq!(text, "total = total + total;y");
        assert_eq!(&text[second], "y");
    }

    #[test]
    fn cursor_outside_the_active_placeholder_is_detected() {
        let mut session = SnippetSession::new(vec![4..6, 9..9], vec![], 10);
        session.finish_advance(10);
        assert!(session.cursor_in_active(4, 10));
        assert!(session.cursor_in_active(6, 10));
        assert!(session.cursor_in_active(8, 12), "typing grew the placeholder");
        assert!(!session.cursor_in_active(0, 10));
        assert!(!session.cursor_in_active(9, 10));
    }
}
