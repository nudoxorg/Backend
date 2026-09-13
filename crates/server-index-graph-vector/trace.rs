//! Defines trace behavior for `server-index-graph-vector`, whose purpose is to execute typed graph and vector work through bounded leased storage.
//! This module owns the trace invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use crate::GraphAuthority;

const TRACE_CAPACITY: usize = 8;

/// Coarse typed graph events retained only by an explicitly enabled recorder.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphTraceEvent {
    /// A bounded partition set was admitted.
    Admitted {
        /// Complete graph authority.
        authority: GraphAuthority,
        /// Number of admitted partitions.
        partitions: u8,
    },
    /// One leased batch became consumer-visible.
    BatchReady {
        /// Complete graph authority.
        authority: GraphAuthority,
        /// Number of charged edges.
        edges: u8,
    },
    /// The leased stream reached a typed terminal.
    Terminal {
        /// Complete graph authority.
        authority: GraphAuthority,
        /// True only for cancellation.
        cancelled: bool,
    },
}

/// Caller-owned bounded event storage for an enabled probe.
#[derive(Debug)]
pub struct TraceRecorder {
    events: [Option<GraphTraceEvent>; TRACE_CAPACITY],
    start: usize,
    len: usize,
    dropped: usize,
}

impl TraceRecorder {
    /// Creates empty fixed-capacity storage without heap allocation.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            events: [None; TRACE_CAPACITY],
            start: 0,
            len: 0,
            dropped: 0,
        }
    }

    fn record(&mut self, event: GraphTraceEvent) {
        if self.len < TRACE_CAPACITY {
            let index = (self.start + self.len) % TRACE_CAPACITY;
            self.events[index] = Some(event);
            self.len += 1;
        } else {
            self.events[self.start] = Some(event);
            self.start = (self.start + 1) % TRACE_CAPACITY;
            self.dropped += 1;
        }
    }

    fn drain_into(&mut self, output: &mut [Option<GraphTraceEvent>]) -> usize {
        let count = output.len().min(self.len);
        for slot in output.iter_mut().take(count) {
            *slot = self.events[self.start].take();
            self.start = (self.start + 1) % TRACE_CAPACITY;
            self.len -= 1;
        }
        count
    }
}

impl Default for TraceRecorder {
    fn default() -> Self {
        Self::new()
    }
}

/// Lazy probe borrowing recorder storage only when observation is enabled.
pub struct TraceProbe<'recorder> {
    recorder: Option<&'recorder mut TraceRecorder>,
}

impl<'recorder> TraceProbe<'recorder> {
    /// Creates the zero-retained-event disabled shape.
    #[must_use]
    pub const fn disabled() -> Self {
        Self { recorder: None }
    }

    /// Borrows caller-owned bounded storage for enabled observation.
    #[must_use]
    pub const fn enabled(recorder: &'recorder mut TraceRecorder) -> Self {
        Self {
            recorder: Some(recorder),
        }
    }

    /// Constructs and records an event only when enabled.
    pub fn record_with(&mut self, build: impl FnOnce() -> GraphTraceEvent) {
        if let Some(recorder) = self.recorder.as_deref_mut() {
            recorder.record(build());
        }
    }

    /// Reports the event slots retained by this concrete probe shape.
    #[must_use]
    pub const fn retained_event_capacity(&self) -> usize {
        if self.recorder.is_some() {
            TRACE_CAPACITY
        } else {
            0
        }
    }

    /// Drains chronological events into caller output.
    pub fn drain_into(&mut self, output: &mut [Option<GraphTraceEvent>]) -> usize {
        self.recorder
            .as_deref_mut()
            .map_or(0, |recorder| recorder.drain_into(output))
    }
}
