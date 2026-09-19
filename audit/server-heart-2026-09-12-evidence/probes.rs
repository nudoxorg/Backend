fn main() {}

#[cfg(test)]
mod tests {
    use compiler_ir::EntityId;
    use heart_identity::{
        ArtifactId, ContentId, GenerationId, IrFragmentDomain, IrFragmentEncoding, ObjectDomain,
    };
    use heart_memory::{MemoryStore, StoreCapacity};
    use heart_object::ObjectRef;
    use heart_schema::SchemaId;
    use server_index_core::*;
    use server_index_graph_vector::{
        Metric, ModelId, PartitionId, ValidatedVectorSegment, VectorAuthority, VectorPoint,
    };
    use server_index_qdrant::QdrantBlockingAdapter;
    use std::{
        cell::Cell,
        io::{Read, Write},
        net::TcpListener,
        rc::Rc,
    };

    fn doc(entity: u32) -> EntityDocumentId {
        EntityDocumentId {
            artifact: EntityArtifactIdentity::Compact(ArtifactId::<
                IrFragmentEncoding,
                IrFragmentDomain,
            >::from_encoded_bytes(b"audit")),
            entity: EntityId::new(entity),
        }
    }

    #[derive(Debug)]
    struct Switch(Rc<Cell<bool>>);
    impl AsRef<[u8]> for Switch {
        fn as_ref(&self) -> &[u8] {
            if self.0.get() { b"evil" } else { b"good" }
        }
    }

    #[test]
    fn admitted_content_can_change_through_safe_as_ref() {
        let switch = Rc::new(Cell::new(false));
        let reference = ObjectRef::<ObjectDomain> {
            content: ContentId::from_canonical_bytes(b"good"),
            length: 4_u64.into(),
            schema: SchemaId::Object,
            kind: 1_u16.into(),
        };
        let mut store = MemoryStore::<ObjectDomain, Switch>::new(StoreCapacity {
            bytes: 4_u64.into(),
            slots: 1_u32.into(),
        })
        .unwrap();
        store
            .insert_owned(reference, Switch(switch.clone()))
            .unwrap();
        switch.set(true);
        let stored = store.get(reference.content).unwrap();
        assert_eq!(stored.bytes, b"evil");
        assert_ne!(
            ContentId::<ObjectDomain>::from_canonical_bytes(stored.bytes),
            stored.reference.content
        );
    }

    #[test]
    fn production_weight_makes_all_proper_prefix_scores_zero() {
        let rows = [
            LexicalRow::new(b"map", doc(1), 1.into()),
            LexicalRow::new(b"map_or_else", doc(2), 1.into()),
        ];
        let segment = LexicalSegment::new(&rows).unwrap();
        let mut output = [LexicalHit::new(b"", doc(0), 0.into()); 2];
        assert_eq!(
            segment
                .rank(
                    LexicalOperation::prefix(b"ma"),
                    LexicalTopK::new(2).unwrap(),
                    &mut output
                )
                .unwrap(),
            2
        );
        assert_eq!(output.map(|hit| u32::from(hit.score)), [0, 0]);
    }

    #[test]
    fn a_tombstoned_alias_hides_a_live_prefix_match() {
        let rows = [
            LexicalRow::tombstone(b"ma", doc(1)),
            LexicalRow::new(b"map", doc(1), 100.into()),
        ];
        let segment = LexicalSegment::new(&rows).unwrap();
        let ids = [segment.id];
        let segments = [segment];
        let snapshot =
            IndexSnapshot::new(GenerationId::from_canonical_bytes(b"audit"), &[], &ids).unwrap();
        let manifest = LexicalManifest::new(snapshot, &segments, &[]).unwrap();
        let mut scratch = [None; 2];
        let mut output = [LexicalSnapshotHit::new(segment.id, b"", doc(0), 0.into()); 1];
        let result = manifest
            .execute(
                LexicalOperation::prefix(b"m"),
                LexicalTopK::new(1).unwrap(),
                &mut scratch,
                &mut output,
            )
            .unwrap();
        assert!(matches!(result, LexicalTerminal::Complete { hits, .. } if hits.is_empty()));
        let result = manifest
            .execute(
                LexicalOperation::new(b"map"),
                LexicalTopK::new(1).unwrap(),
                &mut scratch,
                &mut output,
            )
            .unwrap();
        assert!(matches!(result, LexicalTerminal::Complete { hits, .. } if hits.len() == 1));
    }

    #[test]
    fn nonempty_qdrant_selection_accepts_empty_projection() {
        let authority = VectorAuthority::new(
            IndexSnapshotId::from_canonical_bytes(b"audit"),
            ModelId::new([1; 16]),
            2,
            Metric::SquaredEuclidean,
        );
        let coordinates = [1_i16, 2];
        let points = [VectorPoint::new(EntityId::new(1), &coordinates)];
        let segment =
            ValidatedVectorSegment::try_new(authority, PartitionId::new(1), &points).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let thread = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut request = vec![];
            loop {
                let mut bytes = [0; 4096];
                let read = socket.read(&mut bytes).unwrap();
                assert!(read > 0);
                request.extend_from_slice(&bytes[..read]);
                if let Some(start) = request.windows(4).position(|s| s == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&request[..start]);
                    let length: usize = header
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .map(str::to_owned)
                        })
                        .unwrap()
                        .parse()
                        .unwrap();
                    if request.len() >= start + 4 + length {
                        break;
                    }
                }
            }
            let body = r#"{"result":{"points":[]},"status":"ok"}"#;
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
        });
        let adapter = QdrantBlockingAdapter::new(&endpoint, "audit", authority).unwrap();
        let mut output = [None; 1];
        let count = adapter
            .query(&[segment.descriptor()], &[0, 0], 1, &mut output)
            .unwrap();
        assert_eq!(count.count, 0);
        thread.join().unwrap();
    }

    #[test]
    fn manifest_advances_eventually_exhaust_unreclaimable_residency() {
        use heart_identity::{IndexExactSegmentDomain, derive_index_snapshot};
        use interface_core::{
            ClientIndex, ClientManifest, ClientSyncError, ManifestEpoch, ManifestSegment,
            RemoteGeneration, RemoteManifest, ResidentRange, SegmentId, SegmentRange,
            SyncCancellation, SyncTerminal,
        };
        use std::num::NonZeroU64;
        let mut client = ClientIndex::new();
        let mut previous = None;
        for epoch in 1_u64..=257 {
            let generation = GenerationId::from_canonical_bytes(&epoch.to_le_bytes());
            let exact =
                ContentId::<IndexExactSegmentDomain>::from_canonical_bytes(&epoch.to_le_bytes());
            let snapshot = derive_index_snapshot(generation, &[exact], &[]).unwrap();
            let remote = RemoteGeneration::new(generation, snapshot);
            let manifest = ClientManifest::from_lanes(
                remote,
                ManifestEpoch::new(NonZeroU64::new(epoch).unwrap()),
                vec![ManifestSegment::exact(exact, NonZeroU64::new(32).unwrap())]
                    .into_boxed_slice(),
                Box::new([]),
            )
            .unwrap();
            let incoming = if let Some(previous) = previous {
                RemoteManifest::advance(previous, manifest)
            } else {
                RemoteManifest::initial(manifest)
            };
            assert!(matches!(
                client.accept_manifest(incoming, SyncCancellation::Continue),
                SyncTerminal::AcceptedInitial { .. } | SyncTerminal::AcceptedAdvance { .. }
            ));
            previous = Some(remote);
            let result = client.record_resident_range(ResidentRange::new(
                SegmentId::Exact(exact),
                SegmentRange::new(0, NonZeroU64::new(32).unwrap()).unwrap(),
            ));
            if epoch <= 256 {
                result.unwrap();
            } else {
                assert!(matches!(
                    result,
                    Err(ClientSyncError::ResidenceCapacity { .. })
                ));
            }
        }
    }

    #[test]
    fn oversized_work_waits_forever_even_in_empty_runtime() {
        use server_runtime::{ByteBudget, ByteQuantum, RemoteRuntime, RetainedBytes};
        use std::{
            future::Future,
            pin::Pin,
            task::{Context, Waker},
        };
        let budget = ByteBudget::new(
            RetainedBytes::from(4_usize),
            ByteQuantum::try_from(1_usize).unwrap(),
        )
        .unwrap();
        let mut runtime =
            RemoteRuntime::<u8, RetainedBytes>::with_waiter_capacity(1, budget, 1).unwrap();
        let (admission, _owner) = runtime.split();
        let mut future = admission
            .admit_when_ready(1, RetainedBytes::from(5_usize))
            .unwrap();
        assert!(
            Pin::new(&mut future)
                .poll(&mut Context::from_waker(Waker::noop()))
                .is_pending()
        );
        assert!(
            Pin::new(&mut future)
                .poll(&mut Context::from_waker(Waker::noop()))
                .is_pending()
        );
    }

    #[test]
    fn distributed_top_one_disagrees_with_canonical_newest_wins() {
        use server_index_routing::{
            AttemptOrdinal, Coordinator, ObservedHit, OrderingRecipe, Query, RetryPolicy,
            RouteAttempt, RoutedHitSlot, TopK, WorkerId, WorkerReply, merge_replies,
        };
        let new_rows = [
            LexicalRow::new(b"term", doc(1), 1.into()),
            LexicalRow::new(b"term", doc(2), 50.into()),
        ];
        let old_rows = [LexicalRow::new(b"term", doc(1), 100.into())];
        let newer = LexicalSegment::new(&new_rows).unwrap();
        let older = LexicalSegment::new(&old_rows).unwrap();
        let segments = [newer, older];
        let ids = [newer.id, older.id];
        let snapshot =
            IndexSnapshot::new(GenerationId::from_canonical_bytes(b"audit"), &[], &ids).unwrap();
        let manifest = LexicalManifest::new(snapshot, &segments, &[]).unwrap();
        let mut local = [LexicalSnapshotHit::new(newer.id, b"", doc(0), 0.into()); 1];
        let result = manifest
            .execute(
                LexicalOperation::new(b"term"),
                LexicalTopK::new(1).unwrap(),
                &mut [None; 3],
                &mut local,
            )
            .unwrap();
        assert!(
            matches!(result, LexicalTerminal::Complete { hits, .. } if hits[0].document == doc(2))
        );

        let workers = [WorkerId::new(1)];
        let coordinator =
            Coordinator::new(snapshot, &workers, RetryPolicy::new(1).unwrap()).unwrap();
        let plan = coordinator
            .plan(Query::new(b"term", OrderingRecipe::new(1)).with_top_k(TopK::new(1).unwrap()))
            .unwrap();
        let attempt = |ordinal| {
            let assignment = plan.assignment(ordinal).unwrap();
            RouteAttempt {
                route: plan.route(),
                snapshot: plan.snapshot(),
                query: plan.query(),
                segment_ordinal: assignment.ordinal,
                segment: assignment.segment,
                range: assignment.range,
                worker: assignment.candidate(0).unwrap(),
                ordinal: AttemptOrdinal::new(0),
            }
        };
        let newer_top = [ObservedHit {
            segment: newer.id,
            term: b"term",
            document: doc(2),
            score: 50,
        }];
        let older_top = [ObservedHit {
            segment: older.id,
            term: b"term",
            document: doc(1),
            score: 100,
        }];
        let replies = [
            WorkerReply::new(attempt(0), &newer_top),
            WorkerReply::new(attempt(1), &older_top),
        ];
        let mut output = [RoutedHitSlot::vacant(); 1];
        let mut seen = [None; 2];
        let mut missing = [];
        let result = merge_replies(&plan, &replies, &mut seen, &mut missing, &mut output).unwrap();
        assert!(result.missing.is_empty());
        assert_eq!(result.hits[0].get().unwrap().document(), doc(1));
        assert_eq!(result.hits[0].get().unwrap().score(), 100);
    }
}
