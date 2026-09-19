"""Small executable correctness model for the V2 state and delta laws.

This is deliberately simple: every mutation rebuilds the canonical tree from
sorted logical values.  Hash keyed node reuse models CAS structural sharing,
but this module makes no efficient update claim.
"""
from __future__ import annotations

from collections import Counter
from dataclasses import dataclass
import hashlib
import json
import zlib
from typing import Any, Iterable, Mapping


def _canon(value: Any) -> bytes:
    """Stable encoding for supported JSON-like values; reject opaque objects."""
    def normalize(item):
        if item is None or isinstance(item, (str, int, bool)):
            return item
        if isinstance(item, (list, tuple)):
            return [normalize(x) for x in item]
        if isinstance(item, dict):
            if not all(isinstance(k, str) for k in item):
                raise TypeError("canonical maps require string keys")
            return {k: normalize(v) for k, v in sorted(item.items())}
        raise TypeError(f"unsupported canonical value: {type(item).__name__}")
    return json.dumps(normalize(value), sort_keys=True, separators=(",", ":")).encode()


def _hash(domain: str, *parts: bytes) -> str:
    h = hashlib.sha256(domain.encode() + b"\0")
    for part in parts:
        h.update(len(part).to_bytes(8, "big"))
        h.update(part)
    return h.hexdigest()


@dataclass(frozen=True, eq=False, init=False)
class ObjectVersion:
    """Complete value, including availability and authority references."""

    _payload: bytes

    def __init__(self, value: Any, availability: str = "captured", references: tuple[str, ...] = ()):
        if not isinstance(availability, str) or not all(isinstance(r, str) for r in references):
            raise TypeError("availability and references must be strings")
        object.__setattr__(self, "_payload", _canon((value, availability, references)))

    @property
    def value(self):
        # Return a detached value; the immutable encoding is authoritative.
        return json.loads(self._payload)[0]

    @property
    def availability(self):
        return json.loads(self._payload)[1]

    @property
    def references(self):
        return tuple(json.loads(self._payload)[2])

    @property
    def identity(self) -> str:
        return _hash("object-version", self._payload)

    def __eq__(self, other):
        return isinstance(other, ObjectVersion) and self.identity == other.identity

    def __hash__(self):
        return hash(self.identity)


@dataclass(frozen=True)
class MapChange:
    key: str
    before: ObjectVersion | None
    after: ObjectVersion | None


@dataclass(frozen=True)
class _Leaf:
    entries: tuple[tuple[str, ObjectVersion], ...]
    hash: str


@dataclass(frozen=True)
class _Internal:
    children: tuple[Any, ...]
    separators: tuple[str, ...]
    hash: str


class DeltaError(ValueError):
    pass


class MerkleMap:
    """Canonical key ordered map with key-only 8/32/64 leaf policy."""

    LEAF_MIN = 8
    LEAF_TARGET = 32
    LEAF_MAX = 64
    FANOUT = 16

    def __init__(self, values: Mapping[str, ObjectVersion], root: Any, cache: dict[str, Any] | None = None):
        self.values = dict(values)
        self.root = root
        self._cache = cache if cache is not None else {}

    @classmethod
    def empty(cls) -> "MerkleMap":
        return cls.from_items({})

    @classmethod
    def from_items(cls, items: Mapping[str, ObjectVersion] | Iterable[tuple[str, ObjectVersion]], cache: dict[str, Any] | None = None) -> "MerkleMap":
        values = dict(items)
        cache = cache if cache is not None else {}
        leaves = []
        ordered = sorted(values.items())
        starts = [0]
        for index in range(cls.LEAF_MIN, len(ordered)):
            if ((index - starts[-1] >= cls.LEAF_MIN and _anchor(ordered[index][0], 5))
                    or index - starts[-1] >= cls.LEAF_MAX):
                starts.append(index)
        for index, start in enumerate(starts):
            end = starts[index + 1] if index + 1 < len(starts) else len(ordered)
            chunk = tuple(ordered[start:end])
            payload = _canon([(k, v.identity) for k, v in chunk])
            digest = _hash("leaf", payload)
            leaf = cache.get(digest)
            if leaf is None:
                leaf = _Leaf(chunk, digest)
                cache[digest] = leaf
            leaves.append(leaf)
        level: list[Any] = leaves
        depth = 1
        while len(level) > 1:
            next_level = []
            starts = [0]
            for index in range(2, len(level)):
                if ((index - starts[-1] >= 2 and _anchor(_first_key(level[index]), 3, depth))
                        or index - starts[-1] >= cls.FANOUT):
                    starts.append(index)
            for index, start in enumerate(starts):
                end = starts[index + 1] if index + 1 < len(starts) else len(level)
                children = tuple(level[start:end])
                separators = tuple(_first_key(c) for c in children[1:])
                digest = _hash("internal", _canon(separators), _canon([c.hash for c in children]))
                node = cache.get(digest)
                if node is None:
                    node = _Internal(children, separators, digest)
                    cache[digest] = node
                next_level.append(node)
            # FANOUT >= 2 gives strict reduction whenever this loop runs.
            assert len(next_level) < len(level)
            level = next_level
            depth += 1
        return cls(values, level[0], cache)

    @property
    def state_root(self) -> str:
        return _hash("state-root:json-v1:key-cuts-8-32-64:parent-2-8-16", self.root.hash.encode())

    @property
    def leaf_cuts(self) -> tuple[tuple[str, ...], ...]:
        return tuple(tuple(k for k, _ in leaf.entries) for leaf in _leaves(self.root))

    @property
    def node_count(self) -> int:
        return sum(1 for _ in _nodes(self.root))

    def get(self, key: str) -> ObjectVersion | None:
        return self.values.get(key)

    def with_changes(self, changes: Iterable[MapChange]) -> "MerkleMap":
        values = dict(self.values)
        for change in changes:
            if values.get(change.key) != change.before:
                raise DeltaError(f"before mismatch for {change.key!r}")
            if change.after is None:
                values.pop(change.key, None)
            else:
                values[change.key] = change.after
        return MerkleMap.from_items(values, self._cache)

    def delta_to(self, other: "MerkleMap") -> "MapDelta":
        changes = tuple(MapChange(k, self.values.get(k), other.values.get(k)) for k in sorted(set(self.values) | set(other.values)) if self.values.get(k) != other.values.get(k))
        return MapDelta(self.state_root, other.state_root, changes)


def _first_key(node: Any) -> str:
    return node.entries[0][0] if isinstance(node, _Leaf) else _first_key(node.children[0])


def _anchor(key: str, bits: int, depth: int = 0) -> bool:
    value = int(hashlib.sha256(f"cut:{depth}\0".encode() + key.encode()).hexdigest()[:8], 16)
    return value & ((1 << bits) - 1) == 0


def _leaves(node: Any):
    if isinstance(node, _Leaf):
        yield node
    else:
        for child in node.children:
            yield from _leaves(child)


def _nodes(node: Any):
    yield node
    if isinstance(node, _Internal):
        for child in node.children:
            yield from _nodes(child)


@dataclass(frozen=True)
class MapDelta:
    base_root: str
    target_root: str
    changes: tuple[MapChange, ...]

    def __post_init__(self):
        keys = [change.key for change in self.changes]
        if keys != sorted(set(keys)):
            raise DeltaError("changes must be unique and canonical key order")
        if any(c.before == c.after for c in self.changes):
            raise DeltaError("canonical delta omits no-op changes")

    def apply(self, base: MerkleMap) -> MerkleMap:
        if base.state_root != self.base_root:
            raise DeltaError("wrong base root")
        result = base.with_changes(self.changes)
        if result.state_root != self.target_root:
            raise DeltaError("delta target root mismatch")
        return result

    def inverse(self) -> "MapDelta":
        return MapDelta(self.target_root, self.base_root, tuple(MapChange(c.key, c.after, c.before) for c in self.changes))

    def compose(self, following: "MapDelta") -> "MapDelta":
        if self.target_root != following.base_root:
            raise DeltaError("non-adjacent delta composition")
        left = {c.key: c for c in self.changes}
        right = {c.key: c for c in following.changes}
        merged = []
        for key in sorted(set(left) | set(right)):
            a, b = left.get(key), right.get(key)
            if a is not None and b is not None and a.after != b.before:
                raise DeltaError(f"overlapping delta mismatch for {key!r}")
            before = a.before if a is not None else b.before
            after = b.after if b is not None else a.after
            if before != after:
                merged.append(MapChange(key, before, after))
        return MapDelta(self.base_root, following.target_root, tuple(merged))


@dataclass(frozen=True)
class WorkspaceRoot:
    """Simplified relation-only root; production adds schema/basis/coverage."""

    relations: tuple[tuple[str, str], ...]

    @property
    def root(self) -> str:
        return _hash("workspace-root", _canon(self.relations))

    @classmethod
    def from_maps(cls, maps: Mapping[str, MerkleMap]) -> "WorkspaceRoot":
        return cls(tuple(sorted((name, value.state_root) for name, value in maps.items())))


@dataclass(frozen=True)
class WorkspaceDelta:
    base: WorkspaceRoot
    target: WorkspaceRoot
    relations: tuple[tuple[str, MapDelta], ...]

    def apply(self, maps: Mapping[str, MerkleMap]) -> dict[str, MerkleMap]:
        current = WorkspaceRoot.from_maps(maps)
        if current.root != self.base.root:
            raise DeltaError("wrong workspace base root")
        out = dict(maps)
        for name, delta in self.relations:
            if name not in out:
                raise DeltaError(f"missing relation {name!r}")
            out[name] = delta.apply(out[name])
        if WorkspaceRoot.from_maps(out).root != self.target.root:
            raise DeltaError("workspace target root mismatch")
        return out


def join_delta(old_left: Counter, delta_left: Counter, old_right: Counter, delta_right: Counter) -> Counter:
    """Three-term old-state join, retaining signed multiplicities."""
    def join(a: Counter, b: Counter) -> Counter:
        by_key = {}
        for row, weight in a.items():
            by_key.setdefault(row[0], []).append((row[1], weight))
        result = Counter()
        for row, weight in b.items():
            for left_payload, left_weight in by_key.get(row[0], ()):
                result[(left_payload, row[1])] += left_weight * weight
        return result
    # Do not normalize away negative weights: callers need the signed batch.
    out = Counter()
    for term in (join(delta_left, old_right), join(old_left, delta_right), join(delta_left, delta_right)):
        for row, weight in term.items():
            out[row] += weight
    return out


@dataclass
class Trace:
    """Minimal one dimensional hot trace; historical roots live elsewhere."""

    updates: Counter[tuple[Any, int]]

    @classmethod
    def from_updates(cls, updates: Iterable[tuple[Any, int, int]]) -> "Trace":
        values = Counter()
        for row, time, weight in updates:
            values[(row, time)] += weight
        return cls(values)

    def observe(self, time: int) -> Counter:
        result = Counter()
        for (row, stamp), weight in self.updates.items():
            if stamp <= time:
                result[row] += weight
        return Counter({row: weight for row, weight in result.items() if weight})

    def compact(self, frontier: int) -> "Trace":
        # Safe only after observers older than frontier have released their pins.
        baseline = self.observe(frontier - 1)
        kept = Counter({(row, frontier): weight for row, weight in baseline.items()})
        for key, weight in self.updates.items():
            if key[1] >= frontier:
                kept[key] += weight
        return Trace(Counter({key: weight for key, weight in kept.items() if weight}))


def diff_nodes(left: MerkleMap, right: MerkleMap) -> tuple[int, int]:
    """Count node-pair probes and unequal leaf ranges for equal key cuts.

    Unequal structure uses an intentionally broad leaf-hash comparison;
    it counts every enumerated fallback node and unequal leaf from either
    side. This is structural instrumentation, not an emitted row diff.
    delta_to is the separate O(N) correct row-diff oracle.
    """
    visited = changed = 0
    def walk(a: Any, b: Any):
        nonlocal visited, changed
        visited += 1
        if a.hash == b.hash:
            return
        if isinstance(a, _Leaf) and isinstance(b, _Leaf):
            changed += 1
            return
        if isinstance(a, _Internal) and isinstance(b, _Internal) and len(a.children) == len(b.children) and a.separators == b.separators:
            for x, y in zip(a.children, b.children):
                walk(x, y)
        else:
            # Charge traversal; do not hide O(N) work in a one-probe result.
            visited += sum(1 for _ in _nodes(a)) + sum(1 for _ in _nodes(b)) - 2
            a_hashes = {leaf.hash for leaf in _leaves(a)}
            b_hashes = {leaf.hash for leaf in _leaves(b)}
            changed += len(a_hashes ^ b_hashes)
    walk(left.root, right.root)
    return visited, changed


def repack(mapping: MerkleMap, codec: str) -> tuple[str, dict[str, tuple[str, int]]]:
    """Model physical repacking: new physical ID, same logical root."""
    locations = {key: (_hash("pack", codec.encode(), version.identity.encode()), index) for index, (key, version) in enumerate(sorted(mapping.values.items()))}
    pack_id = _hash("pack-id", codec.encode(), _canon(sorted(locations.items())))
    return pack_id, locations


def encode_pack(mapping: MerkleMap, codec: str) -> bytes:
    payload = _canon([(key, version._payload.decode()) for key, version in sorted(mapping.values.items())])
    if codec == "compressed":
        payload = zlib.compress(payload)
    elif codec != "plain":
        raise ValueError("unsupported physical codec")
    return codec.encode() + b"\0" + payload


def decode_pack(data: bytes) -> MerkleMap:
    codec, payload = data.split(b"\0", 1)
    if codec == b"compressed":
        payload = zlib.decompress(payload)
    elif codec != b"plain":
        raise ValueError("unsupported physical codec")
    values = {}
    for key, encoded in json.loads(payload):
        value, availability, references = json.loads(encoded)
        values[key] = ObjectVersion(value, availability, tuple(references))
    return MerkleMap.from_items(values)
