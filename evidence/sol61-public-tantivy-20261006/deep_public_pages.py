"""Full public-record gate for a real matched CLI/MCP session.

The caller owns fresh publication, cohort verification and resource admission.
This module never starts a daemon, changes sources, or substitutes an in-memory
answer. Each query callback must retain its exact request/reply bytes.
"""
import base64
import collections
import json
import zlib


QUERIES = (
    "cached", "cachedmethod", "__getitem__", "Cache", "LRUCache",
    "LFUCache", "TTLCache", "TLRUCache", "popitem", "methodkey",
    "hashkey", "typedkey", "cache_clear", "cache_info", "expire",
    "__contains__", "__setitem__", "__delitem__", "λ路径", "nohitλ路径",
)


def identity(row):
    return json.dumps(row, ensure_ascii=False, sort_keys=True, separators=(",", ":"))


def token_sizes(token):
    assert len(token.encode()) <= 48 * 1024, "bounded public token diagnostic"
    if token.startswith("mcp1-"):
        encoded = token.split("-", 2)[2].rsplit("-", 1)[0]
        owner = base64.urlsafe_b64decode(encoded + "=" * (-len(encoded) % 4)).decode()
    else:
        owner = token
    if owner.startswith("pc3-"):
        encoded = owner[4:]
        decoder = zlib.decompressobj()
        body = decoder.decompress(base64.urlsafe_b64decode(encoded + "=" * (-len(encoded) % 4)), 256 * 1024 + 1)
        assert decoder.eof and not decoder.unused_data and not decoder.unconsumed_tail, "complete bounded compressed command"
    elif owner.startswith("pc2-"):
        body = bytes.fromhex(owner[4:])
    elif owner.startswith("pc1-"):
        body = bytes.fromhex(owner[4:])
        # a659 CURSOR_QUERY_BYTES = (2 + 32*5 + 2 + 8) + 8.
        assert len(body) == 180, "exact session graph cursor width"
    else:
        raise AssertionError("unrecognized actual owner token")
    assert len(body) <= 256 * 1024, "actual canonical command ceiling"
    assert len(owner.encode()) <= 32 * 1024, "actual owner presentation ceiling"
    return {"public_token_bytes": len(token.encode()), "owner_token_bytes": len(owner.encode()),
            "decoded_command_bytes": len(body), "owner_version": owner[:3]}


def traverse(query, surface, route, text, credit, expected):
    rows, tokens, sizes = [], set(), []
    token = None
    for page in range(4097):
        value = query(surface, route, text, credit, token)
        assert value.get("answer") == "records", (surface, route, text, credit, page, value)
        current = value.get("records", [])
        assert len(current) <= credit, "page credit"
        rows.extend(current)
        successor = value.get("nextCursor")
        assert bool(value.get("more")) == bool(successor), "continuation completeness"
        if not successor:
            break
        assert current, "nonterminal empty page"
        assert successor not in tokens, "repeated full token"
        tokens.add(successor)
        sizes.append(token_sizes(successor))
        token = successor
    else:
        raise AssertionError("bounded traversal exceeded 4096 pages")
    actual, reference = list(map(identity, rows)), list(map(identity, expected))
    assert collections.Counter(actual) == collections.Counter(reference), "complete row multiplicities"
    assert actual == reference, "complete row order"
    return {"surface": surface, "route": route, "query": text, "credit": credit,
            "rows": len(rows), "pages": page + 1, "token_sizes": sizes, "pass": True}


def run_matrix(query, queries=QUERIES, credits=(1, 3, 7)):
    """query(surface, route, text, credit, token) returns the public answer."""
    results = []
    for route in ("resolve", "search"):
        for text in queries:
            references = {surface: query(surface, route, text, 200, None) for surface in ("cli", "mcp")}
            reference_facts = {}
            try:
                for surface, value in references.items():
                    refusals, reference_credit = [], 200
                    # A response-sized first page does not guarantee all later
                    # pages fit. Use the tested small page credits for a full
                    # reference stream after the retained 200-credit refusal.
                    for candidate in (7, 3, 1):
                        if value.get("answer") == "records":
                            break
                        assert (value.get("answer") == "fault" and value.get("slug") == "transport"
                                and "byte budget" in value.get("detail", "")), "typed presentation budget refusal required before reducing reference credit"
                        refusals.append({"credit": reference_credit, "fault": value})
                        reference_credit = candidate
                        value = query(surface, route, text, candidate, None)
                    assert value.get("answer") == "records", "admissible complete reference required"
                    rows, tokens, pages = [], set(), 0
                    while True:
                        assert value.get("answer") == "records", "reference continuation refused"
                        current = value.get("records", [])
                        assert len(current) <= reference_credit
                        rows.extend(current); pages += 1
                        token = value.get("nextCursor")
                        assert bool(value.get("more")) == bool(token), "reference continuation completeness"
                        if not token: break
                        assert current and token not in tokens and pages <= 4096
                        tokens.add(token); token_sizes(token)
                        value = query(surface, route, text, reference_credit, token)
                    references[surface] = {"answer": "records", "records": rows, "more": False}
                    reference_facts[surface] = {"credit": reference_credit, "pages": pages,
                                                "rows": len(rows), "typed_budget_refusals": refusals}
                expected = references["cli"].get("records", [])
                assert list(map(identity, expected)) == list(map(identity, references["mcp"].get("records", []))), "CLI/MCP full reference parity"
            except AssertionError as error:
                results.append({"route": route, "query": text, "pass": False, "error": str(error), "references": references})
                continue
            for surface in ("cli", "mcp"):
                for credit in credits:
                    try:
                        result = traverse(query, surface, route, text, credit, expected)
                        result["references"] = reference_facts
                        results.append(result)
                    except (AssertionError, ValueError, zlib.error) as error:
                        results.append({"surface": surface, "route": route, "query": text,
                                        "credit": credit, "pass": False, "error": str(error)})
    return {"schema": "nudox.actual-full-public-pages.v1", "cases": results,
            "all_pass": bool(results) and all(case["pass"] for case in results),
            "whole_project_runtime_pass": False}
