//! One command call, and the typed engine request it lowers to.
//!
//! Both surfaces reach the same thirty-five registry rows, and both must
//! validate an operand the same way — a package reference that the CLI accepts
//! and the MCP refuses is a parity bug waiting to be discovered by a user. So
//! neither surface owns this: an [`Invocation`] is "which registry row, with
//! which operands", built from `argv` by the CLI and from a JSON arguments
//! object by the MCP, and [`lower`] turns it into the one typed [`Request`]
//! both execute.
//!
//! Nothing here performs I/O, and nothing here renders. An operand the engine
//! cannot admit fails with a [`Fault`] naming that exact operand.

use crate::fault::{Fault, Operand};
use crate::grammar::{ArgumentKind, ArgumentSpec, CommandGrammar, grammar_for};
use backend_library::{
    CommandId, PackageCoordinate, PackageReference, ProductText, ProjectName, ProjectSelector,
    SemanticGenerationId, SemanticLanguageProfile, SurfaceCommand, TreeNodeId, TreeOpener,
    TreeSubject, decode_id,
};
use std::collections::BTreeMap;
use std::num::NonZeroU64;

/// The escape hatch that takes one tagged `SurfaceCommand` JSON object.
pub const SURFACE_VERB: &str = "surface";

/// One parsed command call: which registry row, and its operands.
#[derive(Clone, Debug)]
pub struct Invocation {
    grammar: CommandGrammar,
    positional: Vec<String>,
    named: BTreeMap<&'static str, String>,
    flags: Vec<&'static str>,
}

impl Invocation {
    /// Starts one call against a registry row.
    #[must_use]
    pub const fn new(grammar: CommandGrammar) -> Self {
        Self {
            grammar,
            positional: Vec::new(),
            named: BTreeMap::new(),
            flags: Vec::new(),
        }
    }

    /// Appends one positional operand.
    pub fn push(&mut self, value: impl Into<String>) {
        self.positional.push(value.into());
    }

    /// Sets one named option.
    pub fn set(&mut self, name: &'static str, value: impl Into<String>) {
        self.named.insert(name, value.into());
    }

    /// Raises one flag.
    pub fn raise(&mut self, name: &'static str) {
        if !self.flags.contains(&name) {
            self.flags.push(name);
        }
    }

    /// Returns the registry row's calling convention.
    #[must_use]
    pub const fn grammar(&self) -> CommandGrammar {
        self.grammar
    }

    /// Returns the positional operands in grammar order.
    #[must_use]
    pub fn positional(&self) -> &[String] {
        &self.positional
    }

    /// Returns one positional operand by index.
    #[must_use]
    pub fn at(&self, index: usize) -> Option<&str> {
        self.positional.get(index).map(String::as_str)
    }

    /// Returns one positional operand, or a usage fault naming what is missing.
    ///
    /// # Errors
    ///
    /// Returns a usage fault when the operand was not supplied.
    pub fn require(&self, index: usize) -> Result<&str, Fault> {
        let spec = self.grammar.positional().get(index);
        self.at(index).ok_or_else(|| {
            Fault::usage(
                spec.map_or(self.grammar.name(), |spec| spec.name()),
                format!("{} takes {}", self.grammar.name(), self.grammar.usage()),
            )
        })
    }

    /// Returns one named option's value.
    #[must_use]
    pub fn option(&self, name: &str) -> Option<&str> {
        self.named.get(name).map(String::as_str)
    }

    /// Returns whether one flag was supplied.
    #[must_use]
    pub fn flag(&self, name: &str) -> bool {
        self.flags.contains(&name)
    }

    /// Checks the operand count against the grammar.
    ///
    /// # Errors
    ///
    /// Returns a usage fault carrying the exact usage line.
    pub fn check(&self) -> Result<(), Fault> {
        let specs = self.grammar.positional();
        let required = specs
            .iter()
            .filter(|spec| spec.is_required() && !spec.is_repeated())
            .count();
        let repeated = specs.iter().any(|spec| spec.is_repeated());
        let supplied = self.positional.len();
        let too_few = supplied < required || (repeated && supplied == 0);
        let too_many = !repeated && supplied > specs.len();
        if too_few || too_many {
            return Err(Fault::usage(
                self.grammar.name(),
                format!("usage: backend {}", self.grammar.usage()),
            ));
        }
        Ok(())
    }

    /// Builds one call from a JSON arguments object keyed by operand name.
    ///
    /// An MCP client names its arguments; a person types them in order. This
    /// is the only difference between the two surfaces' calling conventions,
    /// and it is resolved here rather than in two lowerings.
    ///
    /// # Errors
    ///
    /// Returns a usage fault naming an unknown field, a field of the wrong
    /// JSON type, or a missing required operand.
    pub fn from_json(
        grammar: CommandGrammar,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Self, Fault> {
        let mut call = Self::new(grammar);
        for field in arguments.keys() {
            if !grammar
                .positional()
                .iter()
                .chain(grammar.options())
                .any(|spec| spec.name() == field)
            {
                return Err(Fault::usage(
                    field.clone(),
                    format!("{} takes no argument called `{field}`", grammar.name()),
                ));
            }
        }
        for spec in grammar.positional() {
            take_json(&mut call, *spec, arguments, true)?;
        }
        for spec in grammar.options() {
            take_json(&mut call, *spec, arguments, false)?;
        }
        call.check()?;
        Ok(call)
    }
}

fn take_json(
    call: &mut Invocation,
    spec: ArgumentSpec,
    arguments: &serde_json::Map<String, serde_json::Value>,
    positional: bool,
) -> Result<(), Fault> {
    let Some(value) = arguments.get(spec.name()) else {
        return Ok(());
    };
    if spec.kind() == ArgumentKind::Flag {
        if value.as_bool().unwrap_or(false) {
            call.raise(spec.name());
        }
        return Ok(());
    }
    let scalars = match value {
        serde_json::Value::Array(values) if spec.is_repeated() => values.clone(),
        other => vec![other.clone()],
    };
    for scalar in scalars {
        let text = scalar_text(&scalar).ok_or_else(|| {
            Fault::usage(
                spec.name(),
                format!("`{}` must be a {}", spec.name(), spec.kind().json_type()),
            )
        })?;
        if positional {
            call.push(text);
        } else {
            call.set(spec.name(), text);
        }
    }
    Ok(())
}

fn scalar_text(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Number(number) => Some(number.to_string()),
        serde_json::Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

/// Finds the registry row one spelling names.
#[must_use]
pub fn row_for(spelling: &str) -> Option<CommandGrammar> {
    grammar_for(spelling)
}

/// Default page bound when the caller did not ask for one.
pub const DEFAULT_LIMIT: u16 = 25;

/// One typed request, and the extra probes its rendering needs.
#[derive(Clone, Debug)]
pub enum Request {
    /// Every project on the shelf, with readiness.
    Shelf,
    /// The engine's whole state, rolled up.
    Status,
    /// Submit an index intent for one project path.
    Index(String),
    /// Submit a remove intent for one project path.
    Remove(String),
    /// One declaration page: document, members, relations, source.
    Page(String),
    /// One declaration's captured source only.
    Source(String),
    /// One declaration's bounded neighbourhood.
    Neighbourhood {
        /// Coordinate at the centre.
        coordinate: String,
        /// Whether incoming relations are included.
        incoming: bool,
    },
    /// A bounded search page.
    Search {
        /// Query text.
        text: String,
        /// Page bound.
        limit: u16,
    },
    /// A bounded name resolution page.
    Resolve {
        /// Name text.
        text: String,
        /// Page bound.
        limit: u16,
    },
    /// One package outline, resolved to names.
    Outline(String),
    /// One durable product operation.
    Surface(Box<SurfaceCommand>),
}

/// Lowers one invocation into a typed request.
///
/// # Errors
///
/// Returns a typed fault naming the operand the engine cannot admit.
pub fn lower(invocation: &Invocation, project: &str) -> Result<Request, Fault> {
    let grammar = invocation.grammar();
    let Some(spec) = grammar.spec() else {
        return Err(Fault::usage(grammar.name(), "this command has no registry row"));
    };
    match spec.id {
        CommandId::Packages => Ok(Request::Shelf),
        CommandId::Health | CommandId::Revision => Ok(Request::Status),
        CommandId::Add => Ok(Request::Index(path_or(invocation, project))),
        CommandId::Remove => Ok(Request::Remove(invocation.require(0)?.to_owned())),
        CommandId::Show | CommandId::Document => {
            Ok(Request::Page(invocation.require(0)?.to_owned()))
        }
        CommandId::Source => Ok(Request::Source(invocation.require(0)?.to_owned())),
        CommandId::Related => Ok(Request::Neighbourhood {
            coordinate: invocation.require(0)?.to_owned(),
            incoming: true,
        }),
        CommandId::Graph | CommandId::GraphQuery => Ok(Request::Neighbourhood {
            coordinate: invocation.require(0)?.to_owned(),
            incoming: false,
        }),
        CommandId::Search => Ok(Request::Search {
            text: invocation.require(0)?.to_owned(),
            limit: limit(invocation)?,
        }),
        CommandId::Resolve | CommandId::Name => Ok(Request::Resolve {
            text: invocation.require(0)?.to_owned(),
            limit: limit(invocation)?,
        }),
        CommandId::Outline => Ok(Request::Outline(path_or(invocation, project))),
        _ => surface(invocation, spec.id).map(|command| Request::Surface(Box::new(command))),
    }
}

/// Lowers the JSON escape hatch, which takes one tagged surface object.
///
/// # Errors
///
/// Returns a typed fault when the object is not an admitted surface command.
pub fn lower_surface_json(encoded: &str) -> Result<Request, Fault> {
    let command = serde_json::from_str::<SurfaceCommand>(encoded).map_err(|error| {
        Fault::usage(SURFACE_VERB, format!("that is not a surface command: {error}"))
    })?;
    command
        .admit()
        .map_err(|error| Fault::admission(error, Operand::Argument(SURFACE_VERB.to_owned())))?;
    Ok(Request::Surface(Box::new(command)))
}

fn path_or(invocation: &Invocation, project: &str) -> String {
    invocation
        .at(0)
        .map_or_else(|| project.to_owned(), ToOwned::to_owned)
}

fn limit(invocation: &Invocation) -> Result<u16, Fault> {
    let Some(value) = invocation.option("limit") else {
        return Ok(DEFAULT_LIMIT);
    };
    value
        .parse::<u16>()
        .ok()
        .filter(|limit| *limit > 0 && *limit <= 200)
        .ok_or_else(|| {
            Fault::usage(
                "limit",
                format!("`{value}` is not a page bound; choose a whole number from 1 to 200"),
            )
        })
}

fn text(invocation: &Invocation, index: usize) -> Result<ProductText, Fault> {
    let value = invocation.require(index)?;
    ProductText::new(value)
        .map_err(|error| Fault::admission(error, Operand::Argument(value.to_owned())))
}

fn package(invocation: &Invocation, index: usize) -> Result<PackageReference, Fault> {
    let value = invocation.require(index)?;
    PackageReference::parse(value)
        .map_err(|error| Fault::admission(error, Operand::Argument(value.to_owned())))
}

fn selector(invocation: &Invocation, index: usize) -> Result<ProjectSelector, Fault> {
    let value = invocation.require(index)?;
    ProjectSelector::parse(value)
        .map_err(|error| Fault::admission(error, Operand::Argument(value.to_owned())))
}

fn optional_text(invocation: &Invocation, name: &str) -> Result<Option<ProductText>, Fault> {
    invocation.option(name).map_or(Ok(None), |value| {
        ProductText::new(value)
            .map(Some)
            .map_err(|error| Fault::admission(error, Operand::Argument(value.to_owned())))
    })
}

fn node(invocation: &Invocation, index: usize) -> Result<TreeNodeId, Fault> {
    let value = invocation.require(index)?;
    value
        .parse::<u64>()
        .ok()
        .and_then(NonZeroU64::new)
        .map(TreeNodeId::new)
        .ok_or_else(|| {
            Fault::usage(
                "node",
                format!("`{value}` is not a session tree node; node identities are positive whole numbers"),
            )
        })
}

fn option_node(invocation: &Invocation, name: &str) -> Result<Option<TreeNodeId>, Fault> {
    invocation.option(name).map_or(Ok(None), |value| {
        value
            .parse::<u64>()
            .ok()
            .and_then(NonZeroU64::new)
            .map(TreeNodeId::new)
            .map(Some)
            .ok_or_else(|| {
                Fault::usage(
                    name,
                    format!("`{value}` is not a session tree node identity"),
                )
            })
    })
}

fn surface(invocation: &Invocation, id: CommandId) -> Result<SurfaceCommand, Fault> {
    let command = match id {
        CommandId::Read => SurfaceCommand::Read {
            locators: locators(invocation)?,
        },
        CommandId::Diff => SurfaceCommand::Diff {
            from: package(invocation, 0)?,
            to: package(invocation, 1)?,
        },
        CommandId::Explore => SurfaceCommand::Explore {
            query: invocation
                .at(0)
                .map(|value| {
                    ProductText::new(value)
                        .map_err(|error| Fault::admission(error, Operand::Argument(value.to_owned())))
                })
                .transpose()?,
            limit: limit(invocation)?,
        },
        CommandId::Package => SurfaceCommand::Package {
            package: package(invocation, 0)?,
        },
        CommandId::Dependents => SurfaceCommand::Dependents {
            package: package(invocation, 0)?,
        },
        CommandId::Owner => SurfaceCommand::Owner {
            owner: text(invocation, 0)?,
        },
        CommandId::IndexSearch => SurfaceCommand::IndexSearch {
            query: text(invocation, 0)?,
            limit: limit(invocation)?,
        },
        CommandId::PackageVersions => SurfaceCommand::PackageVersions {
            package: package(invocation, 0)?,
        },
        CommandId::SemanticVersions => SurfaceCommand::SemanticVersions {
            package: package(invocation, 0)?,
        },
        CommandId::SelectSemanticVersion => select_semantic_version(invocation)?,
        CommandId::PackageProfile => SurfaceCommand::PackageProfile {
            package: package(invocation, 0)?,
        },
        _ => return home_or_session(invocation, id),
    };
    admit(&command)
}

fn home_or_session(invocation: &Invocation, id: CommandId) -> Result<SurfaceCommand, Fault> {
    let command = match id {
        CommandId::Subscribe => SurfaceCommand::Subscribe {
            package: package(invocation, 0)?,
            project: invocation
                .option("project")
                .map(|value| {
                    ProjectSelector::parse(value).map_err(|error| {
                        Fault::admission(error, Operand::Argument(value.to_owned()))
                    })
                })
                .transpose()?,
        },
        CommandId::Unsubscribe => SurfaceCommand::Unsubscribe {
            package: package(invocation, 0)?,
        },
        CommandId::Subscriptions => SurfaceCommand::Subscriptions,
        CommandId::Releases => SurfaceCommand::Releases {
            mark_seen: invocation.flag("mark-seen"),
        },
        CommandId::Projects => SurfaceCommand::Projects,
        CommandId::ProjectCreate => SurfaceCommand::ProjectCreate {
            name: project_name(invocation)?,
            lockfile: optional_text(invocation, "lockfile")?,
        },
        CommandId::ProjectDelete => SurfaceCommand::ProjectDelete {
            project: selector(invocation, 0)?,
        },
        CommandId::ProjectAdd => SurfaceCommand::ProjectAdd {
            project: selector(invocation, 0)?,
            package: package(invocation, 1)?,
        },
        CommandId::ProjectRemove => SurfaceCommand::ProjectRemove {
            project: selector(invocation, 0)?,
            package: package(invocation, 1)?,
        },
        CommandId::ProjectSync => SurfaceCommand::ProjectSync {
            project: selector(invocation, 0)?,
        },
        CommandId::Tree => SurfaceCommand::Tree,
        CommandId::TreeOpen => SurfaceCommand::TreeOpen {
            subject: subject(invocation)?,
            parent: option_node(invocation, "parent")?,
            title: optional_text(invocation, "title")?,
            opener: TreeOpener::Cli,
        },
        CommandId::TreeClose => SurfaceCommand::TreeClose {
            node: node(invocation, 0)?,
            branch: invocation.flag("branch"),
        },
        _ => {
            return Err(Fault::usage(
                invocation.grammar().name(),
                "this registry row has no lowering on this surface",
            ));
        }
    };
    admit(&command)
}

fn admit(command: &SurfaceCommand) -> Result<SurfaceCommand, Fault> {
    command
        .admit()
        .map(|()| command.clone())
        .map_err(|error| Fault::admission(error, Operand::Text(format!("{:?}", command.id()))))
}

fn locators(invocation: &Invocation) -> Result<Box<[ProductText]>, Fault> {
    invocation
        .positional()
        .iter()
        .map(|value| {
            ProductText::new(value.clone())
                .map_err(|error| Fault::admission(error, Operand::Argument(value.clone())))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Vec::into_boxed_slice)
}

fn project_name(invocation: &Invocation) -> Result<ProjectName, Fault> {
    let value = invocation.require(0)?;
    ProjectName::new(value)
        .map_err(|error| Fault::admission(error, Operand::Argument(value.to_owned())))
}

fn subject(invocation: &Invocation) -> Result<TreeSubject, Fault> {
    let kind = invocation.require(0)?.to_owned();
    let value = invocation.require(1)?.to_owned();
    let admitted = |value: &str| {
        ProductText::new(value)
            .map_err(|error| Fault::admission(error, Operand::Argument(value.to_owned())))
    };
    match kind.as_str() {
        "package" => PackageReference::parse(value.clone())
            .map(TreeSubject::Package)
            .map_err(|error| Fault::admission(error, Operand::Argument(value))),
        "declaration" => admitted(&value).map(TreeSubject::Declaration),
        "explore" => admitted(&value).map(|text| TreeSubject::Explore(Some(text))),
        "search" => admitted(&value).map(TreeSubject::Search),
        "owner" => admitted(&value).map(TreeSubject::Owner),
        other => Err(Fault::usage(
            "subject",
            format!(
                "`{other}` is not a subject; choose package, declaration, explore, search, or owner"
            ),
        )),
    }
}

fn select_semantic_version(invocation: &Invocation) -> Result<SurfaceCommand, Fault> {
    let package = package(invocation, 0)?;
    let raw = invocation.require(1)?;
    let coordinate = PackageCoordinate::parse(raw.to_owned())
        .map_err(|_| Fault::usage("coordinate", format!("`{raw}` is not a pinned package URL")))?;
    let profile_name = invocation.require(2)?;
    let profile = SemanticLanguageProfile::from_name(profile_name).ok_or_else(|| {
        Fault::usage(
            "profile",
            format!(
                "`{profile_name}` is not a profile; choose {}",
                SemanticLanguageProfile::names().join(", ")
            ),
        )
    })?;
    let generation = invocation.require(3)?;
    let bytes = decode_id(generation).map_err(|error| {
        Fault::usage(
            "generation",
            format!("`{generation}` is not a generation identity: {error}"),
        )
    })?;
    Ok(SurfaceCommand::SelectSemanticVersion {
        package,
        coordinate,
        profile,
        generation: SemanticGenerationId::new(bytes),
    })
}
