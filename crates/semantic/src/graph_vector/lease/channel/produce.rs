//! Polled production of leased graph-edge batches.

use super::*;

impl EdgeBatchProducer<'_> {
    /// Registers for a returned partition slot, then rechecks before pending.
    pub fn poll_ready(
        &mut self,
        context: &mut Context<'_>,
    ) -> Poll<Result<(), StreamCapacityError>> {
        if self.closed() {
            return Poll::Ready(Err(StreamCapacityError::StreamClosed));
        }
        if self.has_vacant_slot() {
            return Poll::Ready(Ok(()));
        }
        self.shared.producer_wake.register(context.waker());
        if self.closed() {
            self.shared.producer_wake.take();
            Poll::Ready(Err(StreamCapacityError::StreamClosed))
        } else if self.has_vacant_slot() {
            self.shared.producer_wake.take();
            Poll::Ready(Ok(()))
        } else {
            Poll::Pending
        }
    }

    /// Publishes one selected partition exactly once. Writing -> Ready is its linearization point.
    pub fn settle(
        &mut self,
        partition: PartitionId,
        edges: &[GraphEdge],
    ) -> Result<(), StreamCapacityError> {
        if self.closed() {
            return Err(StreamCapacityError::StreamClosed);
        }
        let Some(index) = self.shared.selected_index(partition) else {
            return Err(StreamCapacityError::UnselectedPartition {
                observed: partition,
            });
        };
        if self.delivered[index] {
            return Err(StreamCapacityError::PartitionAlreadySettled { partition });
        }
        self.validate(partition, edges)?;
        let slot = &self.shared.slots[index];
        if !slot.try_begin_write() {
            return Err(StreamCapacityError::StreamClosed);
        }
        if !slot.publish(edges) {
            return Err(StreamCapacityError::StreamClosed);
        }
        self.delivered[index] = true;
        self.shared.consumer_wake.wake();
        Ok(())
    }

    /// Publishes the terminal derived from exact partition accounting.
    pub fn finish(&mut self) -> Result<(), StreamCapacityError> {
        self.publish_terminal(self.accounted_terminal())
    }

    /// Publishes only when declared absence equals exact partition accounting.
    pub fn finish_partial(&mut self, missing: &[PartitionId]) -> Result<(), StreamCapacityError> {
        self.publish_terminal(self.checked_accounted_terminal(missing)?)
    }

    /// Publishes a snapshot-authoritative degraded terminal with exact partition accounting.
    pub fn finish_degraded(
        &mut self,
        missing: &[PartitionId],
        reason: GraphDegradation,
    ) -> Result<(), StreamCapacityError> {
        let terminal = match self.checked_accounted_terminal(missing)? {
            GraphTerminal::Complete { authority } => GraphTerminal::Degraded { authority, reason },
            GraphTerminal::Partial { authority, missing } => GraphTerminal::DegradedPartial {
                authority,
                missing,
                reason,
            },
            GraphTerminal::Failed { authority, cause } => {
                GraphTerminal::Failed { authority, cause }
            }
            GraphTerminal::Cancelled { authority }
            | GraphTerminal::Degraded { authority, .. }
            | GraphTerminal::DegradedPartial { authority, .. } => GraphTerminal::Failed {
                authority,
                cause: StreamCapacityError::CorruptState {
                    cell: LeaseStateCell::Terminal,
                },
            },
        };
        self.publish_terminal(terminal)
    }

    fn checked_accounted_terminal(
        &self,
        missing: &[PartitionId],
    ) -> Result<GraphTerminal, StreamCapacityError> {
        if missing.len() > self.shared.selected_len {
            return Err(StreamCapacityError::MissingPartitionCapacity {
                maximum: self.shared.selected_len,
                observed: missing.len(),
            });
        }
        for (index, partition) in missing.iter().copied().enumerate() {
            if self.shared.selected_index(partition).is_none() {
                return Err(StreamCapacityError::UnselectedPartition {
                    observed: partition,
                });
            }
            if let Some(first_index) = missing[..index]
                .iter()
                .position(|first| *first == partition)
            {
                return Err(StreamCapacityError::DuplicateMissingPartition {
                    first_index,
                    index,
                    partition,
                });
            }
        }
        let terminal = self.accounted_terminal();
        let expected: &[PartitionId] = match &terminal {
            GraphTerminal::Partial { missing, .. } => missing.as_ref(),
            GraphTerminal::Complete { .. }
            | GraphTerminal::Cancelled { .. }
            | GraphTerminal::Degraded { .. }
            | GraphTerminal::DegradedPartial { .. }
            | GraphTerminal::Failed { .. } => &[],
        };
        if expected != missing {
            let index = expected
                .iter()
                .zip(missing)
                .position(|(expected, observed)| expected != observed)
                .unwrap_or(expected.len().min(missing.len()));
            return Err(StreamCapacityError::IncorrectMissingPartitions {
                expected: expected.get(index).copied(),
                observed: missing.get(index).copied(),
                index,
            });
        }
        Ok(terminal)
    }

    fn has_vacant_slot(&self) -> bool {
        self.shared.slots[..self.shared.selected_len]
            .iter()
            .any(|slot| slot.phase() == SlotPhase::Vacant)
    }

    fn closed(&self) -> bool {
        self.finished
            || self.shared.closed.load(Ordering::Acquire)
            || self.shared.terminal_claimed()
    }

    fn validate(
        &self,
        partition: PartitionId,
        edges: &[GraphEdge],
    ) -> Result<(), StreamCapacityError> {
        let maximum = usize::from(self.shared.capacity.edges_per_partition);
        if edges.len() > maximum {
            return Err(StreamCapacityError::ItemCapacity {
                maximum,
                observed: edges.len(),
            });
        }
        let observed = size_of_val(edges);
        if observed > self.shared.capacity.bytes_per_partition {
            return Err(StreamCapacityError::ByteCapacity {
                maximum: self.shared.capacity.bytes_per_partition,
                observed,
            });
        }
        for (edge_index, edge) in edges.iter().copied().enumerate() {
            if edge.authority != self.shared.authority {
                return Err(StreamCapacityError::WrongAuthority {
                    edge_index,
                    expected: self.shared.authority,
                    observed: edge.authority,
                });
            }
            if edge.partition != partition {
                return Err(StreamCapacityError::WrongPartition {
                    edge_index,
                    expected: partition,
                    observed: edge.partition,
                });
            }
        }
        Ok(())
    }

    fn accounted_terminal(&self) -> GraphTerminal {
        let mut missing = [PartitionId::new(0); MAX_PARTITIONS];
        let mut missing_len = 0;
        for (index, partition) in self.shared.selected[..self.shared.selected_len]
            .iter()
            .copied()
            .enumerate()
        {
            if !self.delivered[index] {
                let Some(partition) = partition else {
                    return GraphTerminal::Failed {
                        authority: self.shared.authority,
                        cause: StreamCapacityError::CorruptState {
                            cell: LeaseStateCell::PartitionSlot,
                        },
                    };
                };
                missing[missing_len] = partition;
                missing_len += 1;
            }
        }
        match MissingPartitions::from_prefix(missing, missing_len) {
            None => GraphTerminal::Complete {
                authority: self.shared.authority,
            },
            Some(missing) => GraphTerminal::Partial {
                authority: self.shared.authority,
                missing,
            },
        }
    }

    fn publish_terminal(&mut self, terminal: GraphTerminal) -> Result<(), StreamCapacityError> {
        if self.closed() || !self.shared.publish_terminal(terminal) {
            return Err(StreamCapacityError::StreamClosed);
        }
        self.finished = true;
        Ok(())
    }
}
