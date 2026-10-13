# Python bound-receiver source checkpoint — October 13

HTTPie's pinned `5b604c37` application defines `@classmethod from_raw_data(self, data)` in `tests/utils/__init__.py`. Python binds the first positional argument regardless of its spelling. Nudox instead removed parameters by the names `self` or `cls`, producing wrong callable arities and hiding ordinary annotated parameters in module functions, nested functions and later argument positions.

The repair derives one immutable `ReceiverBindings` index from exact Ruff parameter-name spans and the lexical owner vector already used for shadowing. Signature consumers use bound membership; annotation consumers use a distinct conventional-receiver role. A custom `this: Target` annotation remains available for exact field resolution. No consumer rescans all declarations or recomputes owners per parameter, and the index uses no shared mutex or reference counting. The existing owner computation is reused; this is not a measured speedup claim.

Protocol callable members and emitted function declarations now use the same receiver policy. Static signatures retain their arguments, while a property interface member remains its result type. Undecorated `__new__` is implicitly static and retains its first argument; explicit `@classmethod __new__` uses class binding. Variadic and keyword-only argument lists retain their declared shape.

The regression controls cover the exact HTTPie signature, arbitrary receiver spellings, module and nested functions, later `self`/`cls`, positional-only and async methods, static/property behavior and the implicit-static exception. Graph laws distinguish `Target.note` from `Other.note` and check the module argument's exact owner-relative source span. The existing Protocol render fixture retains both interface/function pointers and now proves one public `size` parameter plus one result, their exact ordered `int`/`bytes` types and one result slot. Assertions were strengthened rather than removed.

The complete four-file source change is frozen at author `fe80ffc1a651a1fc4129cc3df09f9c95dabd29f0`, tree `875b06096f4089194cdf3a3338420cb9d410e855`, against canonical `8b27d833f71532a73a4d8b660661227023e9892b`. Root read the entire change and surrounding consumers; independent Luna cleared the final source. The single integrated atom `b9e58e5d9595f226b4288afc99c8b531a2becba8` has the **identical complete Git tree**, not just matching selected files. Root independently matched all four author postimages, parsed all four Rust files and checked the diff. Its receipt is `/private/tmp/nudox-root-python-receiver-audit-20261013.json`, SHA-256 `3908f75ed8847f9c3dc9083a23f53ce8dcf85d5704a3d308476b3e0169455d6c`.

The author ran ten definition-only CPython descriptor controls; Root rehashed their retained witness but did not rerun them. They do not import or run the HTTPie application and are not native Nudox acceptance. **Rust compilation, all eight native selectors, fresh HTTPie publication and CLI/MCP/GUI flows remain UNRUN.** No public installer selects this source yet.

Mandatory native selectors under an admitted current source/toolchain are:

- `python_lower_facts`: `exact_httpie_classmethod_omits_its_bound_receiver`, `httpie_classmethod_receiver_name_does_not_add_a_user_parameter`, `receiver_omission_requires_direct_class_and_first_positional_binding`, `protocol_members_and_method_declarations_share_bound_receiver_arity`, `implicit_static_new_retains_cls_unless_explicitly_class_bound`.
- `python_occurrence_receivers`: `module_self_parameter_retains_its_annotated_field_target`, `arbitrary_bound_receiver_keeps_its_annotated_field_target`.
- `python_render`: `python_fragment_forward_reference_structural_rows_validate`.

Run the complete affected suites after those exact selections pass, then ingest the pinned HTTPie application with current ordinary binaries. Existing compiler authority, wire and semantic-image admission remain in place. These gates are separate from the job custodian's injected controls and its still-required actual kernel lifecycle/confinement proof.
