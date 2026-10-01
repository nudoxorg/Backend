//! J11: every row of the key table, pressed by a person where it means
//! something, with what it did checked on screen. Generated from
//! `shell::keys::TABLE`: each row's own chord is pressed, and each command
//! has one [`Case`] (an exhaustive `match`, so a command added to the table
//! does not build until J11 says what it does).
//!
//! The walk starts on the Library of a real install of toml_pin with the
//! packages its lock pins (`install-all`), goes to toml's `Value` by the
//! pointer (its chip, its `value` module, the card) for the keys that need a
//! declaration, and comes back by keys. Every navigation that is not the key under test is itself
//! checked, so a case never judges a key from the wrong place.

use super::parts::Parts;
use super::plan::{Builder, Plan};
use super::script;
use crate::shell::{KEY_TABLE, KeyCommand};

/// Where a case's key means something.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Place {
    /// The Library, nothing focused, no transient open.
    Library,
    /// toml's `Value`, as a page.
    Value,
}

/// What pressing one command's key is checked against.
struct Case {
    /// Where it is pressed.
    place: Place,
    /// Lines that set the context up, after arriving at the place.
    setup: &'static [&'static str],
    /// What must be true after the key.
    then: &'static [&'static str],
    /// What must be true after pressing the same key once more (a toggle),
    /// when it is one.
    again: &'static [&'static str],
    /// Lines that put the place back as it was.
    after: &'static [&'static str],
}

const NONE: &[&str] = &[];

fn case(command: KeyCommand) -> Case {
    use KeyCommand as C;
    use Place::{Library, Value};
    let (place, setup, then, again, after): (Place, &[&str], &[&str], &[&str], &[&str]) = match command {
        C::AddFolder => (Library, NONE, &["state overlay \"add-project\"", "text \"Choose folder…\""], NONE, &["key escape"]),
        C::OpenSettings => (Library, NONE, &["state overlay \"settings*\"", "text \"Theme\" in reader"], NONE, &["key escape"]),
        C::Ask => (Library, NONE, &["state ask \"open\""], NONE, &["key escape"]),
        C::Escape => (Library, &["key cmd-k"], &["state ask \"closed\"", "state overlay \"none\""], NONE, NONE),
        C::ZoomIn => (Library, NONE, &["state text \"110\""], NONE, &["key cmd-0"]),
        C::ZoomOut => (Library, NONE, &["state text \"90\""], NONE, &["key cmd-0"]),
        C::ZoomReset => (Library, &["key cmd-="], &["state text \"100\""], NONE, NONE),
        C::ToggleShelf => (Library, NONE, &["state shelf \"spine\""], &["state shelf \"shelf\""], NONE),
        C::Zen => (Library, NONE, &["state zen \"on\"", "state shelf \"hidden\""], &["state zen \"off\"", "state shelf \"shelf\""], NONE),
        C::NextZone => (Library, NONE, &["state zone \"titlebar\""], NONE, &["key shift-tab"]),
        C::PrevZone => (Library, NONE, &["state zone \"shelf\""], NONE, &["key tab"]),
        C::FocusNext => (Library, NONE, &["state zone \"reader\"", "state focus \"orbit-project-*\""], NONE, &["key escape"]),
        C::FocusPrev => (Library, &["key j", "key j"], &["state focus \"orbit-project-*\""], NONE, &["key escape"]),
        C::Activate => (Library, &["key j"], &["route like \"package *toml_pin\""], NONE, &["key ctrl-1"]),
        C::HintMode => (Library, NONE, &["state hints \"open\""], &["state hints \"closed\""], NONE),
        C::Back => (Library, &["key j", "key enter"], &["route like \"orbit\""], NONE, NONE),
        C::Forward => (Library, &["key j", "key enter", "key cmd-["], &["route like \"package *toml_pin\""], NONE, &["key ctrl-1"]),
        C::DepthOrbit => (Library, &["key j", "key enter"], &["route like \"orbit\""], NONE, NONE),
        C::Surface => (Library, &["key j", "key enter"], &["route like \"orbit\""], NONE, NONE),
        C::CodePage => (Value, NONE, &["route like \"symbol *Value*view=code*\""], &["route like \"symbol *Value*view=page*\""], NONE),
        C::DepthCode => (Value, NONE, &["route like \"symbol *Value*view=code*\""], NONE, &["key ctrl-3"]),
        C::DepthPage => (Value, &["key ctrl-4"], &["route like \"symbol *Value*view=page*\""], NONE, NONE),
        C::DepthPackage => (Value, NONE, &["route like \"package *toml*\""], NONE, &["key cmd-["]),
        C::PeelSource => (Value, NONE, &["route like \"symbol *Value*view=code*\""], NONE, &["key cmd-."]),
        C::Graph => (Value, NONE, &["route like \"symbol *Value*view=graph*\""], &["route like \"symbol *Value*view=page*\""], NONE),
        C::Tour => (Value, NONE, &["route like \"world*\""], NONE, &["key escape", "key cmd-["]),
        C::Peek => (Value, &["key j"], &["state peek \"open\""], NONE, &["key escape", "key escape"]),
        C::CopyAddress => (Value, NONE, &["state clipboard \"nudox://*Value*\""], NONE, NONE),
        C::Hold => (Value, NONE, &["state held \"1\""], NONE, NONE),
        C::OpenHand => (Value, &["key cmd-d"], &["state hand \"open\""], &["state hand \"closed\""], NONE),
        C::HandCard1 => (Value, &["key cmd-d", "key ctrl-1"], &["route like \"symbol *Value*\""], NONE, NONE),
        C::HandCard2 => (Value, &["key cmd-d", "key ctrl-2", "key cmd-d", "key ctrl-1"], &["route like \"package *toml*\"", "state held \"2\""], NONE, NONE),
        // With two cards held there is no third, fourth or fifth: the key
        // goes nowhere and nothing moves.
        C::HandCard3 | C::HandCard4 | C::HandCard5 => (Value, &["key cmd-d", "key ctrl-2", "key cmd-d"], &["route like \"package *toml*\"", "state held \"2\""], NONE, NONE),
    };
    Case { place, setup, then, again, after }
}

/// The journey's spelling of a table chord (`secondary` is ⌘ on this Mac).
fn chord(table: &str) -> String {
    table.replace("secondary", "cmd")
}

/// The words that make a checkpoint name out of a chord.
fn slug(text: &str) -> String {
    text.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect()
}

fn check(plan: &mut Builder<'_>, name: &str, lines: &[&str]) -> Result<(), String> {
    let asserts = lines.iter().map(|line| script::assertion(line).map_err(|error| format!("J11 {name}: `{line}`: {error}"))).collect::<Result<Vec<_>, _>>()?;
    plan.check(name, asserts);
    Ok(())
}

/// Arrives at `place` from anywhere, and says so.
fn arrive(plan: &mut Builder<'_>, place: Place, index: usize) -> Result<(), String> {
    match place {
        Place::Library => {
            plan.step("key escape");
            plan.step("key ctrl-1");
            check(plan, &format!("{index:02}-at-library"), &["route like \"orbit\"", "state overlay \"none\"", "state ask \"closed\""])
        }
        Place::Value => {
            // By the pointer, from the Library: toml's chip, its `value`
            // module, the `Value` card (J9 is the search's own journey).
            plan.step("key escape");
            plan.step("key ctrl-1");
            plan.step("click \"toml\" in reader");
            plan.step("click \"value\" in reader");
            plan.step("click \"Value\" in reader");
            check(plan, &format!("{index:02}-at-value"), &["route like \"symbol *toml*Value*view=page*\"", "state ask \"closed\""])
        }
    }
}

/// J11, built from the key table.
///
/// # Errors
/// A line that does not parse, or a part that does not expand.
pub fn plan(parts: &Parts) -> Result<Plan, String> {
    let mut failure = None;
    let plan = Plan::build("J11", parts, |plan| {
        plan.size(1440, 900).start("from install-all frontends/rust/fixtures/toml_pin");
        for (index, key) in KEY_TABLE.iter().enumerate() {
            let case = case(key.command);
            let pressed = chord(key.chord);
            let name = format!("{index:02}-{}", slug(&pressed));
            let result = (|| {
                arrive(plan, case.place, index)?;
                for line in case.setup {
                    plan.step(line);
                }
                plan.step(&format!("key {pressed}"));
                check(plan, &name, case.then)?;
                if !case.again.is_empty() {
                    plan.step(&format!("key {pressed}"));
                    check(plan, &format!("{name}-again"), case.again)?;
                }
                for line in case.after {
                    plan.step(line);
                }
                Ok::<(), String>(())
            })();
            if let Err(error) = result {
                failure.get_or_insert(error);
            }
        }
    });
    match failure {
        Some(error) => Err(error),
        None => plan,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every row of the table is pressed, by its own chord, and checked.
    #[test]
    fn j11_presses_every_row_of_the_key_table_and_checks_what_it_did() {
        let parts = Parts::load(&Parts::dir()).expect("the parts");
        let plan = plan(&parts).expect("J11 builds");
        let text = plan.steps.iter().map(|step| step.text.clone()).collect::<Vec<_>>();
        for key in KEY_TABLE {
            let pressed = format!("key {}", chord(key.chord));
            assert!(text.iter().any(|line| *line == pressed), "J11 presses `{pressed}` ({:?})", key.command);
        }
        let checks = plan.steps.iter().filter(|step| matches!(step.kind, super::super::script::StepKind::Check { .. })).count();
        assert!(checks >= KEY_TABLE.len() * 2, "a check after each key and at each arrival: {checks}");
    }
}
