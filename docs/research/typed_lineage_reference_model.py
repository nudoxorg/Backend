#!/usr/bin/env python3
"""Independent small oracle for the typed lineage-edge contract.

This deliberately does not import, generate, or parse the Rust wire format.
It models history as immutable commit/root/entity sets and checks the semantic
claims directly, so a matching Rust encoder and decoder cannot make a bad
round-trip look correct. Run with `python3 docs/research/typed_lineage_reference_model.py`.
"""

from __future__ import annotations

from collections import defaultdict, deque
from dataclasses import dataclass
import unittest
from itertools import product


Identity = tuple[int, int]  # full (family, variant) test identities


@dataclass(frozen=True)
class Snapshot:
    commit: str
    root: str
    parents: tuple[str, ...]
    declarations: frozenset[Identity]


@dataclass(frozen=True)
class Edge:
    kind: str
    source_commit: str
    source_root: str
    source: Identity
    target: Identity
    status: str
    group: str | None = None
    index: int | None = None
    count: int | None = None
    attestation: str | None = None


@dataclass(frozen=True)
class EdgeSet:
    parent_commit: str
    parent_root: str
    child_root: str
    edges: tuple[Edge, ...]


class Reject(ValueError):
    pass


def edge_pair(edge: Edge) -> tuple[object, ...]:
    return (
        edge.source_commit,
        edge.source_root,
        edge.source,
        edge.target,
    )


def verify_reference(
    claim: EdgeSet,
    history: dict[str, Snapshot],
    trusted_attestations: frozenset[
        tuple[str, str, str, str, tuple[object, ...], str]
    ] = frozenset(),
) -> tuple[Edge, ...]:
    """Check roots, identity membership, ancestry, ambiguity, and authority."""
    parent = history[claim.parent_commit]
    children = [
        snapshot
        for snapshot in history.values()
        if snapshot.root == claim.child_root
        and snapshot.parents
        and snapshot.parents[0] == parent.commit
    ]
    if parent.root != claim.parent_root or not children:
        raise Reject("generation-root-mismatch")
    if len({snapshot.declarations for snapshot in children}) != 1:
        raise Reject("generation-root-content-collision")
    child = children[0]

    pairs: set[tuple[object, ...]] = set()
    groups: dict[str, list[Edge]] = defaultdict(list)
    for edge in claim.edges:
        pair = edge_pair(edge)
        if pair in pairs:
            raise Reject("duplicate-edge")
        pairs.add(pair)
        if edge.status == "ambiguous":
            if edge.group is None:
                raise Reject("candidate-group-missing")
            groups[edge.group].append(edge)

    for members in groups.values():
        declared = {edge.count for edge in members}
        if len(declared) != 1:
            raise Reject("candidate-count-conflict")
        count = next(iter(declared))
        if count is None or count < 2 or count > 4096 or len(members) != count:
            raise Reject("candidate-group-incomplete")
        if sorted(edge.index for edge in members) != list(range(count)):
            raise Reject("candidate-index-collision")
        if len({edge.kind for edge in members}) != 1:
            raise Reject("candidate-kind-conflict")

    for edge in claim.edges:
        if edge.kind == "rename":
            if (
                edge.source_commit != parent.commit
                or edge.source_root != parent.root
                or edge.source == edge.target
            ):
                raise Reject("rename-source-binding")
            _presence(parent.declarations, edge.source, True)
            _presence(child.declarations, edge.source, False)
            _presence(parent.declarations, edge.target, False)
            _presence(child.declarations, edge.target, True)
        elif edge.kind == "resurrection":
            origin = history.get(edge.source_commit)
            if (
                origin is None
                or origin.root != edge.source_root
                or origin.commit == parent.commit
                or not _is_ancestor(history, origin.commit, parent.commit)
            ):
                raise Reject("resurrection-origin-not-ancestor")
            if edge.source != edge.target:
                raise Reject("resurrection-identity-changed")
            _presence(origin.declarations, edge.source, True)
            _presence(parent.declarations, edge.source, False)
            _presence(parent.declarations, edge.target, False)
            _presence(child.declarations, edge.target, True)
        else:
            raise Reject("unknown-kind")

        if edge.status == "confirmed":
            transition_proof = (
                claim.parent_commit,
                claim.parent_root,
                claim.child_root,
                edge.kind,
                edge_pair(edge),
                edge.attestation,
            )
            if edge.attestation is None or transition_proof not in trusted_attestations:
                raise Reject("unattested-confirmation")
        elif edge.status not in {"ambiguous", "unresolved"}:
            raise Reject("unknown-status")

    return claim.edges


def _presence(rows: frozenset[Identity], identity: Identity, expected: bool) -> None:
    if (identity in rows) != expected:
        raise Reject("endpoint-presence-mismatch")


def _is_ancestor(history: dict[str, Snapshot], ancestor: str, commit: str) -> bool:
    """Walk commit parents; same-root or same-content does not imply ancestry."""
    pending = deque(history[commit].parents)
    seen: set[str] = set()
    while pending:
        current = pending.popleft()
        if current == ancestor:
            return True
        if current not in seen:
            seen.add(current)
            pending.extend(history[current].parents)
    return False


def _history_for_edit(
    parent_rows: frozenset[Identity], child_rows: frozenset[Identity]
) -> dict[str, Snapshot]:
    return {
        "p": Snapshot("p", "root-p", (), parent_rows),
        "c": Snapshot("c", "root-c", ("p",), child_rows),
    }


class IndependentLineageOracleTests(unittest.TestCase):
    def test_generated_direct_identity_rekeys(self) -> None:
        # Generated edit sequences use plain sets and exact composite IDs.
        for family in range(1, 9):
            for variant in range(1, 7):
                before = (family, variant)
                after = (family, variant + 32)
                history = _history_for_edit(frozenset({before}), frozenset({after}))
                edge = Edge("rename", "p", "root-p", before, after, "unresolved")
                self.assertEqual(
                    verify_reference(EdgeSet("p", "root-p", "root-c", (edge,)), history),
                    (edge,),
                )

                # A canonical wire round-trip could preserve this mutation,
                # but the semantic oracle rejects a target that is not in the
                # exact child reader.
                wrong_target = (family + 40, variant + 32)
                forged = Edge("rename", "p", "root-p", before, wrong_target, "unresolved")
                with self.assertRaisesRegex(Reject, "endpoint-presence-mismatch"):
                    verify_reference(EdgeSet("p", "root-p", "root-c", (forged,)), history)

                # The same root/identity facts cannot skip an intermediate
                # commit: lineage is one direct first-parent transition.
                skipped = {
                    "p": Snapshot("p", "root-p", (), frozenset({before})),
                    "mid": Snapshot("mid", "root-mid", ("p",), frozenset()),
                    "c": Snapshot("c", "root-c", ("mid",), frozenset({after})),
                }
                with self.assertRaisesRegex(Reject, "generation-root-mismatch"):
                    verify_reference(EdgeSet("p", "root-p", "root-c", (edge,)), skipped)

                # A merge's second parent is not the transition parent either.
                merge_history = {
                    "p": history["p"],
                    "other": Snapshot("other", "root-other", (), frozenset()),
                    "merge": Snapshot("merge", "root-c", ("other", "p"), frozenset({after})),
                }
                with self.assertRaisesRegex(Reject, "generation-root-mismatch"):
                    verify_reference(EdgeSet("p", "root-p", "root-c", (edge,)), merge_history)

    def test_overload_ambiguity_is_complete_and_selects_no_winner(self) -> None:
        for width in range(2, 5):
            old = frozenset((12, slot) for slot in range(width))
            new = frozenset((12, 100 + slot) for slot in range(width))
            history = _history_for_edit(old, new)
            count = width * width
            edges = tuple(
                Edge(
                    "rename",
                    "p",
                    "root-p",
                    source,
                    target,
                    "ambiguous",
                    group=f"overload-{width}",
                    index=index,
                    count=count,
                )
                for index, (source, target) in enumerate(
                    product(sorted(old), sorted(new))
                )
            )
            self.assertEqual(
                verify_reference(EdgeSet("p", "root-p", "root-c", edges), history),
                edges,
            )
            with self.assertRaisesRegex(Reject, "candidate-group-incomplete"):
                verify_reference(EdgeSet("p", "root-p", "root-c", edges[:-1]), history)

            # A reused digest/group ID cannot silently combine two declared
            # groups into a new matching authority.
            colliding_sources = sorted((13, slot) for slot in range(width))
            colliding_targets = sorted((13, 100 + slot) for slot in range(width))
            collided = edges + tuple(
                Edge(
                    "rename",
                    "p",
                    "root-p",
                    source,
                    target,
                    "ambiguous",
                    group=f"overload-{width}",
                    index=index,
                    count=count,
                )
                for index, (source, target) in enumerate(
                    product(colliding_sources, colliding_targets)
                )
            )
            with self.assertRaisesRegex(Reject, "candidate-group-incomplete"):
                verify_reference(EdgeSet("p", "root-p", "root-c", collided), history)

    def test_delete_resurrect_sequences_require_actual_ancestry(self) -> None:
        identity = (21, 34)
        for gap in range(1, 8):
            history = {
                "origin": Snapshot("origin", "root-origin", (), frozenset({identity}))
            }
            previous = "origin"
            for index in range(gap):
                commit = f"gap-{gap}-{index}"
                history[commit] = Snapshot(commit, f"root-{commit}", (previous,), frozenset())
                previous = commit
            history["child"] = Snapshot("child", "root-child", (previous,), frozenset({identity}))
            edge = Edge("resurrection", "origin", "root-origin", identity, identity, "unresolved")
            claim = EdgeSet(previous, history[previous].root, "root-child", (edge,))
            self.assertEqual(verify_reference(claim, history), (edge,))

            # Same identity/root facts on a sibling branch are not ancestry.
            history["sibling"] = Snapshot("sibling", "root-origin", (), frozenset({identity}))
            sibling_edge = Edge("resurrection", "sibling", "root-origin", identity, identity, "unresolved")
            with self.assertRaisesRegex(Reject, "resurrection-origin-not-ancestor"):
                verify_reference(EdgeSet(previous, history[previous].root, "root-child", (sibling_edge,)), history)

            # Correct commit with a forged generation root is also rejected.
            forged_root_edge = Edge("resurrection", "origin", "root-sibling", identity, identity, "unresolved")
            with self.assertRaisesRegex(Reject, "resurrection-origin-not-ancestor"):
                verify_reference(EdgeSet(previous, history[previous].root, "root-child", (forged_root_edge,)), history)

    def test_bad_parent_root_and_unattested_confirmation_fail(self) -> None:
        old, new = (30, 1), (30, 2)
        history = _history_for_edit(frozenset({old}), frozenset({new}))
        edge = Edge("rename", "p", "root-p", old, new, "confirmed", attestation="proof-1")
        with self.assertRaisesRegex(Reject, "generation-root-mismatch"):
            verify_reference(EdgeSet("p", "stale-parent-root", "root-c", (edge,)), history)
        with self.assertRaisesRegex(Reject, "unattested-confirmation"):
            verify_reference(EdgeSet("p", "root-p", "root-c", (edge,)), history)
        self.assertEqual(
            verify_reference(
                EdgeSet("p", "root-p", "root-c", (edge,)),
                history,
                frozenset({("p", "root-p", "root-c", edge.kind, edge_pair(edge), "proof-1")}),
            ),
            (edge,),
        )
        other_transition = {
            **history,
            "child-alt": Snapshot("child-alt", "root-alt", ("p",), frozenset({new})),
        }
        with self.assertRaisesRegex(Reject, "unattested-confirmation"):
            verify_reference(
                EdgeSet("p", "root-p", "root-alt", (edge,)),
                other_transition,
                frozenset({("p", "root-p", "root-c", edge.kind, edge_pair(edge), "proof-1")}),
            )


if __name__ == "__main__":
    unittest.main()
