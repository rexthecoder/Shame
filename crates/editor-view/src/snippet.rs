//! LSP snippet syntax (`$1`, `${1:placeholder}`, `$0`) and tab-stop sessions.

use std::ops::Range;

/// A snippet expanded to plain text plus its tab stops, in visiting order
/// (ascending number, `$0` last). Ranges are byte offsets into `text`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParsedSnippet {
    pub text: String,
    pub stops: Vec<Range<usize>>,
}

/// Expand `raw`. Supports `$N`, `${N}`, `${N:default}` (defaults may nest
/// other placeholders, which are flattened to their text) and the escapes
/// `\$`, `\}` and `\\`. A repeated number keeps only its first occurrence as the
/// tab stop. When stops exist but there is no `$0`, the end of the text is
/// used as the final stop.
pub fn parse_snippet(raw: &str) -> ParsedSnippet {
    let chars: Vec<char> = raw.chars().collect();
    let mut text = String::with_capacity(raw.len());
    let mut numbered: Vec<(u32, Range<usize>)> = Vec::new();
    let mut i = 0;

    let record = |n: u32, range: Range<usize>, numbered: &mut Vec<(u32, Range<usize>)>| {
        if !numbered.iter().any(|(m, _)| *m == n) {
            numbered.push((n, range));
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

    numbered.sort_by_key(|(n, _)| if *n == 0 { u32::MAX } else { *n });
    let mut stops: Vec<Range<usize>> = numbered.iter().map(|(_, r)| r.clone()).collect();
    if !stops.is_empty() && !numbered.iter().any(|(n, _)| *n == 0) {
        stops.push(text.len()..text.len());
    }
    ParsedSnippet { text, stops }
}

fn read_number(chars: &[char], mut at: usize) -> (u32, usize) {
    let mut n: u32 = 0;
    while at < chars.len() && chars[at].is_ascii_digit() {
        n = n.saturating_mul(10).saturating_add(chars[at].to_digit(10).unwrap_or(0));
        at += 1;
    }
    (n, at)
}

/// An in-progress snippet: the tab stops not yet visited, as absolute
/// buffer offsets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SnippetSession {
    pub stops: Vec<Range<usize>>,
    /// Buffer length when the current stop was activated; the difference to
    /// the length at the next Tab is how far the remaining stops moved.
    pub len_at_active: usize,
    /// The stop currently selected (set by `next_stop`), used to detect that
    /// the cursor has since left the placeholder.
    pub active: Option<Range<usize>>,
}

impl SnippetSession {
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

    /// Pop the next stop, shifted by however much the buffer grew or shrank
    /// since the previous one was activated. `None` when the snippet is done.
    pub(crate) fn next_stop(&mut self, current_len: usize) -> Option<Range<usize>> {
        if self.stops.is_empty() {
            return None;
        }
        let delta = current_len as isize - self.len_at_active as isize;
        let shift = |offset: usize| (offset as isize + delta).max(0) as usize;
        for stop in &mut self.stops {
            *stop = shift(stop.start)..shift(stop.end);
        }
        self.len_at_active = current_len;
        let next = self.stops.remove(0);
        self.active = Some(next.clone());
        Some(next)
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
        assert_eq!(s, ParsedSnippet { text: "print".into(), stops: vec![] });
    }

    #[test]
    fn escapes_and_nested_placeholders() {
        let s = parse_snippet(r"cost \$5 ${1:x ${2:y}}");
        assert_eq!(s.text, "cost $5 x y");
        assert_eq!(s.stops[0], 8..11);
    }

    #[test]
    fn repeated_number_keeps_first_stop() {
        let s = parse_snippet("${1:a} + ${1:a}");
        assert_eq!(s.text, "a + a");
        assert_eq!(s.stops[0], 0..1);
    }

    #[test]
    fn session_shifts_remaining_stops_by_edits() {
        let mut session = SnippetSession { stops: vec![0..1, 5..6, 9..9], len_at_active: 10, active: None };
        let first = session.next_stop(10).unwrap();
        assert_eq!(first, 0..1);
        // The user typed 3 extra bytes into the first placeholder.
        let second = session.next_stop(13).unwrap();
        assert_eq!(second, 8..9);
        assert_eq!(session.stops, vec![12..12]);
    }

    #[test]
    fn cursor_outside_the_active_placeholder_is_detected() {
        let mut session = SnippetSession { stops: vec![4..6, 9..9], len_at_active: 10, active: None };
        session.next_stop(10);
        assert!(session.cursor_in_active(4, 10));
        assert!(session.cursor_in_active(6, 10));
        assert!(session.cursor_in_active(8, 12), "typing grew the placeholder");
        assert!(!session.cursor_in_active(0, 10));
        assert!(!session.cursor_in_active(9, 10));
    }
}
