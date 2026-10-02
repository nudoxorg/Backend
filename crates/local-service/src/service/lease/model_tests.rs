//! A random schedule of lease operations, run against the real host and
//! against an independent time-line model written from the rules alone.
//!
//! The model shares no code with the implementation: it keeps plain offsets
//! from the start of the schedule, applies "everything due is gone before a
//! request is served" as one rule, and states each operation's effect in terms
//! of those offsets. Any disagreement over what is retained, what succeeded or
//! why a lease was released is a bug in one of them.

#![allow(clippy::expect_used, clippy::panic, clippy::too_many_lines)]

use super::limits::SubscriptionLeaseLimits;
use super::state::ReleaseReason;
use super::tests::{Harness, reset_root, term};
use crate::protocol::ProtocolError;
use backend_client::lease_contract::PUBLICATION_LEASE;
use backend_engine::{
    Cursor, CursorResetReason, LocalSubscriptionId, LocalSubscriptionOperation,
    LocalSubscriptionResponse, SubscriptionReply,
};
use std::collections::BTreeSet;
use std::time::Duration;

/// Small, fixed limits so every bound is reachable in a short schedule.
const MAX_ACTIVE: usize = 5;
const MAX_PAGES: u16 = 4;
const WINDOW: Duration = Duration::from_secs(12);

/// How much of the rule space a run actually visited, so a schedule that
/// never reaches a rule cannot pass for having checked it.
#[derive(Clone, Copy, Debug, Default)]
struct Reached {
    opened: u32,
    hydrated: u32,
    pages: u32,
    completed_resets: u32,
    stale_pages_refused: u32,
    resumes: u32,
    renewals: u32,
    cancels: u32,
    full_table_refusals: u32,
    refused_on_dead_leases: u32,
}

impl Reached {
    fn add(&mut self, other: Self) {
        self.opened += other.opened;
        self.hydrated += other.hydrated;
        self.pages += other.pages;
        self.completed_resets += other.completed_resets;
        self.stale_pages_refused += other.stale_pages_refused;
        self.resumes += other.resumes;
        self.renewals += other.renewals;
        self.cancels += other.cancels;
        self.full_table_refusals += other.full_table_refusals;
        self.refused_on_dead_leases += other.refused_on_dead_leases;
    }
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: usize) -> usize {
        let bound = u64::try_from(bound).expect("bound");
        usize::try_from(self.next() % bound).expect("index")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Op {
    OpenQuiet,
    OpenReset(usize),
    Resume,
    Renew,
    Ack,
    Cancel,
    Page,
    StalePage,
    Credit,
    /// Resume, Renew or Ack at a cursor the lease does not hold.
    WrongCursor(u8),
    Advance(Duration),
    Poll,
}

fn pick(rng: &mut Rng) -> Op {
    match rng.below(26) {
        0..=2 => Op::OpenQuiet,
        3..=5 => Op::OpenReset(1 + rng.below(6)),
        6 => Op::Resume,
        7 => Op::Renew,
        8 => Op::Ack,
        9 => Op::Cancel,
        10..=14 => Op::Page,
        15 => Op::StalePage,
        16 => Op::Credit,
        17 => Op::WrongCursor(u8::try_from(rng.below(3)).expect("selector")),
        18..=23 => Op::Advance(Duration::from_millis(
            500 * u64::try_from(1 + rng.below(30)).expect("steps"),
        )),
        _ => Op::Poll,
    }
}

/// What the model believes one retained lease holds.
#[derive(Clone, Debug)]
struct Held {
    id: LocalSubscriptionId,
    cursor: Box<[u8]>,
    expires: Duration,
    /// `Some` while a reset is hydrating.
    hydration: Option<Hydration>,
    /// Every continuation token the system has handed out for this lease, the
    /// last being the one currently accepted.
    tokens: Vec<Box<[u8]>>,
}

#[derive(Clone, Debug)]
struct Hydration {
    window_end: Duration,
    /// Pages the budget still allows, counting the one being requested.
    pages_left: u16,
    /// Rows not yet delivered; one row travels per page.
    rows_left: usize,
}

#[derive(Default)]
struct Model {
    now: Duration,
    live: Vec<Held>,
    released: [u64; 6],
    dead: Vec<LocalSubscriptionId>,
}

impl Model {
    fn position(&self, id: LocalSubscriptionId) -> Option<usize> {
        self.live.iter().position(|held| held.id == id)
    }

    fn release(&mut self, index: usize, reason: ReleaseReason) {
        let held = self.live.remove(index);
        self.released[reason.index()] += 1;
        self.dead.push(held.id);
    }

    /// Everything due is gone before any request is served.
    fn reclaim(&mut self) {
        let mut index = 0;
        while index < self.live.len() {
            let held = &self.live[index];
            let reason = if held.expires <= self.now {
                Some(ReleaseReason::Expired)
            } else if held
                .hydration
                .as_ref()
                .is_some_and(|hydration| hydration.window_end <= self.now)
            {
                Some(ReleaseReason::ResetWindowElapsed)
            } else {
                None
            };
            match reason {
                Some(reason) => self.release(index, reason),
                None => index += 1,
            }
        }
    }
}

/// Opens a reset of `labels` and returns the lease, its first continuation
/// and the cursor the reset targets (computed from the root, not read back).
fn open_reset(
    harness: &mut Harness,
    labels: &[&str],
) -> Result<(LocalSubscriptionId, Option<Box<[u8]>>, Box<[u8]>), ProtocolError> {
    let root = reset_root(labels);
    let target = Cursor::for_view_root(&root).encode_control();
    let reply = SubscriptionReply::ResetWithRoot {
        credit: 1,
        cursor: target.clone(),
        root: Box::new(root),
        reason: CursorResetReason::Gap,
    };
    match harness.open_with(reply, 1, PUBLICATION_LEASE.get())? {
        LocalSubscriptionResponse::SnapshotPage { lease, next, .. } => Ok((lease, next, target)),
        other => panic!("a reset open answers a page: {other:?}"),
    }
}

fn run(seed: u64, steps: usize) -> (Reached, [u64; 6]) {
    let limits = SubscriptionLeaseLimits::new(
        MAX_ACTIVE,
        Duration::from_mins(1),
        usize::from(MAX_PAGES),
        WINDOW,
    )
    .expect("limits");
    let mut harness = Harness::new(limits);
    let quiet = harness.source.owner_cursor_bytes();
    let mut model = Model::default();
    let mut reached = Reached::default();
    let mut rng = Rng(seed | 1);
    for step in 0..steps {
        let op = pick(&mut rng);
        let context = format!("seed {seed} step {step} op {op:?}");
        match op {
            Op::Advance(by) => {
                harness.clock.advance(by);
                model.now += by;
                continue;
            }
            Op::Poll => {
                model.reclaim();
                let now = harness.table.now();
                harness.table.reclaim_due(now);
            }
            // The host sweeps at the start of every request it serves.
            _ => model.reclaim(),
        }
        // An operation names a lease the model holds, or one it released.
        let target = if model.live.is_empty() || rng.below(10) == 0 {
            (!model.dead.is_empty()).then(|| model.dead[rng.below(model.dead.len())])
        } else {
            Some(model.live[rng.below(model.live.len())].id)
        };
        let now = model.now;
        match op {
            Op::OpenQuiet => {
                let full = model.live.len() >= MAX_ACTIVE;
                harness.script(SubscriptionReply::Accepted { credit: 64 });
                let result = harness.call(LocalSubscriptionOperation::Open {
                    cursor: Box::new([]),
                    credit: 64,
                    lease_ms: PUBLICATION_LEASE.get(),
                });
                assert_eq!(result.is_ok(), !full, "{context}");
                reached.full_table_refusals += u32::from(full);
                match result {
                    Ok(LocalSubscriptionResponse::Opened { lease, cursor, .. }) => {
                        reached.opened += 1;
                        model.live.push(Held {
                            id: lease,
                            cursor,
                            expires: now + term(),
                            hydration: None,
                            tokens: Vec::new(),
                        });
                    }
                    Ok(other) => panic!("{context}: {other:?}"),
                    Err(_) => {
                        // Refused before the daemon was asked: the scripted
                        // reply is still queued.
                        harness.source.replies.pop_back();
                    }
                }
            }
            Op::OpenReset(rows) => {
                let labels: Vec<String> = (0..rows)
                    .map(|n| format!("row-{seed}-{step}-{n}"))
                    .collect();
                let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
                let full = model.live.len() >= MAX_ACTIVE;
                let result = open_reset(&mut harness, &labels);
                assert_eq!(result.is_ok(), !full, "{context}");
                match result {
                    Ok((id, token, cursor)) => {
                        reached.opened += 1;
                        reached.hydrated += u32::from(token.is_some());
                        let hydration = token.is_some().then(|| Hydration {
                            window_end: now + WINDOW,
                            pages_left: MAX_PAGES - 1,
                            rows_left: rows - 1,
                        });
                        model.live.push(Held {
                            id,
                            cursor,
                            expires: now + term(),
                            hydration,
                            tokens: token.into_iter().collect(),
                        });
                    }
                    Err(_) => {
                        harness.source.replies.pop_back();
                    }
                }
            }
            Op::Page | Op::StalePage => {
                let Some(id) = target else { continue };
                let held = model.position(id);
                let stale = op == Op::StalePage
                    && held.is_some_and(|index| model.live[index].tokens.len() > 1);
                let token: Box<[u8]> = match held {
                    Some(index) if stale => model.live[index].tokens[0].clone(),
                    Some(index) => model.live[index]
                        .tokens
                        .last()
                        .cloned()
                        .unwrap_or_else(|| Box::from(b"none".as_slice())),
                    None => Box::from(b"token".as_slice()),
                };
                let result = harness.page(id, &token);
                let hydration = held
                    .filter(|_| !stale)
                    .and_then(|index| model.live[index].hydration.clone());
                let (Some(index), Some(hydration)) = (held, hydration) else {
                    assert!(result.is_err(), "{context}");
                    reached.stale_pages_refused += u32::from(stale);
                    reached.refused_on_dead_leases += u32::from(held.is_none());
                    continue;
                };
                let more = hydration.rows_left > 1;
                if hydration.pages_left <= 1 && more {
                    assert_eq!(
                        result.err(),
                        Some(ProtocolError::ResetPageBudgetExhausted),
                        "{context}"
                    );
                    model.release(index, ReleaseReason::ResetPagesExhausted);
                    continue;
                }
                let Ok(LocalSubscriptionResponse::SnapshotPage { next, .. }) = result else {
                    panic!("{context}: a valid page must be served");
                };
                assert_eq!(next.is_some(), more, "{context}");
                reached.pages += 1;
                reached.completed_resets += u32::from(!more);
                let held = &mut model.live[index];
                held.expires = now + term();
                match next {
                    Some(next) => {
                        held.tokens.push(next);
                        let hydration = held.hydration.as_mut().expect("still hydrating");
                        hydration.pages_left -= 1;
                        hydration.rows_left -= 1;
                    }
                    None => {
                        held.hydration = None;
                        held.tokens.clear();
                    }
                }
            }
            Op::Cancel => {
                let Some(id) = target else { continue };
                let held = model.position(id);
                assert_eq!(harness.cancel(id).is_ok(), held.is_some(), "{context}");
                reached.cancels += u32::from(held.is_some());
                if let Some(index) = held {
                    model.release(index, ReleaseReason::Cancelled);
                }
            }
            Op::Credit => {
                let Some(id) = target else { continue };
                let usable = model
                    .position(id)
                    .is_some_and(|index| model.live[index].hydration.is_none());
                let result = harness.call(LocalSubscriptionOperation::Credit {
                    lease: id,
                    credit: 1,
                });
                assert_eq!(result.is_ok(), usable, "{context}");
            }
            Op::Ack | Op::Renew | Op::Resume | Op::WrongCursor(_) => {
                let Some(id) = target else { continue };
                let held = model.position(id);
                let exact = !matches!(op, Op::WrongCursor(_));
                let cursor: Box<[u8]> = match held {
                    Some(index) if exact => model.live[index].cursor.clone(),
                    _ => Box::from(b"not-this-cursor".as_slice()),
                };
                // Which of the three cursor-fenced operations this is.
                let which = match op {
                    Op::Ack => 0,
                    Op::Renew => 1,
                    Op::Resume => 2,
                    Op::WrongCursor(which) => which,
                    _ => unreachable!("matched above"),
                };
                let accepted =
                    held.is_some_and(|index| model.live[index].hydration.is_none()) && exact;
                let result = match which {
                    0 => harness.ack(id, &cursor),
                    1 => harness.renew(id, &cursor),
                    _ => {
                        harness.script(SubscriptionReply::Accepted { credit: 64 });
                        let result = harness.resume(id, &cursor);
                        if result.is_err() {
                            harness.source.replies.pop_back();
                        }
                        result
                    }
                };
                assert_eq!(result.is_ok(), accepted, "{context}");
                if let (true, Some(index)) = (accepted, held) {
                    let held = &mut model.live[index];
                    match which {
                        0 => {}
                        1 => {
                            reached.renewals += 1;
                            held.expires = now + term();
                        }
                        _ => {
                            reached.resumes += 1;
                            held.expires = now + term();
                            held.cursor = quiet.clone();
                        }
                    }
                }
            }
            Op::Advance(_) | Op::Poll => {}
        }
        let actual: BTreeSet<LocalSubscriptionId> = harness.table.ids().into_iter().collect();
        let expected: BTreeSet<LocalSubscriptionId> =
            model.live.iter().map(|held| held.id).collect();
        assert_eq!(actual, expected, "{context}: retained leases differ");
        for reason in ReleaseReason::ALL {
            assert_eq!(
                harness.released(reason),
                model.released[reason.index()],
                "{context}: releases for {reason:?} differ"
            );
        }
        harness.assert_hint_is_a_lower_bound();
    }
    (reached, model.released)
}

#[test]
fn the_host_and_an_independent_time_line_model_agree_over_random_schedules() {
    let mut reached = Reached::default();
    let mut released = [0_u64; 6];
    for seed in 1..=60_u64 {
        let (seen, counts) = run(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15), 500);
        reached.add(seen);
        for (total, count) in released.iter_mut().zip(counts) {
            *total += count;
        }
    }
    // A model test that never reaches a rule has not tested it.
    for (name, count) in [
        ("opens", reached.opened),
        ("hydrating opens", reached.hydrated),
        ("pages served", reached.pages),
        ("completed resets", reached.completed_resets),
        ("stale pages refused", reached.stale_pages_refused),
        ("resumes", reached.resumes),
        ("renewals", reached.renewals),
        ("cancels", reached.cancels),
        ("refusals at capacity", reached.full_table_refusals),
        (
            "refusals on released leases",
            reached.refused_on_dead_leases,
        ),
    ] {
        assert!(
            count >= 50,
            "the schedules reached {name} only {count} times"
        );
    }
    for reason in [
        ReleaseReason::Cancelled,
        ReleaseReason::Expired,
        ReleaseReason::ResetWindowElapsed,
        ReleaseReason::ResetPagesExhausted,
    ] {
        assert!(
            released[reason.index()] >= 20,
            "the schedules released for {reason:?} only {} times: {reached:?}",
            released[reason.index()]
        );
    }
}
