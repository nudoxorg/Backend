//! Opening a file that a replacing rename may unlink between the open and its checks.
//!
//! A writer that publishes state with a replacing rename unlinks the previous generation while a
//! reader may already hold it. The reader's admission check ("exactly one name, owned by me, mode
//! 0600") then runs against an object that reports zero names, which is a lost race and not a
//! refusal: the file it holds is the complete generation that was published under the name when it
//! opened it, exactly what a POSIX reader of an unlinked inode has. This module is the one place
//! that decides what each kind of caller does about that, on every platform.

use crate::retry::{BACKOFF, retry_when};
use std::io;
use std::time::Duration;

/// How many directory entries may name an object an admission check inspects.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Linkage {
    /// Exactly one: the object is the live entry that was opened by name, and no second name can
    /// reach (and mutate) its bytes.
    Named,
    /// One, or none because a replacing rename or a delete removed the last name after the open.
    /// An unlinked object has no names at all, so it cannot be reached through an alias either.
    NamedOrUnlinked,
}

impl Linkage {
    /// Whether an object that `links` names reach passes the check.
    pub(crate) const fn admits(self, links: u64) -> bool {
        links == 1 || (links == 0 && matches!(self, Self::NamedOrUnlinked))
    }
}

/// What an open does when the object it holds lost its last name before the admission checks ran.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IfUnlinked {
    /// Keep the handle. A reader then has the complete generation that was published under the
    /// name when it opened it. No second open is needed, so a publisher that replaces the name in
    /// a loop cannot starve it.
    Keep,
    /// Discard the handle and open the name again after a short pause. A writer must act on the
    /// object the name refers to now; a handle to the replaced generation would swallow its
    /// writes.
    Reopen,
}

impl IfUnlinked {
    /// The link counts the admission check accepts under this policy.
    pub(crate) const fn linkage(self) -> Linkage {
        match self {
            Self::Keep => Linkage::NamedOrUnlinked,
            Self::Reopen => Linkage::Named,
        }
    }
}

/// One attempt that can lose a race to a replacing rename.
pub(crate) enum Attempt<T> {
    /// The attempt produced its result.
    Done(T),
    /// A replacing rename unlinked what the attempt inspected; look again.
    Replaced,
}

/// Repeats `attempt` after a short pause while it reports that a replacing rename got there
/// first. A name replaced faster than the schedule can settle it is reported as busy rather than
/// spun against.
pub(crate) fn reopen_while_replaced<T>(
    attempt: impl FnMut() -> io::Result<Attempt<T>>,
    pause: impl FnMut(Duration),
    exhausted: &'static str,
) -> io::Result<T> {
    let outcome = retry_when(
        attempt,
        |outcome| matches!(outcome, Ok(Attempt::Replaced)),
        &BACKOFF,
        pause,
    );
    match outcome? {
        Attempt::Done(value) => Ok(value),
        Attempt::Replaced => Err(io::Error::new(io::ErrorKind::ResourceBusy, exhausted)),
    }
}

/// Opens an object and runs `admit` on it, applying `if_unlinked` when the object lost its last
/// name in between.
///
/// `admit` receives the [`Linkage`] this policy accepts; any other admission failure is returned
/// unchanged. `is_unlinked` reports whether a failed admission was the lost race.
pub(crate) fn open_admitted<T>(
    if_unlinked: IfUnlinked,
    open: impl Fn() -> io::Result<T>,
    admit: impl Fn(&T, Linkage) -> io::Result<()>,
    is_unlinked: impl Fn(&T) -> bool,
    pause: impl FnMut(Duration),
) -> io::Result<T> {
    reopen_while_replaced(
        || {
            let opened = open()?;
            match admit(&opened, if_unlinked.linkage()) {
                Ok(()) => Ok(Attempt::Done(opened)),
                Err(_) if if_unlinked == IfUnlinked::Reopen && is_unlinked(&opened) => {
                    Ok(Attempt::Replaced)
                }
                Err(error) => Err(error),
            }
        },
        pause,
        "file was replaced faster than it could be opened",
    )
}

#[cfg(test)]
mod tests {
    use super::{IfUnlinked, Linkage, open_admitted};
    use crate::retry::BACKOFF;
    use std::cell::Cell;
    use std::io;
    use std::time::Duration;

    #[test]
    fn linkage_admits_exactly_the_link_counts_its_policy_names() {
        assert!(Linkage::Named.admits(1));
        assert!(!Linkage::Named.admits(0), "a writer needs the live entry");
        assert!(!Linkage::Named.admits(2), "an alias could mutate the bytes");
        assert!(Linkage::NamedOrUnlinked.admits(1));
        assert!(
            Linkage::NamedOrUnlinked.admits(0),
            "a reader keeps its generation"
        );
        assert!(!Linkage::NamedOrUnlinked.admits(2));
        assert_eq!(IfUnlinked::Keep.linkage(), Linkage::NamedOrUnlinked);
        assert_eq!(IfUnlinked::Reopen.linkage(), Linkage::Named);
    }

    /// An object whose link count is read from the `links` cell, as a publisher would change it.
    struct Generation {
        links: Cell<u64>,
        number: usize,
    }

    /// A publisher that replaces the name at every open, `replacements` times: the object each
    /// open returns is already unlinked by the time its admission runs.
    fn open_while_published(
        if_unlinked: IfUnlinked,
        replacements: usize,
    ) -> (io::Result<usize>, usize, Vec<Duration>) {
        let opens = Cell::new(0_usize);
        let mut pauses = Vec::new();
        let outcome = open_admitted(
            if_unlinked,
            || {
                let number = opens.get();
                opens.set(number + 1);
                // The publisher replaces the name right after this open returns, until it is
                // out of replacements; the last generation stays linked.
                let linked = number >= replacements;
                Ok(Generation {
                    links: Cell::new(u64::from(linked)),
                    number,
                })
            },
            |generation, linkage| {
                if linkage.admits(generation.links.get()) {
                    Ok(())
                } else {
                    Err(io::Error::from(io::ErrorKind::PermissionDenied))
                }
            },
            |generation| generation.links.get() == 0,
            |delay| pauses.push(delay),
        )
        .map(|generation| generation.number);
        (outcome, opens.get(), pauses)
    }

    #[test]
    fn a_reader_settles_on_the_first_open_however_often_the_name_is_replaced() {
        let (outcome, opens, pauses) = open_while_published(IfUnlinked::Keep, usize::MAX);
        assert_eq!(outcome.expect("the opened generation is kept"), 0);
        assert_eq!(opens, 1);
        assert!(pauses.is_empty(), "a reader never waits for a publisher");
    }

    #[test]
    fn a_writer_reopens_with_pauses_until_the_name_settles() {
        let (outcome, opens, pauses) = open_while_published(IfUnlinked::Reopen, 3);
        assert_eq!(outcome.expect("the writer settles"), 3);
        assert_eq!(opens, 4);
        assert_eq!(pauses, BACKOFF[..3], "one scheduled pause per lost race");
    }

    #[test]
    fn a_writer_gives_up_busy_on_a_name_that_never_settles() {
        let (outcome, opens, pauses) = open_while_published(IfUnlinked::Reopen, usize::MAX);
        let error = outcome.expect_err("a name replaced at every open never settles");
        assert_eq!(error.kind(), io::ErrorKind::ResourceBusy);
        assert_eq!(opens, BACKOFF.len() + 1);
        assert_eq!(pauses, BACKOFF);
    }

    #[test]
    fn a_refusal_that_is_not_a_lost_race_is_returned_at_once() {
        let opens = Cell::new(0_usize);
        let error = open_admitted(
            IfUnlinked::Reopen,
            || {
                opens.set(opens.get() + 1);
                Ok(2_u64)
            },
            |links, linkage| {
                if linkage.admits(*links) {
                    Ok(())
                } else {
                    Err(io::Error::from(io::ErrorKind::PermissionDenied))
                }
            },
            |links| *links == 0,
            |_| panic!("an aliased file is not waited on"),
        )
        .expect_err("two names reach the file");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(opens.get(), 1);
    }
}
