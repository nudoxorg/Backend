# Native35 graph return, pinned actions, and page publication

Actual Native35 source16dbe7a8b36e3aa8943a7a1fc3ccc9ac3a19fe09 compiled the desktop target successfully, but selected tests115,119,122 failed. This source repair is layered directly on that exact source; the original runtime receipts remain failures. No Cargo, GPUI/native test, owner or compiler workload was run for this candidate.

A retained graph can remain visible while its destination page read is pending. Reader already chooses the exact departing Place (route and visit), but Map recorded the current snapshot's destination visit alongside the departing graph route. On Back, that mismatched pair failed same_presentation_visit and caused the original graph route to be consumed again, replacing selectedB with routeA. Map.show now receives the displayed Place's actual VisitId. That same bound visit accompanies scene creation and render-time reentry, so neither path can relabel the retained graph from the newer snapshot. The existing native header test now verifies exact scene entity, reading visit and selected node directly.

Pinned cards deliberately retain explicit Focus and Open actions while the graph is hidden: peek_action has a hidden Focus route and open distinguishes Peek from visible-only Graph origin. The outer callback nevertheless rejected every hidden action. Remove only that contradictory visibility condition. Each callback additionally captures its originating GraphView EntityId at installation and checks that exact retained scene before capturing dispatch context and again after defer. Existing route/read-visit/open-generation/authority/owner attachment checks and peek_action owner/overlay/node checks remain. A stale retained GraphView cannot operate a replacement even at the same root. The original pinned-card test keeps its Focus/Open/no-exact-match assertions and adds rejection after publication retires the hidden scene.

The publication test's terminal paint assertion inspected the painted buffer after draining effects without drawing an ordinary platform frame. Its fixture returns the exact same symbol/package with changed docs, Reader watches that key, and the independent render counter already advanced. The test now draws normally before inspecting initial and renewed words. Rig.draw calls window.draw without window.refresh; it cannot bypass the cached Reader's notification requirement as Rig.repaint would. The Resource render-count/AXframe requirements, queued GraphFocus ordering, After-publication words and unchanged snapshot/observation assertions remain intact. No Reader/store invalidation production policy was changed for this fixture correction.

Focused native selectors (backend-desktop --lib; retain the existing admitted feature/profile graph):

- shell::graph_tests::graph_header_requests_the_exact_selection_at_native_activation_time
- shell::tests::retained_pinned_card_actions_reveal_or_open_the_cards_exact_node
- shell::tests::publication::graph_context_event_cannot_consume_a_queued_mounted_page_landing

Relevant existing continuity guards for a later broader batch:

- shell::graph_tests::publication_while_a_selected_declaration_is_open_preserves_the_graph_history_visit
- shell::graph_tests::a_fresh_graph_visit_cannot_restore_a_hidden_history_visits_presentation

Validation at this checkpoint: rustfmt2024 parse-only on all five Rust paths and git diff --check pass. Adversarial source review is recorded in the source packet. Native compilation and all test bodies remain UNRUN for the repaired head; source inspection is not actual GUI acceptance. No changes to main checkout, Nix, frozen Native35 source, or retained images.
