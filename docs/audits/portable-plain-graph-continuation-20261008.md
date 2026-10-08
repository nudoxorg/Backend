# Portable plain graph continuations

Source-only atom based on reviewed `816adb775ee49b7524fc6446a1f0d65239638973`.
No Cargo, native build, target copy, application-state mutation or runtime acceptance was performed for this atom.

## Actual baseline

The independent Mac acceptance cohort was source `5f2a97eb4c74c175e190ff9637373c6bcf9ae39e`, with HTTPie in its ordinary private HOME. Mint owner 6823 and fresh owner 8080 used the same HOME, unchanged view/native selection and no reindex. Replay was 867.103 seconds before expiry. Query, names and class-children inner pc3 tokens reproduced their exact second pages; plain `backend.graph` alone emitted inner pc1 and failed cold.

The exact graph request was a structured-resolve-selected canonical coordinate ending `::semantic::2d56767f4dc1542d9e843d9652e8580c49d52bdeef87a8315b0381a2dff2dc42::parse_args`, limit 1, standard detail. Warm second page selected `_process_request_type` at `httpie/cli/argparser.py:196`, with more rows. This is observed projection ordering, not an independent semantic-edge correctness claim.

Raw base:
`/Users/mileswirht/Downloads/sol61-python-platform-default-current-20261008-5f2/httpie5f2-completed-evidence-v1/evidence/`

| Raw file | SHA256 |
| --- | --- |
| `focused-mint-01/focused-mint-call-graph-page1.json` | `5c2f5a131d1839f317fa199feb177694bc72c356a69c4121b3e01ea30740d480` |
| `focused-mint-01/focused-mint-call-graph-page2.json` | `91ee8d367b63a849758dd80466d2b1370f4aa83f820961516430d723a447dd3c` |
| `focused-replay-01/focused-cold-call-graph.json` | `394b4a8cb331462bc28119b90f9147bf3001b292950e8d92fd44811df5a655db` |
| `focused-replay-01/focused-exact-view-native-comparison.json` | `c518e04446dbd293c43be0e54e7aa49f3277f56bdafa99056ced42e87bb18e67` |

The cold graph reply was JSON-RPC -32602, unknown/expired/foreign cursor. All owners and permits are retired. Earlier expired-token results are not evidence for this unexpired defect.

## Cause and repair

`Session::send_success` retained portable requests only for Search, Name and GraphQuery. GraphPage already retained a certified predecessor, but had no `next_request`; MCP therefore selected stateful pc1. A new process has no matching continuation-map entry. An intervening paged response can also clear that map while the cursor remains unexpired.

The repair retains GraphPage's exact next command in the existing bounded pc3 envelope. It preserves its original canonical coordinate key claim, which the owner needs when copied coordinates resolve to compiler-owned row identities. Opaque selected addresses retain only their existing key commitment and remain distinct from canonical addresses.

Strict standalone command decoding rehashes the predecessor's recipe, relation and version. Fresh authority then supplies the owner basis/scope; import checks the exact selected root, stream/schema, nonfuture sequence and existing graph recipe. Resume checks exact address kind/value, page credit and predecessor before issuing the bounded command. The producer still reconstructs the exact previous page before returning any successor.

`QueryPageRecipe::graph` exposes a witness for the existing graph-page recipe bytes. It does not change producer recipes or add page credit to their historical grammar; credit remains in the exact continued command and predecessor reconstruction.

No protocol version, token family, expiry, HMAC, pc1 behavior, page limit, graph bound, ordering algorithm or wire-byte ceiling changes. No full graph is materialized by the new continuation path. Large proofs remain subject to the existing command/token bounds; this atom does not implement byte-packed pages.

## Controls prepared, not run

- `plain_graph_portable_continuation_reopens_exact_page_in_fresh_owner`: reconstructs a new producer and fresh Session with no retained continuation map, compares exact warm/cold second pages, traverses the bounded neighborhood without duplicated identities, and requires one revision plus one exact page request. Covers structural children and sorted selected nonparent neighbors with a copied coordinate differing from the producer key. Confirms pc1 remains valid in the issuing active session and refuses in a fresh one.
- `plain_graph_portable_continuation_refuses_foreign_contract_view_and_forgery`: changed coordinate, query family and credit refuse before a page RPC; changed scope/view refuses import; intent-only sequence progress succeeds. Forged recipe/version/root/address/zero offset refuse import. Positive forged offsets and changed serialized credit must fail actual predecessor reconstruction. Forged coverage authority fields refuse.
- `plain_graph_selected_address_is_portable_without_becoming_a_canonical_key`: opaque selected address survives cold import, while a canonical request with the same digest-derived source cannot consume that prepared selector contract.

The existing test transport was extended to construct graph certificates with the same canonical recipe witness and exercise real command encoding/decoding, producer-backed reply decoding and shared request/reply admission. Fixture authority is scoped to controls and is not actual native Python evidence.

Static validation: edited Rust parsed with rustfmt, and `git diff --check` passed. Required next gate is these client controls plus existing portable-query controls on an explicitly allocated remote warm graph, then a verified matched-trio real HTTPie mint/restart/replay before expiry. No native or actual successor pass is claimed here.
