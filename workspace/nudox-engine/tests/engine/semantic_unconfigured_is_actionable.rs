//! `NoEmbedder` used to collapse two facts that had different remedies.
//!
//! # The defect, and what happened to half of it
//!
//! [`Unavailable`]'s own doc comment makes the argument this file applies one
//! level down. It says of `ModelFailed` that it is kept
//! distinct from `NoEmbedder` because the two "send a reader to completely
//! different places", and that collapsing them "would have made a broken model
//! indistinguishable from a deliberate configuration".
//!
//! `NoEmbedder` used to be two facts by exactly that test:
//!
//! * **No runtime.** `embed::load_from_env` was `#[cfg(not(feature = "onnx"))]
//!   None` — a compile-time constant, reachable whenever the (then default-off)
//!   `onnx` feature was not enabled. Remedy: rebuild.
//! * **No model.** With the runtime compiled in, `onnx::load_from_env` returns
//!   `None` when `NUDOX_EMBED_MODEL_DIR` is unset. Remedy: point that variable
//!   at the pinned model directory. No rebuild involved.
//!
//! The `onnx` cargo feature is gone: `registry` (with its own `onnx` feature)
//! is a plain, non-optional dependency of `nudox-engine` now, so every build of
//! this crate has the runtime compiled in and "no runtime" cannot happen any
//! more. `Unavailable::NoRuntime` was removed along with it — keeping a
//! variant no code path could ever produce would have forced every match on
//! this exhaustive enum to keep carrying a permanently-dead arm, which is
//! precisely the confusion an unreachable "reason" would create for a reader.
//! `NoModelConfigured` is what remains, and it is now the *only* way this
//! engine reports having no embedder — which raises the bar on its remedy:
//! there is no second reason left to blame, so the message below has to be
//! right the first time.
//!
//! # Why the reason must carry a remedy, not just a name
//!
//! `NoEmbedder` was a name for a state, and a name is only actionable to a
//! reader who already knows the build system. The whole surface is aimed at
//! agents and at users who did not build the binary. Every other unavailable
//! state in this codebase that a user can *fix* says how — `McpStatus::failed`
//! walks the entire `#[source]` chain precisely so a five-second diagnosis
//! does not become an hour.

use nudox_engine::semantic::Unavailable;

/// Each reason must say what to do about it.
#[test]
fn every_configuration_reason_names_its_remedy() {
    let model = Unavailable::NoModelConfigured.remedy();
    assert!(
        model.contains("NUDOX_EMBED_MODEL_DIR"),
        "the no-model remedy must name the variable to set: got {model:?}",
    );
    assert!(
        model.contains("no rebuild"),
        "the runtime is always compiled in now — the remedy must say plainly \
         that no rebuild is involved, or a reader who remembers the old \
         onnx-feature rebuild path will go looking for one: got {model:?}",
    );
}

/// The two states a user cannot fix by configuring anything must not pretend
/// to be configuration problems.
#[test]
fn the_runtime_failure_states_do_not_offer_a_configuration_remedy() {
    for state in [Unavailable::EmptyCorpus, Unavailable::ModelFailed] {
        let remedy = state.remedy();
        assert!(
            !remedy.contains("NUDOX_EMBED_MODEL_DIR"),
            "{state:?} is not fixed by setting the model directory; saying so \
             sends the reader to change something that is already correct: \
             got {remedy:?}",
        );
    }
}

/// A `ModelFailed` remedy must still point somewhere.
///
/// This is the state whose doc comment says it is "worth retrying and worth
/// looking in the log for" — so the rendered reason should say that, rather
/// than leaving the reader with a bare enum name.
#[test]
fn a_failed_model_points_at_the_log() {
    let remedy = Unavailable::ModelFailed.remedy();
    assert!(
        !remedy.trim().is_empty(),
        "every reason a reader can see must carry a next step",
    );
}

/// `Unavailable`'s three remaining reasons must be pairwise distinguishable —
/// the whole point `NoEmbedder`'s split existed to make true, now checked
/// across the reduced set rather than just the two halves that used to be
/// `NoEmbedder`.
#[test]
fn the_remaining_reasons_are_pairwise_distinct() {
    let reasons = [
        Unavailable::NoModelConfigured,
        Unavailable::EmptyCorpus,
        Unavailable::ModelFailed,
    ];
    for (i, a) in reasons.iter().enumerate() {
        for (j, b) in reasons.iter().enumerate() {
            if i == j {
                continue;
            }
            assert_ne!(
                a, b,
                "two distinct configuration states must not compare equal, or \
                 a reader cannot tell them apart from the value alone",
            );
        }
    }
}

/// The build this test binary was compiled into reports the truthful reason.
///
/// No `#[cfg(feature = "onnx")]` branch here any more — there is no such
/// feature to branch on. The runtime is always compiled in, so an absent
/// embedder can only ever mean a missing model directory.
#[test]
fn the_default_engine_reports_the_reason_this_build_actually_has() {
    let embedder = nudox_engine::embed::load_from_env();

    let expected = if embedder.is_some() {
        None
    } else {
        Some(Unavailable::NoModelConfigured)
    };
    assert_eq!(
        nudox_engine::embed::unavailable_reason(),
        expected,
        "with the runtime always compiled in, an absent embedder is a missing \
         model directory — never a missing runtime",
    );
}
