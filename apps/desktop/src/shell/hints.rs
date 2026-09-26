//! Hint mode (F): every visible target gets a one- or two-letter home-row
//! code; typing narrows the field until one remains and activates.
//!
//! Generating codes and placing them over the targets' recorded bounds is
//! the shell's job; drawing one label is the key cap's (hint voice).

use super::focus::Target;
use gpui::{Bounds, Pixels};

/// Home row first, then the rest of the easy reach.
const ALPHABET: &[char] = &['a', 's', 'd', 'f', 'g', 'h', 'j', 'k', 'l', 'e', 'r', 'u', 'i', 'o', 'w', 'n'];

/// One labelled target.
#[derive(Clone)]
pub(crate) struct Hinted {
    /// The code.
    pub code: String,
    /// The target.
    pub target: Target,
    /// Where it is, window coordinates.
    pub bounds: Bounds<Pixels>,
}

/// An active hint mode.
#[derive(Clone)]
pub(crate) struct HintMode {
    hinted: Vec<Hinted>,
    typed: String,
}

/// What one key did.
pub(crate) enum Step {
    /// Still narrowing.
    Narrowed,
    /// One target matched: activate it (the mode ends).
    Chosen(Target),
    /// Nothing matches that key: the mode ends.
    Missed,
}

/// `count` distinct codes of the shortest length that holds them, no code a
/// prefix of another.
pub(crate) fn codes(count: usize) -> Vec<String> {
    let base = ALPHABET.len();
    if count <= base {
        return ALPHABET.iter().take(count).map(char::to_string).collect();
    }
    let mut out = Vec::with_capacity(count);
    'outer: for first in ALPHABET {
        for second in ALPHABET {
            if out.len() == count {
                break 'outer;
            }
            out.push(format!("{first}{second}"));
        }
    }
    out
}

impl HintMode {
    /// Labels `targets` (already filtered to the visible ones).
    pub(crate) fn new(targets: Vec<(Target, Bounds<Pixels>)>) -> Self {
        let codes = codes(targets.len().min(ALPHABET.len() * ALPHABET.len()));
        let hinted = targets
            .into_iter()
            .zip(codes)
            .map(|((target, bounds), code)| Hinted { code, target, bounds })
            .collect();
        Self {
            hinted,
            typed: String::new(),
        }
    }

    /// The labels still in play and how much of each is typed.
    pub(crate) fn visible(&self) -> impl Iterator<Item = (&Hinted, usize)> {
        self.hinted
            .iter()
            .filter(|hinted| hinted.code.starts_with(&self.typed))
            .map(|hinted| (hinted, self.typed.len()))
    }

    /// How many labels are in play.
    pub(crate) fn remaining(&self) -> usize {
        self.visible().count()
    }

    /// Types one key.
    pub(crate) fn key(&mut self, key: char) -> Step {
        let mut typed = self.typed.clone();
        typed.push(key.to_ascii_lowercase());
        let matching = self
            .hinted
            .iter()
            .filter(|hinted| hinted.code.starts_with(&typed))
            .collect::<Vec<_>>();
        match matching.as_slice() {
            [] => Step::Missed,
            [only] if only.code == typed => Step::Chosen(only.target.clone()),
            _ => {
                self.typed = typed;
                Step::Narrowed
            }
        }
    }

    /// Un-types one key; `false` when nothing was typed.
    pub(crate) fn backspace(&mut self) -> bool {
        self.typed.pop().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, px, size};
    use std::rc::Rc;

    fn target(id: &str) -> (Target, Bounds<Pixels>) {
        (
            Target {
                id: id.to_owned().into(),
                label: id.to_owned().into(),
                act: Rc::new(|_, _| {}),
                peek: None,
                source: None,
            },
            Bounds::new(point(px(0.0), px(0.0)), size(px(10.0), px(10.0))),
        )
    }

    #[test]
    fn codes_are_unique_and_prefix_free() {
        for count in [1, 9, 16, 17, 60, 256] {
            let codes = codes(count);
            assert_eq!(codes.len(), count);
            for (index, code) in codes.iter().enumerate() {
                for (other_index, other) in codes.iter().enumerate() {
                    if index != other_index {
                        assert!(!other.starts_with(code.as_str()), "{code} prefixes {other}");
                    }
                }
            }
        }
    }

    #[test]
    fn typing_narrows_then_chooses_and_a_miss_ends() {
        let mut mode = HintMode::new((0..20).map(|index| target(&format!("t{index}"))).collect());
        assert_eq!(mode.remaining(), 20);
        assert!(matches!(mode.key('a'), Step::Narrowed));
        assert_eq!(mode.remaining(), 16, "every code starting with a");
        match mode.key('s') {
            Step::Chosen(target) => assert_eq!(target.id.as_ref(), "t1"),
            _ => panic!("as names exactly one target"),
        }
        let mut mode = HintMode::new((0..3).map(|index| target(&format!("t{index}"))).collect());
        assert!(matches!(mode.key('z'), Step::Missed));
    }
}
