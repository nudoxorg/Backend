//! Narrow (`v6/cohesion/COHESION.md`, "The sidebar: primitives", 3). No
//! field: typing while the sidebar has the keyboard narrows the current
//! scope in place. It searches collapsed rows too and keeps each match's
//! parents as context (JetBrains' speed search only searches expanded
//! nodes). A count says how much of the scope matched. The last row widens
//! the same query to Ask, the whole library's search (Things' narrow, then
//! widen). Esc clears it.
//!
//! "Used by" narrows the same way: choosing one of your crates keeps what
//! that crate uses ("only what desktop uses"), and Esc clears that too.

use super::state::WorkspaceCrate;
use std::ops::Range;

/// The words being typed to narrow the scope.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Narrow {
    query: String,
}

impl Narrow {
    /// The words.
    pub(crate) fn query(&self) -> &str {
        &self.query
    }

    /// Whether anything is typed.
    pub(crate) fn is_empty(&self) -> bool {
        self.query.is_empty()
    }

    /// Types one character. Whitespace never starts or extends a query:
    /// Space is the peek's key.
    pub(crate) fn push(&mut self, character: char) {
        if !character.is_whitespace() && !character.is_control() {
            self.query.push(character);
        }
    }

    /// Types several characters (a chord's letters that turned out to be
    /// words).
    pub(crate) fn push_str(&mut self, text: &str) {
        text.chars().for_each(|character| self.push(character));
    }

    /// Deletes the last character. Returns whether there was one.
    pub(crate) fn pop(&mut self) -> bool {
        self.query.pop().is_some()
    }

    /// Clears the words. Returns whether there were any.
    pub(crate) fn clear(&mut self) -> bool {
        let had = !self.query.is_empty();
        self.query.clear();
        had
    }
}

/// What the list is narrowed to: the typed words, and the one of your
/// crates whose uses to keep.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Filter<'a> {
    /// The typed words.
    pub query: &'a str,
    /// The crate of yours whose uses are kept.
    pub via: Option<&'a WorkspaceCrate>,
}

impl Filter<'_> {
    /// Whether anything narrows the list.
    pub(crate) fn is_active(&self) -> bool {
        !self.query.is_empty() || self.via.is_some()
    }
}

/// How much of a scope matched: the "9 of 191".
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Matched {
    /// Nodes that matched (context parents are not counted).
    pub shown: usize,
    /// Nodes in the scope, collapsed ones too.
    pub of: usize,
}

/// Where `query` occurs in `name`, ignoring case: the byte range of the
/// characters to underline.
pub(crate) fn find_fold(name: &str, query: &str) -> Option<Range<usize>> {
    if query.is_empty() {
        return None;
    }
    let fold = |c: char| c.to_lowercase().next().unwrap_or(c);
    let wanted: Vec<char> = query.chars().map(fold).collect();
    name.char_indices().find_map(|(start, _)| {
        let mut chars = name[start..].char_indices();
        let mut end = start;
        for want in &wanted {
            let (offset, got) = chars.next()?;
            if fold(got) != *want {
                return None;
            }
            end = start + offset + got.len_utf8();
        }
        Some(start..end)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_builds_a_query_and_backspace_takes_it_apart_and_space_is_never_part_of_it() {
        let mut narrow = Narrow::default();
        assert!(narrow.is_empty());
        narrow.push('a');
        narrow.push('s');
        narrow.push(' ');
        narrow.push('_');
        assert_eq!(narrow.query(), "as_", "the peek owns Space");
        assert!(narrow.pop());
        assert_eq!(narrow.query(), "as");
        assert!(narrow.clear());
        assert!(!narrow.clear(), "nothing to clear twice");
        assert!(!narrow.pop());
    }

    #[test]
    fn a_match_is_found_anywhere_in_the_name_ignoring_case_and_reports_the_bytes_to_underline() {
        assert_eq!(find_fold("as_str", "as_"), Some(0..3));
        assert_eq!(find_fold("ValueDeserializer", "deser"), Some(5..10));
        assert_eq!(find_fold("AS_STR", "as_s"), Some(0..4));
        assert_eq!(find_fold("as_str", "xyz"), None);
        assert_eq!(
            find_fold("as_str", ""),
            None,
            "an empty query matches everything and underlines nothing"
        );
        assert_eq!(
            find_fold("héllo", "llo"),
            Some(3..6),
            "byte offsets survive a multi-byte character"
        );
        assert_eq!(find_fold("héllo", "É"), Some(1..3));
        assert_eq!(
            find_fold("ab", "abc"),
            None,
            "a query longer than the name never matches"
        );
    }
}
