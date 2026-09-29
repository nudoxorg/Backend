"""Deterministic, standard-library-only IR residency/materialization model.

This is an experiment harness, not production Rust and not a timing benchmark.
It models the existing V1 exact segment routes (hot / checked delta / pristine)
and a separate bounded exact-generation live-byte cache for evaluating V2
replay. Run its tests with:

    python3 -m unittest docs.prototypes.test_ir_materialization_policy_model

Byte budgets use explicit logical metadata constants. They are deterministic
policy accounting, not Python RSS or a production allocator measurement.
"""

from __future__ import annotations

from collections import OrderedDict
from dataclasses import dataclass
import hashlib
import json
from types import MappingProxyType
from typing import Callable, Mapping


class ModelError(RuntimeError):
    """A modeled integrity, ancestry, or bounded-policy failure."""


class StaleAncestry(ModelError):
    """The requested commit is not reachable from the current named-ref tip."""


class MissingObject(ModelError):
    """A physical object or exact per-generation bridge is unavailable."""


class CorruptObject(ModelError):
    """A physical object no longer matches its content-addressed identity."""


def _hash(domain: bytes, *parts: bytes) -> str:
    digest = hashlib.sha256()
    digest.update(domain)
    digest.update(b"\0")
    for part in parts:
        digest.update(len(part).to_bytes(8, "big"))
        digest.update(part)
    return digest.hexdigest()


@dataclass(frozen=True)
class Segment:
    plane: str
    first_key: bytes
    last_key: bytes
    payload: bytes
    row_count: int = 1

    @property
    def key(self) -> tuple[str, str]:
        return self.plane, _hash(b"semantic-segment.v1", self.plane.encode(), self.payload)

    @property
    def object_id(self) -> str:
        return _hash(b"filestore-segment.v1", self.payload)

    @property
    def descriptor(self) -> tuple[str, str, bytes, bytes, int]:
        return self.plane, self.key[1], self.first_key, self.last_key, len(self.payload)


@dataclass(frozen=True)
class Generation:
    target: str
    commit: str
    build: str
    image_facts: str
    input_claim: str
    segments: tuple[Segment, ...]
    parent: str | None = None
    changed_bytes: int = 0
    changed_actions: int = 0
    verification_tier: str = "standard"
    jumbo_limit: int = 1024

    @property
    def content_root(self) -> str:
        descriptors = tuple(sorted(segment.descriptor for segment in self.segments))
        parts = [self.image_facts.encode()]
        for plane, segment_id, first, last, length in descriptors:
            parts.extend((plane.encode(), segment_id.encode(), first, last, length.to_bytes(8, "big")))
        return _hash(b"typed-content-root.v1", *parts)

    @property
    def generation_root(self) -> str:
        return _hash(
            b"typed-generation-root.v1",
            self.build.encode(),
            self.input_claim.encode(),
            self.content_root.encode(),
        )

    @property
    def closure_id(self) -> str:
        return _hash(b"closure-root.v1", *(value.encode() for value in self.object_ids))

    @property
    def locator_id(self) -> str:
        parts: list[bytes] = []
        for segment in sorted(self.segments, key=lambda item: item.descriptor):
            parts.extend(
                (
                    segment.plane.encode(),
                    segment.key[1].encode(),
                    segment.object_id.encode(),
                    segment.first_key,
                    segment.last_key,
                    len(segment.payload).to_bytes(8, "big"),
                )
            )
        return _hash(b"typed-locator.v1", *parts)

    @property
    def object_ids(self) -> tuple[str, ...]:
        return tuple(sorted({segment.object_id for segment in self.segments}))

    @property
    def exact_key(self) -> tuple[object, ...]:
        # Keep every generation/proof input in equality, not only the content
        # root or key ranges. This key models the minimum safe V2 cache binding.
        return (
            self.target,
            self.commit,
            self.content_root,
            self.generation_root,
            self.closure_id,
            self.locator_id,
            self.verification_tier,
            self.jumbo_limit,
        )

    @property
    def exact_key_bytes(self) -> int:
        return sum(len(str(value).encode()) for value in self.exact_key)


@dataclass
class Work:
    physical_payload_bytes: int = 0
    object_hash_bytes: int = 0
    spool_bytes_written: int = 0
    spool_bytes_read: int = 0
    semantic_verifier_runs: int = 0
    segments_verified: int = 0
    rows_verified: int = 0
    v1_hot_hits: int = 0
    delta_reads: int = 0
    pristine_reads: int = 0
    delta_fallbacks: int = 0
    delta_fanout_fallbacks: int = 0
    delta_hops_considered: int = 0
    delta_actions_considered: int = 0
    delta_changed_bytes_considered: int = 0
    replay_cache_hits: int = 0
    replay_cache_misses: int = 0
    replay_cache_admissions: int = 0
    replay_cache_evictions: int = 0
    replay_admission_rejections: int = 0
    full_cold_replays: int = 0
    ancestry_checks: int = 0
    generation_hot_hits: int = 0
    metadata_checks: int = 0
    gc_pins_acquired: int = 0
    gc_pins_released: int = 0

    def as_dict(self) -> dict[str, int]:
        return {name: getattr(self, name) for name in self.__dataclass_fields__}


class Repository:
    """Content-addressed fixture store with exact closure and ref checks."""

    def __init__(self) -> None:
        self.generations: dict[str, Generation] = {}
        self.closures: dict[str, frozenset[str]] = {}
        self.mappings: dict[str, dict[tuple[str, str], str]] = {}
        self.objects: dict[str, bytes] = {}
        self.refs: dict[tuple[str, str], str] = {}
        self.pins: dict[str, int] = {}
        self.before_final_ancestry: Callable[[], None] | None = None

    def add(self, generation: Generation) -> None:
        if generation.commit in self.generations:
            raise ValueError(f"duplicate commit {generation.commit}")
        if generation.parent is not None and generation.parent not in self.generations:
            raise ValueError(f"unknown parent {generation.parent}")
        self.generations[generation.commit] = generation
        closure: set[str] = set()
        mappings: dict[tuple[str, str], str] = {}
        for segment in generation.segments:
            closure.add(segment.object_id)
            mappings[segment.key] = segment.object_id
            prior = self.objects.setdefault(segment.object_id, segment.payload)
            if prior != segment.payload:
                raise ValueError("object ID collision in fixture")
        self.closures[generation.commit] = frozenset(closure)
        self.mappings[generation.commit] = mappings

    def set_ref(self, target: str, name: str, tip: str) -> None:
        self.refs[(target, name)] = tip

    def check_ancestry(self, generation: Generation, ref_name: str, work: Work) -> None:
        work.ancestry_checks += 1
        tip = self.refs.get((generation.target, ref_name))
        if tip is None:
            raise StaleAncestry("named ref is missing")
        cursor: str | None = tip
        seen: set[str] = set()
        while cursor is not None and cursor not in seen:
            if cursor == generation.commit:
                return
            seen.add(cursor)
            cursor = self.generations[cursor].parent
        raise StaleAncestry("commit is not reachable from the current ref tip")

    def check_metadata(self, generation: Generation, work: Work) -> None:
        work.metadata_checks += 1
        expected_closure = frozenset(segment.object_id for segment in generation.segments)
        expected_mapping = {segment.key: segment.object_id for segment in generation.segments}
        if self.closures[generation.commit] != expected_closure:
            raise CorruptObject("durable closure metadata differs from the exact generation")
        if self.mappings[generation.commit] != expected_mapping:
            raise CorruptObject("durable locator metadata differs from the exact generation")

    def pin(self, commit: str, work: Work | None = None) -> None:
        self.pins[commit] = self.pins.get(commit, 0) + 1
        if work is not None:
            work.gc_pins_acquired += 1

    def unpin(self, commit: str, work: Work | None = None) -> None:
        count = self.pins.get(commit, 0)
        if count <= 1:
            self.pins.pop(commit, None)
        else:
            self.pins[commit] = count - 1
        if work is not None:
            work.gc_pins_released += 1

    def collect(self) -> set[str]:
        live_commits = set(self.pins)
        for (target, _), tip in self.refs.items():
            cursor: str | None = tip
            while cursor is not None:
                live_commits.add(cursor)
                cursor = self.generations[cursor].parent
        live_objects = {
            object_id
            for commit in live_commits
            for object_id in self.closures[commit]
        }
        removed = set(self.objects) - live_objects
        for object_id in removed:
            del self.objects[object_id]
        return removed

    def remove_mapping(self, commit: str, segment: Segment) -> None:
        self.mappings[commit].pop(segment.key, None)

    def mutate_object(self, object_id: str, payload: bytes) -> None:
        if object_id not in self.objects:
            raise MissingObject(object_id)
        self.objects[object_id] = payload

    @staticmethod
    def _check_object_bytes(object_id: str, payload: bytes) -> None:
        if _hash(b"filestore-segment.v1", payload) != object_id:
            raise CorruptObject(f"object {object_id} failed content identity")

    def read_segment(self, commit: str, segment: Segment, work: Work) -> bytes:
        mapping = self.mappings[commit].get(segment.key)
        if mapping is None or segment.object_id not in self.closures[commit]:
            raise MissingObject(f"missing exact mapping for {commit}/{segment.key}")
        if mapping != segment.object_id:
            raise CorruptObject("segment bridge differs from exact descriptor")
        payload = self.objects.get(mapping)
        if payload is None:
            raise MissingObject(mapping)
        work.physical_payload_bytes += len(payload)
        work.object_hash_bytes += len(payload)
        self._check_object_bytes(mapping, payload)
        if len(payload) != len(segment.payload) or segment.key[1] != _hash(
            b"semantic-segment.v1", segment.plane.encode(), payload
        ):
            raise CorruptObject("physical payload differs from exact semantic segment")
        return memoryview(payload).tobytes()

    def read_closure_object(self, commit: str, object_id: str, work: Work) -> bytes:
        if object_id not in self.closures[commit]:
            raise MissingObject(f"closure {commit} omits {object_id}")
        payload = self.objects.get(object_id)
        if payload is None:
            raise MissingObject(object_id)
        work.physical_payload_bytes += len(payload)
        work.object_hash_bytes += len(payload)
        self._check_object_bytes(object_id, payload)
        return memoryview(payload).tobytes()


def _verify_semantics(generation: Generation, payloads: Mapping[tuple[str, str], bytes], work: Work) -> None:
    work.semantic_verifier_runs += 1
    work.segments_verified += len(generation.segments)
    work.rows_verified += sum(segment.row_count for segment in generation.segments)
    expected_keys = {segment.key for segment in generation.segments}
    if set(payloads) != expected_keys:
        raise CorruptObject("materialized descriptor set differs from the manifest")
    for segment in generation.segments:
        payload = payloads[segment.key]
        if len(payload) != len(segment.payload) or segment.key[1] != _hash(
            b"semantic-segment.v1", segment.plane.encode(), payload
        ):
            raise CorruptObject("semantic verifier rejected a row segment")
    actual_content = _content_root(generation, payloads)
    if actual_content != generation.content_root:
        raise CorruptObject("semantic content root differs from the commit")
    expected_generation = _hash(
        b"typed-generation-root.v1",
        generation.build.encode(),
        generation.input_claim.encode(),
        actual_content.encode(),
    )
    if expected_generation != generation.generation_root:
        raise CorruptObject("semantic generation root differs from the commit")


def _content_root(generation: Generation, payloads: Mapping[tuple[str, str], bytes]) -> str:
    parts = [generation.image_facts.encode()]
    for segment in sorted(generation.segments, key=lambda item: item.descriptor):
        payload = payloads[segment.key]
        parts.extend(
            (
                segment.plane.encode(),
                _hash(b"semantic-segment.v1", segment.plane.encode(), payload).encode(),
                segment.first_key,
                segment.last_key,
                len(payload).to_bytes(8, "big"),
            )
        )
    return _hash(b"typed-content-root.v1", *parts)


class ColdV2Replay:
    """Cold oracle: verify physical objects, spool, then verify all semantics."""

    def __init__(self, repository: Repository) -> None:
        self.repository = repository
        self.work = Work()

    def replay(self, commit: str, ref_name: str) -> "ReplayLease":
        generation = self.repository.generations[commit]
        self.repository.pin(commit, self.work)
        try:
            self.repository.check_ancestry(generation, ref_name, self.work)
            self._cold_verify(generation)
            if self.repository.before_final_ancestry is not None:
                hook, self.repository.before_final_ancestry = self.repository.before_final_ancestry, None
                hook()
            self.repository.check_ancestry(generation, ref_name, self.work)
            self.repository.check_metadata(generation, self.work)
            return ReplayLease(self.repository, generation, None, None, self.work)
        except Exception:
            self.repository.unpin(commit, self.work)
            raise

    def _cold_verify(self, generation: Generation) -> None:
        self.work.full_cold_replays += 1
        expected_closure = frozenset(segment.object_id for segment in generation.segments)
        self.repository.check_metadata(generation, self.work)
        if expected_closure != self.repository.closures[generation.commit]:
            raise CorruptObject("closure is not the exact locator object set")
        spool: dict[str, bytes] = {}
        for object_id in sorted(expected_closure):
            verified = self.repository.read_closure_object(generation.commit, object_id, self.work)
            # The real V2 path reads the verified ArtifactObjectReader again to
            # spool payload bytes. Count that independent source read here.
            copied = self.repository.read_closure_object(generation.commit, object_id, self.work)
            if copied != verified:
                raise CorruptObject("object changed between verification and spool")
            spool[object_id] = copied
            self.work.spool_bytes_written += len(copied)
        self.work.spool_bytes_read += sum(len(payload) for payload in spool.values())
        payloads = {
            segment.key: spool[segment.object_id]
            for segment in generation.segments
        }
        _verify_semantics(generation, payloads, self.work)


class V1RouteBaseline:
    """Small deterministic model of V1's exact segment source selection.

    Production has richer cost accounting and TinyLFU replacement. This oracle
    models source choices and hard route bounds only; it is not a timing model.
    """

    def __init__(
        self,
        repository: Repository,
        *,
        hot_bytes: int,
        warm_uses: int = 2,
        observation_entries: int = 64,
        max_hops: int = 4,
        max_changed_bytes: int = 8 * 1024 * 1024,
        max_actions: int = 20_000,
    ) -> None:
        self.repository = repository
        self.hot_bytes_limit = hot_bytes
        self.warm_uses = warm_uses
        self.observation_entries = observation_entries
        self.max_hops = max_hops
        self.max_changed_bytes = max_changed_bytes
        self.max_actions = max_actions
        self.hot: OrderedDict[
            tuple[str, str], tuple[tuple[str, str, bytes, bytes, int], bytes]
        ] = OrderedDict()
        self.hot_bytes = 0
        self.observations: OrderedDict[tuple[str, str], int] = OrderedDict()
        self.work = Work()

    def read(self, commit: str, segment: Segment) -> bytes:
        generation = self.repository.generations[commit]
        if not any(candidate.descriptor == segment.descriptor for candidate in generation.segments):
            raise MissingObject("requested segment is not in the selected target manifest")
        entry = self.hot.get(segment.key)
        if entry is not None:
            descriptor, payload = entry
            if descriptor == segment.descriptor and segment.key[1] == _hash(
                b"semantic-segment.v1", segment.plane.encode(), payload
            ):
                self.hot.move_to_end(segment.key)
                self.work.v1_hot_hits += 1
                return payload
            del self.hot[segment.key]
            self.hot_bytes -= len(payload)

        candidate, hops, changed_bytes, actions = _find_delta_candidate(
            self.repository,
            generation,
            segment,
            self.max_hops,
            self.max_changed_bytes,
            self.max_actions,
        )
        self.work.delta_hops_considered += hops
        self.work.delta_changed_bytes_considered += changed_bytes
        self.work.delta_actions_considered += actions
        if candidate is not None and _modest_delta(segment, hops, changed_bytes, actions, self.max_actions):
            try:
                payload = self.repository.read_segment(candidate.commit, segment, self.work)
                self.work.delta_reads += 1
            except ModelError:
                self.work.delta_fallbacks += 1
                payload = self.repository.read_segment(generation.commit, segment, self.work)
                self.work.pristine_reads += 1
        else:
            if candidate is not None:
                self.work.delta_fanout_fallbacks += 1
            payload = self.repository.read_segment(generation.commit, segment, self.work)
            self.work.pristine_reads += 1
        uses = self.observations.get(segment.key, 0) + 1
        if self.observation_entries:
            self.observations[segment.key] = uses
            self.observations.move_to_end(segment.key)
            while len(self.observations) > self.observation_entries:
                self.observations.popitem(last=False)
        if uses >= self.warm_uses and len(payload) <= self.hot_bytes_limit:
            while self.hot and self.hot_bytes + len(payload) > self.hot_bytes_limit:
                _, (_, evicted) = self.hot.popitem(last=False)
                self.hot_bytes -= len(evicted)
            if self.hot_bytes + len(payload) <= self.hot_bytes_limit:
                self.hot[segment.key] = (segment.descriptor, payload)
                self.hot_bytes += len(payload)
        return payload


def _find_delta_candidate(
    repository: Repository,
    generation: Generation,
    segment: Segment,
    max_hops: int,
    max_changed_bytes: int,
    max_actions: int,
) -> tuple[Generation | None, int, int, int]:
    cursor = generation
    changed_bytes = 0
    actions = 0
    considered_hops = 0
    for hop_index in range(1, max_hops + 1):
        if cursor.parent is None:
            break
        considered_hops = hop_index
        changed_bytes += cursor.changed_bytes
        actions += cursor.changed_actions
        cursor = repository.generations[cursor.parent]
        if changed_bytes > max_changed_bytes or actions > max_actions:
            return None, considered_hops, changed_bytes, actions
        if any(parent_segment.descriptor == segment.descriptor for parent_segment in cursor.segments):
            return cursor, considered_hops, changed_bytes, actions
    return None, considered_hops, changed_bytes, actions


def _modest_delta(
    segment: Segment, hops: int, changed_bytes: int, actions: int, configured_actions: int
) -> bool:
    """The existing V1 cold-route bound before measured costs are available."""
    return (
        hops <= 2
        and actions <= min(configured_actions, 512)
        and changed_bytes <= len(segment.payload) * 4
    )


class FrequencySketch:
    """Fixed-size deterministic aging sketch; estimates only affect admission."""

    ROWS = 4
    WIDTH = 64
    AGE_AFTER = ROWS * WIDTH * 4

    def __init__(self) -> None:
        self.cells = bytearray(self.ROWS * self.WIDTH)
        self.observations = 0
        self.ages = 0

    @property
    def bytes(self) -> int:
        return len(self.cells) + 16

    def _indices(self, key: bytes) -> tuple[int, ...]:
        digest = hashlib.sha256(key).digest()
        return tuple(
            row * self.WIDTH
            + int.from_bytes(hashlib.sha256(bytes((row,)) + digest).digest()[:4], "big")
            % self.WIDTH
            for row in range(self.ROWS)
        )

    def observe(self, key: bytes) -> int:
        self.observations += 1
        for index in self._indices(key):
            self.cells[index] = min(15, self.cells[index] + 1)
        if self.observations >= self.AGE_AFTER:
            self.cells[:] = bytes(value // 2 for value in self.cells)
            self.observations = 0
            self.ages += 1
        return self.estimate(key)

    def estimate(self, key: bytes) -> int:
        return min(self.cells[index] for index in self._indices(key))


@dataclass
class _Ghost:
    key_bytes: int
    uses: int = 1


@dataclass
class _Entry:
    generation: Generation
    payloads: dict[tuple[str, str], bytes]
    footprint: int
    reuse_work: int
    tier: str = "probation"
    pins: int = 0
    last_used: int = 0


class ReplayLease:
    """Returned replay token; a cache-backed lease also pins live bytes."""

    def __init__(
        self,
        repository: Repository,
        generation: Generation,
        policy: "LiveGenerationPolicy | None",
        entry: _Entry | None,
        work: Work,
    ) -> None:
        self.generation = generation
        self.content_root = generation.content_root
        self.generation_root = generation.generation_root
        self._repository = repository
        self._policy = policy
        self._entry = entry
        self._work = work
        self.payloads: Mapping[tuple[str, str], bytes] | None = (
            MappingProxyType(entry.payloads) if entry is not None else None
        )
        self._closed = False
        if entry is not None:
            entry.pins += 1

    def close(self) -> None:
        if self._closed:
            return
        self._closed = True
        if self._entry is not None:
            self._entry.pins -= 1
        self._repository.unpin(self.generation.commit, self._work)

    def __enter__(self) -> "ReplayLease":
        return self

    def __exit__(self, *_: object) -> None:
        self.close()


class LiveGenerationPolicy:
    """Experiment-only exact-generation byte cache with explicit total bounds."""

    ENTRY_BASE_BYTES = 128
    ENTRY_SEGMENT_BYTES = 88
    STAGING_BASE_BYTES = 96
    STAGING_SEGMENT_BYTES = 112
    GHOST_BASE_BYTES = 24

    def __init__(
        self,
        repository: Repository,
        *,
        max_bytes: int,
        max_entries: int,
        ghost_entries: int = 64,
        warm_uses: int = 2,
        probation_percent: int = 25,
        max_hops: int = 4,
        max_changed_bytes: int = 8 * 1024 * 1024,
        max_actions: int = 20_000,
    ) -> None:
        if max_entries < 0 or ghost_entries < 0 or warm_uses < 1:
            raise ValueError("invalid policy count bound")
        if not 0 <= probation_percent <= 100:
            raise ValueError("probation_percent must be between 0 and 100")
        if max_bytes <= FrequencySketch().bytes:
            raise ValueError("max_bytes must fit the fixed frequency sketch")
        self.repository = repository
        self.max_bytes = max_bytes
        self.max_entries = max_entries
        self.ghost_limit = ghost_entries
        self.warm_uses = warm_uses
        self.probation_percent = probation_percent
        self.max_hops = max_hops
        self.max_changed_bytes = max_changed_bytes
        self.max_actions = max_actions
        self.sketch = FrequencySketch()
        self.ghosts: OrderedDict[tuple[object, ...], _Ghost] = OrderedDict()
        self.entries: OrderedDict[tuple[object, ...], _Entry] = OrderedDict()
        self.probation_bytes = 0
        self.protected_bytes = 0
        self.staging_bytes = 0
        self.tick = 0
        self.work = Work()
        self.peak_policy_bytes = self.sketch.bytes

    @property
    def total_bytes(self) -> int:
        return self.sketch.bytes + self.ghost_bytes + self.probation_bytes + self.protected_bytes + self.staging_bytes

    @property
    def ghost_bytes(self) -> int:
        return sum(self.GHOST_BASE_BYTES + ghost.key_bytes for ghost in self.ghosts.values())

    @property
    def entry_count(self) -> int:
        return len(self.entries)

    def materialize(self, commit: str, ref_name: str) -> ReplayLease:
        generation = self.repository.generations[commit]
        self.repository.pin(commit, self.work)
        try:
            self.repository.check_ancestry(generation, ref_name, self.work)
            self.repository.check_metadata(generation, self.work)
        except Exception:
            self.repository.unpin(commit, self.work)
            raise
        key = generation.exact_key
        key_bytes = b"\0".join(str(value).encode() for value in key)
        self.sketch.observe(key_bytes)
        self.tick += 1
        entry = self.entries.get(key)
        if entry is not None:
            self.work.replay_cache_hits += 1
            self.work.generation_hot_hits += 1
            entry.last_used = self.tick
            entry.reuse_work += 1
            self.entries.move_to_end(key)
            self._promote(key, entry)
            try:
                self._final_ancestry_check(generation, ref_name)
                self._record_peak()
                return ReplayLease(self.repository, generation, self, entry, self.work)
            except Exception:
                self.repository.unpin(commit, self.work)
                raise

        self.work.replay_cache_misses += 1
        uses = self._observe_ghost(key, generation.exact_key_bytes)
        work_before = self.work.physical_payload_bytes + self.work.semantic_verifier_runs
        staged_payload_bytes = sum(
            {segment.object_id: len(segment.payload) for segment in generation.segments}.values()
        )
        staged_size = (
            staged_payload_bytes
            + self.STAGING_BASE_BYTES
            + len(generation.segments) * self.STAGING_SEGMENT_BYTES
            + sum(len(segment.plane.encode()) + len(segment.key[1].encode()) for segment in generation.segments)
        )
        can_stage = (
            staged_size + self.total_bytes <= self.max_bytes
            and staged_size <= self.max_bytes - self.sketch.bytes
        )
        if can_stage:
            self.staging_bytes += staged_size
            self._record_peak()
            staging_owned = True
            try:
                self.work.full_cold_replays += 1
                payloads = self._read_by_v1_routes(generation)
                _verify_semantics(generation, payloads, self.work)
                self._final_ancestry_check(generation, ref_name)
                entry = None
                if uses >= self.warm_uses and self.sketch.estimate(key_bytes) >= self.warm_uses:
                    replay_work = (
                        self.work.physical_payload_bytes
                        + self.work.semantic_verifier_runs
                        - work_before
                        + len(generation.segments)
                    )
                    entry = self._admit(generation, payloads, staged_size, max(1, replay_work))
                    if entry is not None:
                        staging_owned = False
                return ReplayLease(self.repository, generation, self, entry, self.work)
            except Exception:
                self.repository.unpin(commit, self.work)
                raise
            finally:
                if staging_owned:
                    self.staging_bytes -= staged_size
                self._record_peak()

        # Oversized or pressured materializations retain the production cold
        # fallback: bounded closure verification and disk spool, no memory copy.
        cold = ColdV2Replay(self.repository)
        try:
            cold._cold_verify(generation)
            self._final_ancestry_check(generation, ref_name)
            return ReplayLease(self.repository, generation, self, None, self.work)
        except Exception:
            self.repository.unpin(commit, self.work)
            raise
        finally:
            self._merge_cold_work(cold.work)

    def _read_by_v1_routes(self, generation: Generation) -> dict[tuple[str, str], bytes]:
        payloads: dict[tuple[str, str], bytes] = {}
        objects: dict[str, bytes] = {}
        for segment in generation.segments:
            if segment.object_id in objects:
                payloads[segment.key] = objects[segment.object_id]
                continue
            candidate, hops, changed_bytes, actions = self._delta_candidate(generation, segment)
            self.work.delta_hops_considered += hops
            self.work.delta_changed_bytes_considered += changed_bytes
            self.work.delta_actions_considered += actions
            if candidate is not None and _modest_delta(
                segment, hops, changed_bytes, actions, self.max_actions
            ):
                try:
                    payload = self.repository.read_segment(candidate.commit, segment, self.work)
                    self.work.delta_reads += 1
                except ModelError:
                    self.work.delta_fallbacks += 1
                    payload = self.repository.read_segment(generation.commit, segment, self.work)
                    self.work.pristine_reads += 1
            else:
                if candidate is not None:
                    self.work.delta_fanout_fallbacks += 1
                payload = self.repository.read_segment(generation.commit, segment, self.work)
                self.work.pristine_reads += 1
            objects.setdefault(segment.object_id, payload)
            payloads[segment.key] = objects[segment.object_id]
        return payloads

    def _delta_candidate(
        self, generation: Generation, segment: Segment
    ) -> tuple[Generation | None, int, int, int]:
        return _find_delta_candidate(
            self.repository,
            generation,
            segment,
            self.max_hops,
            self.max_changed_bytes,
            self.max_actions,
        )

    def _observe_ghost(self, key: tuple[object, ...], key_bytes: int) -> int:
        ghost = self.ghosts.get(key)
        if ghost is not None:
            ghost.uses = min(self.warm_uses, ghost.uses + 1)
            self.ghosts.move_to_end(key)
            return ghost.uses
        if self.ghost_limit == 0:
            return 1
        footprint = self.GHOST_BASE_BYTES + key_bytes
        while self.ghosts and (
            len(self.ghosts) >= self.ghost_limit
            or self.total_bytes + footprint > self.max_bytes
        ):
            self.ghosts.popitem(last=False)
        if self.total_bytes + footprint > self.max_bytes:
            return 1
        self.ghosts[key] = _Ghost(key_bytes)
        self._record_peak()
        return 1

    def _entry_footprint(self, generation: Generation, payloads: Mapping[tuple[str, str], bytes]) -> int:
        unique_bytes = sum({segment.object_id: len(payloads[segment.key]) for segment in generation.segments}.values())
        index_bytes = sum(
            self.ENTRY_SEGMENT_BYTES
            + len(segment.plane.encode())
            + len(segment.key[1].encode())
            for segment in generation.segments
        )
        return self.ENTRY_BASE_BYTES + generation.exact_key_bytes + index_bytes + unique_bytes

    def _probation_limit(self) -> int:
        return (self.max_bytes - self.sketch.bytes) * self.probation_percent // 100

    def _protected_limit(self) -> int:
        return self.max_bytes - self.sketch.bytes - self._probation_limit()

    def _utility(self, entry: _Entry) -> int:
        return (entry.reuse_work * max(1, len(entry.generation.segments))) // max(1, entry.footprint)

    def _admit(
        self,
        generation: Generation,
        payloads: dict[tuple[str, str], bytes],
        staged_size: int,
        reuse_work: int,
    ) -> _Entry | None:
        key = generation.exact_key
        footprint = self._entry_footprint(generation, payloads)
        probation_need = footprint
        self.ghosts.pop(key, None)
        while self.ghosts and self.total_bytes - staged_size + footprint > self.max_bytes:
            self.ghosts.popitem(last=False)
        if footprint > self._probation_limit() or footprint + self.sketch.bytes > self.max_bytes:
            self.work.replay_admission_rejections += 1
            return None
        while self.probation_bytes + probation_need > self._probation_limit():
            victim = self._oldest_unpinned("probation")
            if victim is None:
                self.work.replay_admission_rejections += 1
                return None
            self._evict(victim)
        while (
            self.entry_count >= self.max_entries
            or self.total_bytes - staged_size + footprint > self.max_bytes
        ):
            victim = self._oldest_unpinned("probation")
            if victim is not None:
                self._evict(victim)
                continue
            protected_victim = self._lowest_utility_unpinned("protected")
            candidate_score = (
                max(1, self.sketch.estimate(_key_bytes(key))) * max(1, reuse_work)
            ) // max(1, footprint)
            if protected_victim is None or candidate_score <= self._utility(protected_victim[1]):
                self.work.replay_admission_rejections += 1
                return None
            self._evict(protected_victim[0])
        entry = _Entry(
            generation=generation,
            payloads=payloads,
            footprint=footprint,
            reuse_work=reuse_work,
            last_used=self.tick,
        )
        self.entries[key] = entry
        # Payload bytes transfer from the bounded stage map into the entry;
        # the dict and immutable bytes are moved, not duplicated.
        self.staging_bytes -= staged_size
        self.probation_bytes += footprint
        self.work.replay_cache_admissions += 1
        self.ghosts.pop(key, None)
        self._record_peak()
        return entry

    def _promote(self, key: tuple[object, ...], entry: _Entry) -> None:
        if entry.tier != "probation" or entry.footprint > self._protected_limit():
            return
        while self.protected_bytes + entry.footprint > self._protected_limit():
            victim = self._lowest_utility_unpinned("protected")
            if victim is None or self._utility(entry) <= self._utility(victim[1]):
                return
            self._evict(victim[0])
        self.probation_bytes -= entry.footprint
        self.protected_bytes += entry.footprint
        entry.tier = "protected"
        entry.reuse_work += 1
        self.entries.move_to_end(key)

    def _oldest_unpinned(self, tier: str) -> tuple[object, ...] | None:
        return next(
            (key for key, entry in self.entries.items() if entry.tier == tier and entry.pins == 0),
            None,
        )

    def _lowest_utility_unpinned(self, tier: str) -> tuple[tuple[object, ...], _Entry] | None:
        candidates = [
            (key, entry)
            for key, entry in self.entries.items()
            if entry.tier == tier and entry.pins == 0
        ]
        return min(candidates, key=lambda pair: (self._utility(pair[1]), pair[1].last_used)) if candidates else None

    def _evict(self, key: tuple[object, ...]) -> None:
        entry = self.entries.pop(key)
        if entry.pins:
            raise AssertionError("attempted to evict a pinned generation")
        if entry.tier == "probation":
            self.probation_bytes -= entry.footprint
        else:
            self.protected_bytes -= entry.footprint
        self.work.replay_cache_evictions += 1

    def _final_ancestry_check(self, generation: Generation, ref_name: str) -> None:
        if self.repository.before_final_ancestry is not None:
            hook, self.repository.before_final_ancestry = self.repository.before_final_ancestry, None
            hook()
        self.repository.check_ancestry(generation, ref_name, self.work)
        self.repository.check_metadata(generation, self.work)

    def _merge_cold_work(self, cold: Work) -> None:
        for name in (
            "physical_payload_bytes",
            "object_hash_bytes",
            "spool_bytes_written",
            "spool_bytes_read",
            "semantic_verifier_runs",
            "segments_verified",
            "rows_verified",
            "full_cold_replays",
            "metadata_checks",
        ):
            setattr(self.work, name, getattr(self.work, name) + getattr(cold, name))

    def _record_peak(self) -> None:
        self.peak_policy_bytes = max(self.peak_policy_bytes, self.total_bytes)
        if self.total_bytes > self.max_bytes:
            raise AssertionError(f"policy exceeded byte bound: {self.total_bytes} > {self.max_bytes}")
        if self.entry_count > self.max_entries:
            raise AssertionError(f"policy exceeded entry bound: {self.entry_count} > {self.max_entries}")


def _key_bytes(key: tuple[object, ...]) -> bytes:
    return b"\0".join(str(value).encode() for value in key)


def run_trace() -> dict[str, object]:
    """Run a deterministic hot-set plus more-than-10x one-pass scan trace."""
    repository = Repository()
    hot_segments = tuple(
        Segment("Core", index.to_bytes(2, "big"), index.to_bytes(2, "big"), bytes([index]) * 128)
        for index in range(2)
    )
    hot = Generation("trace-target", "hot", "build-1", "facts-1", "input-1", hot_segments)
    repository.add(hot)
    repository.set_ref(hot.target, "main", hot.commit)

    budget = 8192
    policy = LiveGenerationPolicy(repository, max_bytes=budget, max_entries=16, ghost_entries=32)
    for _ in range(3):
        with policy.materialize(hot.commit, "main"):
            pass

    scan_payload_bytes = 0
    for index in range(800):
        payload = bytes([index % 251]) * 128
        segment = Segment(
            "Core",
            (index + 100).to_bytes(4, "big"),
            (index + 100).to_bytes(4, "big"),
            payload,
        )
        item = Generation(
            "trace-target",
            f"scan-{index:03}",
            "build-1",
            "facts-1",
            f"scan-input-{index}",
            (segment,),
        )
        repository.add(item)
        scan_payload_bytes += len(payload)
        repository.set_ref(item.target, "scan", item.commit)
        with policy.materialize(item.commit, "scan"):
            pass

    return {
        "budget_bytes": budget,
        "scan_payload_bytes": scan_payload_bytes,
        "scan_over_budget_multiple": scan_payload_bytes / budget,
        "protected_hot_generation_present": hot.exact_key in policy.entries
        and policy.entries[hot.exact_key].tier == "protected",
        "entry_count": policy.entry_count,
        "total_policy_bytes": policy.total_bytes,
        "peak_policy_bytes": policy.peak_policy_bytes,
        "probation_bytes": policy.probation_bytes,
        "protected_bytes": policy.protected_bytes,
        "work": policy.work.as_dict(),
    }


def run_ab_trace() -> dict[str, object]:
    """Compare V1 routes, cold V2 replay, and live generation replay on one DAG."""
    repository = Repository()
    unchanged = Segment("Core", b"a", b"a", b"unchanged" * 32)
    old = Segment("Core", b"b", b"b", b"old-row" * 32)
    changed = Segment("Core", b"b", b"b", b"new-row" * 32)
    base = Generation("ab-target", "ab-base", "build-a", "facts-a", "input-1", (unchanged, old))
    target = Generation(
        "ab-target",
        "ab-target",
        "build-a",
        "facts-a",
        "input-2",
        (unchanged, changed),
        parent=base.commit,
        changed_bytes=len(old.payload) + len(changed.payload),
        changed_actions=1,
    )
    repository.add(base)
    repository.add(target)
    repository.set_ref(target.target, "main", target.commit)

    route = V1RouteBaseline(repository, hot_bytes=4096, max_hops=4)
    route_oracle: dict[tuple[str, str], bytes] = {}
    for _ in range(3):
        for item in target.segments:
            route_oracle[item.key] = route.read(target.commit, item)
    if route_oracle != {item.key: item.payload for item in target.segments}:
        raise AssertionError("V1 route trace differed from the target payload oracle")

    cold = ColdV2Replay(repository)
    for _ in range(3):
        with cold.replay(target.commit, "main") as replay:
            if (replay.content_root, replay.generation_root) != (
                target.content_root,
                target.generation_root,
            ):
                raise AssertionError("cold V2 trace differed from the generation oracle")

    live = LiveGenerationPolicy(repository, max_bytes=16_384, max_entries=8)
    for _ in range(3):
        with live.materialize(target.commit, "main") as replay:
            if (replay.content_root, replay.generation_root) != (
                target.content_root,
                target.generation_root,
            ):
                raise AssertionError("live materialization trace differed from the generation oracle")
            if replay.payloads is not None:
                if dict(replay.payloads) != {item.key: item.payload for item in target.segments}:
                    raise AssertionError("live byte materialization differed from the payload oracle")

    return {
        "oracle_roots_match": True,
        "target_payload_bytes": sum(len(item.payload) for item in target.segments),
        "v1_segment_routes": route.work.as_dict(),
        "v2_cold_replay": cold.work.as_dict(),
        "v2_live_generation": live.work.as_dict(),
        "v2_live_resident_bytes": live.total_bytes,
        "v2_live_peak_bytes": live.peak_policy_bytes,
    }


if __name__ == "__main__":
    print(json.dumps({"ab_trace": run_ab_trace(), "scan_trace": run_trace()}, sort_keys=True, indent=2))
