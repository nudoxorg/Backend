//! Exercises the `interface-mcp` schema-card contract through its observable boundary.
//! The cases target example decoding, vocabulary coverage, and drift against the shared registry.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
//!
//! The card is the one document an agent reads instead of nine long tool descriptions, so a
//! sentence in it that is no longer true is worse than a missing one. Every worked call it prints
//! is pushed through [`interface_mcp::arguments::decode`] — the same decoder the server uses — and
//! every relation spelling it names is checked against the shared vocabulary.

use std::error::Error;

use interface_documents::Direction;
use interface_library::{COMMANDS, Command};
use interface_mcp::{arguments, card};
use interface_search::{LINK_KINDS, parse_relation, relation_label};
use serde_json::{Map, Value};

/// One worked call as the card prints it.
struct WorkedCall {
    tool: String,
    arguments: Map<String, Value>,
}

fn worked_calls(card: &str) -> Result<Vec<WorkedCall>, Box<dyn Error>> {
    card::worked_examples(card)
        .into_iter()
        .map(|example| {
            let value: Value = serde_json::from_str(example)?;
            let tool = value
                .get("tool")
                .and_then(Value::as_str)
                .ok_or("a worked call has no `tool`")?
                .to_owned();
            let arguments = match value.get("arguments") {
                Some(Value::Object(object)) => object.clone(),
                _ => return Err("a worked call has no `arguments` object".into()),
            };
            Ok(WorkedCall { tool, arguments })
        })
        .collect()
}

#[test]
fn every_worked_call_decodes_through_the_server_s_own_decoder() -> Result<(), Box<dyn Error>> {
    let card = card::render();
    let calls = worked_calls(&card)?;
    assert!(!calls.is_empty(), "the card carries worked calls");
    for call in &calls {
        let command = arguments::decode(&call.tool, &call.arguments).map_err(|fault| {
            format!(
                "the card's `{}` example does not decode: {} {} — {}",
                call.tool, fault.slug, fault.operand, fault.detail
            )
        })?;
        let spec = interface_library::spec(command.id());
        assert_eq!(
            spec.name, call.tool,
            "a worked call decoded into a different command than it names"
        );
    }
    Ok(())
}

#[test]
fn the_worked_calls_exercise_the_three_tools_an_agent_reaches_for_first(
) -> Result<(), Box<dyn Error>> {
    let card = card::render();
    let calls = worked_calls(&card)?;
    let tools: Vec<&str> = calls.iter().map(|call| call.tool.as_str()).collect();
    assert_eq!(tools, ["search", "show", "graph"]);
    Ok(())
}

#[test]
fn the_search_example_actually_narrows_what_it_claims_to_narrow() -> Result<(), Box<dyn Error>> {
    let card = card::render();
    let calls = worked_calls(&card)?;
    let search = calls
        .iter()
        .find(|call| call.tool == "search")
        .ok_or("no search example")?;
    let Command::Search(request) = arguments::decode("search", &search.arguments)? else {
        return Err("the search example is not a search".into());
    };
    assert_eq!(request.text.as_str(), "deserialize map");
    assert!(
        request
            .scope
            .kinds
            .contains(compiler_ir_vocabulary::EntityKind::Function)
    );
    assert!(
        !request
            .scope
            .kinds
            .contains(compiler_ir_vocabulary::EntityKind::Record),
        "naming kinds must exclude the ones not named"
    );
    assert_eq!(request.limit.get(), 10);
    Ok(())
}

#[test]
fn the_graph_example_carries_a_direction_the_shared_vocabulary_parses(
) -> Result<(), Box<dyn Error>> {
    let card = card::render();
    let calls = worked_calls(&card)?;
    let graph = calls
        .iter()
        .find(|call| call.tool == "graph")
        .ok_or("no graph example")?;
    let Command::Graph(request) = arguments::decode("graph", &graph.arguments)? else {
        return Err("the graph example is not a graph".into());
    };
    assert_eq!(
        request.direction,
        Direction::Incoming,
        "the example must show that a relation spelling carries its direction"
    );
    assert!(request.kind.is_some());
    Ok(())
}

#[test]
fn the_card_names_every_relation_in_both_directions_and_nothing_else() -> Result<(), Box<dyn Error>>
{
    let card = card::render();
    for kind in LINK_KINDS {
        for direction in [Direction::Outgoing, Direction::Incoming] {
            let label = relation_label(kind, direction);
            assert!(
                card.contains(&format!("`{label}`")),
                "the card never spells `{label}`"
            );
            assert_eq!(
                parse_relation(label),
                Some((kind, direction)),
                "the card spells a relation the parser does not accept"
            );
        }
    }
    Ok(())
}

#[test]
fn the_card_states_the_rules_a_row_can_never_teach() -> Result<(), Box<dyn Error>> {
    let card = card::render();
    for rule in [
        "never accepted as input",
        "Pass it back **verbatim**",
        "is not loaded here",
        "before concluding nothing exists",
        "no action available",
    ] {
        assert!(card.contains(rule), "the card omits the rule: {rule}");
    }
    Ok(())
}

#[test]
fn no_tool_description_restates_what_the_card_owns() -> Result<(), Box<dyn Error>> {
    for row in interface_mcp::tools::descriptors() {
        assert!(
            !row.description.contains("```"),
            "{} smuggles a code block into its description",
            row.name
        );
        assert!(
            row.description.len() < 260,
            "{} is long enough that it belongs in the card: {} bytes",
            row.name,
            row.description.len()
        );
    }
    assert_eq!(interface_mcp::tools::descriptors().len(), COMMANDS.len());
    Ok(())
}
