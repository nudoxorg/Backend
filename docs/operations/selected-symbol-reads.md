# Read a selected declaration

Search, resolve, outline and graph records expose `identity.semantic_data` as
`{"kind":"selected-symbol-id","value":[/* 32 byte integers */]}`. This is the
full row locator; the short `identity.key` is display-only. Two compiler roles
can have the same coordinate, such as a TypeScript constructor parameter
property's field and parameter variable. Select the returned ID to read each
role independently.

For CLI `show`, `source`, `graph` and `related`, encode the returned 32 bytes as
64 hexadecimal digits and pass `--selected-id`. For example, when `record` is
one JSON record from a previous result:

```sh
id=$(printf '%s' "$record" | python3 -c 'import json,sys; d=json.load(sys.stdin)["identity"]["semantic_data"]; assert d["kind"] == "selected-symbol-id"; print(bytes(d["value"]).hex())')
nudox show --selected-id "$id" --format json
nudox source --selected-id "$id"
nudox graph --selected-id "$id"
nudox related --selected-id "$id"
```

For the named MCP tools `backend.document`, `backend.source`, `backend.graph`
and `backend.related`, copy that complete object into `arguments.selected_symbol`.
The same argument works through `backend.call` with the named tool. Supply either
a coordinate or a selected symbol, or both. If both are supplied, `coordinate`
is an exact assertion of the selected row's current label; it does not resolve
an otherwise missing ID. No name expansion is applied to this assertion.

The owner resolves an ID only against its current immutable view. It refuses
missing membership, malformed or all-zero IDs and mismatched coordinate
assertions. An ID is neither a compiler identity constructor nor a publication
proof. A removed row's old ID cannot read retained historical data through
these current-view commands. Graph cursors retain the selected ID, optional
coordinate assertion and original query credit; preserve the arguments when
continuing a page.

`references` still uses its existing coordinate-only protocol. The generic
`backend.surface` batch `Read` and `nudox read` retain their legacy coordinate
semantics, including first-match behavior for repeated labels. Use the selected
operand on the four commands above when exact same-coordinate role selection
matters. This change does not add selected IDs to batch Read or references.
