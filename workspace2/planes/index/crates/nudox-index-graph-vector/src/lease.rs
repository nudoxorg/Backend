use core::{
    mem::size_of,
    pin::Pin,
    sync::atomic::Ordering,
    task::{Context, Poll, Waker},
};

#[cfg(feature = "loom-model")]
use loom::sync::{Mutex, atomic::AtomicBool};
#[cfg(not(feature = "loom-model"))]
use std::sync::{Mutex, atomic::AtomicBool};

use crate::{GraphAuthority, GraphEdge, PartitionId};

const MAX_LEASED_EDGES: usize = 16;

/// Cancellation authority that owns the wake registration for pending work.
#[derive(Debug)]
pub struct Cancellation {
    cancelled: AtomicBool,
    waiter: Mutex<Option<Waker>>,
}

impl Cancellation {
    /// Creates a clear cancellation authority.
    #[must_use]
    pub fn new() -> Self {
        Self {
            cancelled: AtomicBool::new(false),
            waiter: Mutex::new(None),
        }
    }

    /// Publishes cancellation and wakes the registered pending consumer once.
    pub fn cancel(&self) {
        if !self.cancelled.swap(true, Ordering::AcqRel) {
            let wake = match self.waiter.lock() {
                Ok(mut waiter) => waiter.take(),
                Err(poisoned) => poisoned.into_inner().take(),
            };
            if let Some(waker) = wake {
                waker.wake();
            }
        }
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    fn register(&self, waker: &Waker) {
        let mut waiter = match self.waiter.lock() {
            Ok(waiter) => waiter,
            Err(poisoned) => poisoned.into_inner(),
        };
        if waiter
            .as_ref()
            .is_none_or(|current| !current.will_wake(waker))
        {
            *waiter = Some(waker.clone());
        }
    }

    fn clear_registration(&self) {
        let mut waiter = match self.waiter.lock() {
            Ok(waiter) => waiter,
            Err(poisoned) => poisoned.into_inner(),
        };
        *waiter = None;
    }
}

impl Default for Cancellation {
    fn default() -> Self {
        Self::new()
    }
}

/// Exact capacity rejection before a lease is accepted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamCapacityError {
    /// Configured item capacity is invalid.
    InvalidItemCapacity {
        /// Maximum fixed storage supported by this stream.
        maximum: usize,
        /// Rejected requested capacity.
        observed: usize,
    },
    /// Configured byte capacity cannot hold the declared item reserve.
    InvalidByteCapacity {
        /// Bytes needed for the item reserve.
        required: usize,
        /// Rejected requested bytes.
        observed: usize,
    },
    /// A second settled batch would exceed the single bounded completion slot.
    BatchAlreadySettled,
    /// Settled data exceeds the configured item reserve.
    ItemCapacity {
        /// Configured item reserve.
        maximum: usize,
        /// Complete observed item count.
        observed: usize,
    },
    /// Settled data exceeds the configured byte reserve.
    ByteCapacity {
        /// Configured byte reserve.
        maximum: usize,
        /// Complete observed byte charge.
        observed: usize,
    },
    /// One settled edge belongs to another stream authority.
    WrongAuthority {
        /// Rejected edge coordinate.
        edge_index: usize,
        /// Stream authority.
        expected: GraphAuthority,
        /// Rejected authority.
        observed: GraphAuthority,
    },
    /// One settled edge belongs to another partition.
    WrongPartition {
        /// Rejected edge coordinate.
        edge_index: usize,
        /// Completion partition.
        expected: PartitionId,
        /// Rejected edge partition.
        observed: PartitionId,
    },
}

/// Exact short-output rejection; the lending batch remains unchanged.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InsufficientOutput {
    required: usize,
    available: usize,
}

impl InsufficientOutput {
    /// Returns the complete batch size.
    #[must_use]
    pub const fn required(self) -> usize {
        self.required
    }

    /// Returns the supplied destination size.
    #[must_use]
    pub const fn available(self) -> usize {
        self.available
    }
}

/// Terminal facts for the graph leased edge.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphTerminal {
    /// All admitted partitions completed.
    Complete {
        /// Complete graph authority.
        authority: GraphAuthority,
    },
    /// Cancellation won and released every stream-owned charge.
    Cancelled {
        /// Complete graph authority.
        authority: GraphAuthority,
    },
    /// One or more requested partitions were exactly absent.
    Partial {
        /// Complete graph authority.
        authority: GraphAuthority,
        /// Exact absent partition coordinates in semantic order.
        missing: [Option<PartitionId>; crate::MAX_PARTITIONS],
        /// Number of occupied entries in `missing`.
        missing_len: usize,
    },
}

impl GraphTerminal {
    /// Returns the authority retained by every terminal.
    #[must_use]
    pub const fn authority(self) -> GraphAuthority {
        match self {
            Self::Complete { authority }
            | Self::Cancelled { authority }
            | Self::Partial { authority, .. } => authority,
        }
    }
}

/// One runtime-independent leased stream observation.
#[derive(Debug)]
pub enum GraphStreamEvent<'stream> {
    /// A complete settled batch lending the stream's exact owners.
    Batch(LeasedGraphBatch<'stream>),
    /// The exact terminal, observed once.
    Terminal(GraphTerminal),
    /// Polling after terminal performs no registration or work.
    Fused,
}

/// One bounded completion slot and its wake/credit owner.
#[derive(Debug)]
pub struct EdgeBatchStream {
    authority: GraphAuthority,
    item_capacity: usize,
    byte_capacity: usize,
    edges: [Option<GraphEdge>; MAX_LEASED_EDGES],
    len: usize,
    charged_bytes: usize,
    ready: bool,
    terminal: Option<GraphTerminal>,
    terminal_emitted: bool,
    waiter: Option<Waker>,
}

impl EdgeBatchStream {
    /// Creates one fixed-storage stream with explicit item and byte reserves.
    pub fn new(
        authority: GraphAuthority,
        item_capacity: usize,
        byte_capacity: usize,
    ) -> Result<Self, StreamCapacityError> {
        if item_capacity > MAX_LEASED_EDGES {
            return Err(StreamCapacityError::InvalidItemCapacity {
                maximum: MAX_LEASED_EDGES,
                observed: item_capacity,
            });
        }
        let required = item_capacity.saturating_mul(size_of::<GraphEdge>());
        if byte_capacity < required {
            return Err(StreamCapacityError::InvalidByteCapacity {
                required,
                observed: byte_capacity,
            });
        }
        Ok(Self {
            authority,
            item_capacity,
            byte_capacity,
            edges: [None; MAX_LEASED_EDGES],
            len: 0,
            charged_bytes: 0,
            ready: false,
            terminal: None,
            terminal_emitted: false,
            waiter: None,
        })
    }

    /// Transfers one complete settled provider batch into the fixed lease slot.
    pub fn settle(
        &mut self,
        partition: PartitionId,
        edges: &[GraphEdge],
    ) -> Result<(), StreamCapacityError> {
        if self.ready || self.len != 0 {
            return Err(StreamCapacityError::BatchAlreadySettled);
        }
        if edges.len() > self.item_capacity {
            return Err(StreamCapacityError::ItemCapacity {
                maximum: self.item_capacity,
                observed: edges.len(),
            });
        }
        let observed_bytes = edges.len().saturating_mul(size_of::<GraphEdge>());
        if observed_bytes > self.byte_capacity {
            return Err(StreamCapacityError::ByteCapacity {
                maximum: self.byte_capacity,
                observed: observed_bytes,
            });
        }
        for (edge_index, edge) in edges.iter().copied().enumerate() {
            if edge.authority() != self.authority {
                return Err(StreamCapacityError::WrongAuthority {
                    edge_index,
                    expected: self.authority,
                    observed: edge.authority(),
                });
            }
            if edge.partition() != partition {
                return Err(StreamCapacityError::WrongPartition {
                    edge_index,
                    expected: partition,
                    observed: edge.partition(),
                });
            }
        }
        for (slot, edge) in self.edges.iter_mut().zip(edges.iter().copied()) {
            *slot = Some(edge);
        }
        self.len = edges.len();
        self.charged_bytes = observed_bytes;
        self.ready = true;
        if let Some(waker) = self.waiter.take() {
            waker.wake();
        }
        Ok(())
    }

    /// Queues complete termination after every settled batch is consumed.
    pub fn finish(&mut self) {
        self.terminal = Some(GraphTerminal::Complete {
            authority: self.authority,
        });
        if let Some(waker) = self.waiter.take() {
            waker.wake();
        }
    }

    /// Polls the bounded completion slot with register-before-pending and recheck.
    pub fn poll_batch<'stream>(
        self: Pin<&'stream mut Self>,
        context: &mut Context<'_>,
        cancellation: &Cancellation,
    ) -> Poll<GraphStreamEvent<'stream>> {
        let stream = self.get_mut();
        if stream.terminal_emitted {
            return Poll::Ready(GraphStreamEvent::Fused);
        }
        if cancellation.is_cancelled() {
            stream.release_batch();
            stream.terminal = None;
            stream.terminal_emitted = true;
            cancellation.clear_registration();
            return Poll::Ready(GraphStreamEvent::Terminal(GraphTerminal::Cancelled {
                authority: stream.authority,
            }));
        }
        if stream.ready {
            cancellation.clear_registration();
            return Poll::Ready(GraphStreamEvent::Batch(LeasedGraphBatch { stream }));
        }
        if let Some(terminal) = stream.terminal.take() {
            stream.terminal_emitted = true;
            cancellation.clear_registration();
            return Poll::Ready(GraphStreamEvent::Terminal(terminal));
        }
        stream.waiter = Some(context.waker().clone());
        cancellation.register(context.waker());
        if cancellation.is_cancelled() {
            stream.waiter = None;
            stream.release_batch();
            stream.terminal_emitted = true;
            cancellation.clear_registration();
            return Poll::Ready(GraphStreamEvent::Terminal(GraphTerminal::Cancelled {
                authority: stream.authority,
            }));
        }
        Poll::Pending
    }

    /// Reports currently charged item slots.
    #[must_use]
    pub const fn charged_items(&self) -> usize {
        self.len
    }

    /// Reports currently charged bytes.
    #[must_use]
    pub const fn charged_bytes(&self) -> usize {
        self.charged_bytes
    }

    fn release_batch(&mut self) {
        for slot in self.edges.iter_mut().take(self.len) {
            *slot = None;
        }
        self.len = 0;
        self.charged_bytes = 0;
        self.ready = false;
    }
}

/// Exclusive lending authority over one complete settled provider batch.
#[derive(Debug)]
pub struct LeasedGraphBatch<'stream> {
    stream: &'stream mut EdgeBatchStream,
}

impl LeasedGraphBatch<'_> {
    /// Returns the complete retained batch size.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.stream.len
    }

    /// Returns true only for an empty provider batch.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.stream.len == 0
    }

    /// Copies only after proving the destination can retain every leased edge.
    pub fn copy_into(&mut self, output: &mut [GraphEdge]) -> Result<usize, InsufficientOutput> {
        let required = self.stream.len;
        if output.len() < required {
            return Err(InsufficientOutput {
                required,
                available: output.len(),
            });
        }
        for (destination, source) in output
            .iter_mut()
            .zip(self.stream.edges.iter())
            .take(required)
        {
            if let Some(edge) = source {
                *destination = *edge;
            }
        }
        self.stream.release_batch();
        Ok(required)
    }
}
