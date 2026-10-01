//! The one composition model: a [`Plan`] is a machine to start from and the
//! steps to walk. A `.journey` file ([`Plan::parse`]) and Rust code
//! ([`Plan::build`]) both produce it through the same [`Sink`], so parts,
//! their typed parameters, sites, `needs` gates and cycle detection are one
//! implementation, not two.

use super::parts::{Arg, Input, Parts, bind, substitute};
use super::script::{
    Assert, Gap, GapKind, Origin, Site, Step, StepKind, Tok, assertion, injects, split_else,
    step_kind, tokens,
};
use std::path::{Path, PathBuf};

/// The machine a journey starts on.
#[derive(Clone, Debug)]
pub enum Start {
    /// The harness's fixture index, its roots pre-admitted, booted at a
    /// route (`start orbit`): the scenes' machine. Not an install.
    Fixture {
        /// The boot route, in harness route words.
        route: String,
    },
    /// No state dir, no settings, no projects: the production launch path
    /// on an empty machine (`start clean`).
    Clean,
    /// The end state of running these parts from clean
    /// (`start from install frontends/rust/fixtures/toml_pin`), materialized
    /// once by really running them and cached by key ([`super::state`]).
    From(Recipe),
}

impl Start {
    /// Whether the journey runs on the production launch path.
    #[must_use]
    pub const fn is_production(&self) -> bool {
        !matches!(self, Self::Fixture { .. })
    }
}

/// A state's recipe: part uses, run in order from clean.
#[derive(Clone, Debug)]
pub struct Recipe {
    /// The uses.
    pub uses: Vec<PartUse>,
}

impl Recipe {
    /// Every input the recipe's arguments name (hashed into the key).
    #[must_use]
    pub fn inputs(&self) -> Vec<Input> {
        self.uses
            .iter()
            .flat_map(|used| used.args.iter().filter_map(|(_, arg)| arg.input()))
            .collect()
    }

    /// The plan that materializes this state: the uses, from clean.
    ///
    /// # Errors
    /// A use that no longer expands (parts changed since parsing).
    pub fn plan(&self, parts: &Parts, size: (u32, u32)) -> Result<Plan, String> {
        let mut sink = Sink::new(parts);
        for used in &self.uses {
            sink.line(
                &Origin::at(used.site.clone()),
                &format!("do {} {}", used.part, used.raw),
            )?;
        }
        Ok(Plan {
            name: format!("state {self}"),
            source: used_source(&self.uses),
            size,
            start: Start::Clean,
            steps: sink.finish()?,
        })
    }
}

fn used_source(uses: &[PartUse]) -> PathBuf {
    uses.first()
        .map_or_else(PathBuf::new, |used| used.site.file.to_path_buf())
}

impl std::fmt::Display for Recipe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (index, used) in self.uses.iter().enumerate() {
            if index > 0 {
                f.write_str(" then ")?;
            }
            write!(
                f,
                "{}({})",
                used.part,
                used.raw.split_whitespace().collect::<Vec<_>>().join(", ")
            )?;
        }
        Ok(())
    }
}

/// One use of a part, its arguments bound.
#[derive(Clone, Debug)]
pub struct PartUse {
    /// The part's name.
    pub part: String,
    /// The arguments as written.
    pub raw: String,
    /// The arguments, typed.
    pub args: Vec<(String, Arg)>,
    /// Where it is written.
    pub site: Site,
}

/// A composed journey.
#[derive(Clone, Debug)]
pub struct Plan {
    /// Its name (`J0`).
    pub name: String,
    /// Where it was read from (a Rust file for a built plan).
    pub source: PathBuf,
    /// The window's logical size.
    pub size: (u32, u32),
    /// The machine it starts on.
    pub start: Start,
    /// The steps, parts expanded, in order.
    pub steps: Vec<Step>,
}

/// Assembles lines into steps: checks gather their indented asserts, `needs`
/// gates the next step, `do` expands a part through this same sink.
pub struct Sink<'a> {
    parts: &'a Parts,
    steps: Vec<Step>,
    needs: Option<(Gap, Origin)>,
    /// The parts being expanded, outermost first, with their use sites.
    using: Vec<(String, Site)>,
}

impl<'a> Sink<'a> {
    /// An empty sink over `parts`.
    #[must_use]
    pub const fn new(parts: &'a Parts) -> Self {
        Self {
            parts,
            steps: Vec::new(),
            needs: None,
            using: Vec::new(),
        }
    }

    /// The steps, once every line is in.
    ///
    /// # Errors
    /// A `needs` with no step after it.
    pub fn finish(self) -> Result<Vec<Step>, String> {
        if let Some((_, origin)) = self.needs {
            return Err(format!(
                "{origin}: `needs` gates the step after it, and there is none"
            ));
        }
        Ok(self.steps)
    }

    fn fail(origin: &Origin, raw: &str, message: &str) -> String {
        format!("{origin}: {message}\n    {}", raw.trim())
    }

    /// One line (not a header) written at `origin`.
    ///
    /// # Errors
    /// The line, a part it uses or a part that part uses does not parse; a
    /// part cycle; a stand-in act outside a detour.
    pub fn line(&mut self, origin: &Origin, raw: &str) -> Result<(), String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            return Ok(());
        }
        if raw.starts_with(char::is_whitespace) {
            let Some(Step {
                kind: StepKind::Check { asserts, .. },
                ..
            }) = self.steps.last_mut()
            else {
                return Err(Self::fail(
                    origin,
                    raw,
                    "an indented assert belongs under a `check NAME`",
                ));
            };
            asserts.push(assertion(trimmed).map_err(|message| Self::fail(origin, raw, &message))?);
            return Ok(());
        }
        let (verb, rest) = trimmed.split_once(' ').unwrap_or((trimmed, ""));
        match verb {
            "size" | "start" => Err(Self::fail(
                origin,
                raw,
                "`size` and `start` are journey headers: a part cannot set them",
            )),
            "do" => self.expand(origin, raw, rest.trim()),
            "needs" => {
                let gap = needs(rest).map_err(|message| Self::fail(origin, raw, &message))?;
                if self.needs.is_some() {
                    return Err(Self::fail(
                        origin,
                        raw,
                        "two `needs` in a row: one gates one step",
                    ));
                }
                self.needs = Some((gap, origin.clone()));
                Ok(())
            }
            "check" => {
                if rest.trim().is_empty() {
                    return Err(Self::fail(origin, raw, "`check` needs a name"));
                }
                self.push(
                    origin,
                    trimmed,
                    StepKind::Check {
                        name: rest.trim().to_owned(),
                        asserts: Vec::new(),
                    },
                    None,
                );
                Ok(())
            }
            _ => {
                let (main, otherwise) = split_else(trimmed);
                let kind = step_kind(main).map_err(|message| Self::fail(origin, raw, &message))?;
                if let StepKind::Acts(acts) = &kind
                    && let Some(act) = acts.iter().find(|act| injects(act))
                {
                    return Err(Self::fail(
                        origin,
                        raw,
                        &format!(
                            "`{act}` injects a product intent instead of a person's input: it may only be a detour (`STEP else {act}`)"
                        ),
                    ));
                }
                let otherwise = otherwise
                    .map(|detour| match step_kind(detour)? {
                        kind @ (StepKind::Acts(_) | StepKind::Pointer { .. }) => Ok(Box::new(kind)),
                        _ => Err("a detour is acts or a click/hover".to_owned()),
                    })
                    .transpose()
                    .map_err(|message| Self::fail(origin, raw, &message))?;
                self.push(origin, trimmed, kind, otherwise);
                Ok(())
            }
        }
    }

    fn push(
        &mut self,
        origin: &Origin,
        text: &str,
        kind: StepKind,
        otherwise: Option<Box<StepKind>>,
    ) {
        let needs = self.needs.take().map(|(gap, _)| gap);
        self.steps.push(Step {
            origin: origin.clone(),
            text: text.to_owned(),
            kind,
            otherwise,
            needs,
        });
    }

    /// `do NAME ARGS…`: the part's body, its arguments substituted, through
    /// this sink, each step remembering the use and the part line.
    fn expand(&mut self, origin: &Origin, raw: &str, used: &str) -> Result<(), String> {
        let (name, args) = used.split_once(' ').unwrap_or((used, ""));
        let parts = self.parts;
        let def = parts.get(name).ok_or_else(|| {
            Self::fail(
                origin,
                raw,
                &format!(
                    "no part `{name}` (have: {})",
                    parts.names().collect::<Vec<_>>().join(", ")
                ),
            )
        })?;
        let here = origin.last().cloned().unwrap_or_else(|| def.site.clone());
        if let Some(first) = self.using.iter().position(|(using, _)| using == name) {
            let chain = self.using[first..]
                .iter()
                .map(|(part, site)| format!("{part} (used at {site})"))
                .chain(std::iter::once(format!("{name} (used at {here})")))
                .collect::<Vec<_>>()
                .join(" > ");
            return Err(Self::fail(origin, raw, &format!("part cycle: {chain}")));
        }
        let bound = bind(def, args).map_err(|message| {
            Self::fail(origin, raw, &format!("{message} (part at {})", def.site))
        })?;
        // A `needs` right before `do` gates the part's first step.
        self.using.push((name.to_owned(), here));
        let result = def.body.iter().try_for_each(|(site, line)| {
            let within = origin.then(site.clone());
            let filled =
                substitute(line, &bound).map_err(|message| Self::fail(&within, line, &message))?;
            self.line(&within, &filled)
        });
        self.using.pop();
        result
    }
}

/// `needs product|data "WHAT"`.
fn needs(rest: &str) -> Result<Gap, String> {
    match tokens(rest)?.as_slice() {
        [Tok::Word(kind), Tok::Str(what)] if !what.trim().is_empty() => Ok(Gap {
            kind: match kind.as_str() {
                "product" => GapKind::Product,
                "data" => GapKind::Data,
                other => return Err(format!("`needs {other}`: a gap is `product` or `data`")),
            },
            what: what.clone(),
        }),
        _ => Err("`needs product|data \"WHAT IS MISSING\"`".to_owned()),
    }
}

/// `start clean`, `start from PART ARGS [then PART ARGS]…`, or `start ROUTE`
/// (the fixture machine).
fn start(rest: &str, site: &Site, parts: &Parts) -> Result<Start, String> {
    let rest = rest.trim();
    if rest == "clean" {
        return Ok(Start::Clean);
    }
    if let Some(chain) = rest.strip_prefix("from ") {
        let mut uses = Vec::new();
        for used in chain.split(" then ") {
            let used = used.trim();
            let (name, raw) = used.split_once(' ').unwrap_or((used, ""));
            let def = parts.get(name).ok_or_else(|| {
                format!(
                    "`start from {name}`: no part `{name}` (have: {})",
                    parts.names().collect::<Vec<_>>().join(", ")
                )
            })?;
            let args =
                bind(def, raw).map_err(|message| format!("{message} (part at {})", def.site))?;
            uses.push(PartUse {
                part: name.to_owned(),
                raw: raw.trim().to_owned(),
                args,
                site: site.clone(),
            });
        }
        return Ok(Start::From(Recipe { uses }));
    }
    super::super::route::parse(rest)?;
    Ok(Start::Fixture {
        route: rest.to_owned(),
    })
}

impl Plan {
    /// Parses a journey file: headers (`size WxH`, `start …`), then lines.
    ///
    /// # Errors
    /// The first line that does not parse, with the sites it came through.
    pub fn parse(name: &str, source: &Path, text: &str, parts: &Parts) -> Result<Self, String> {
        let mut plan = Self {
            name: name.to_owned(),
            source: source.to_path_buf(),
            size: (1440, 900),
            start: Start::Fixture {
                route: "orbit".to_owned(),
            },
            steps: Vec::new(),
        };
        let mut sink = Sink::new(parts);
        for (index, raw) in text.lines().enumerate() {
            let site = Site::new(source, index + 1);
            let trimmed = raw.trim();
            let (verb, rest) = trimmed.split_once(' ').unwrap_or((trimmed, ""));
            match verb {
                "size" if !raw.starts_with(char::is_whitespace) => {
                    plan.size = rest
                        .split_once('x')
                        .and_then(|(w, h)| Some((w.trim().parse().ok()?, h.trim().parse().ok()?)))
                        .ok_or_else(|| format!("{site}: `{rest}` is not WxH\n    {raw}"))?;
                }
                "start" if !raw.starts_with(char::is_whitespace) => {
                    plan.start = start(rest, &site, parts)
                        .map_err(|message| format!("{site}: {message}\n    {raw}"))?;
                }
                _ => sink.line(&Origin::at(site), raw)?,
            }
        }
        plan.steps = sink.finish()?;
        plan.validate()?;
        Ok(plan)
    }

    /// A plan in Rust: `build` receives a [`Builder`] whose calls add steps
    /// through the same sink a journey file uses; each call's site is the
    /// Rust line that made it.
    ///
    /// # Errors
    /// The first call that does not parse or expand.
    #[track_caller]
    pub fn build(
        name: &str,
        parts: &Parts,
        build: impl FnOnce(&mut Builder<'_>),
    ) -> Result<Self, String> {
        let caller = std::panic::Location::caller();
        let mut builder = Builder {
            sink: Sink::new(parts),
            parts,
            size: (1440, 900),
            start: Start::Clean,
            error: None,
        };
        build(&mut builder);
        if let Some(error) = builder.error {
            return Err(error);
        }
        let plan = Self {
            name: name.to_owned(),
            source: PathBuf::from(caller.file()),
            size: builder.size,
            start: builder.start,
            steps: builder.sink.finish()?,
        };
        plan.validate()?;
        Ok(plan)
    }

    fn validate(&self) -> Result<(), String> {
        if !self
            .steps
            .iter()
            .any(|step| matches!(step.kind, StepKind::Check { .. }))
        {
            return Err(format!(
                "{}: a journey needs at least one `check`",
                self.source.display()
            ));
        }
        if !self.start.is_production() {
            for step in &self.steps {
                if matches!(
                    step.kind,
                    StepKind::Restart | StepKind::AnswerPicker(_) | StepKind::Await { .. }
                ) {
                    return Err(format!(
                        "{}: `{}` needs the production machine (`start clean` or `start from …`), not the fixture",
                        step.origin, step.text
                    ));
                }
            }
        }
        Ok(())
    }
}

/// Builds a [`Plan`] in Rust. Every method takes the same words a journey
/// file does, or typed asserts, and records its own call site.
pub struct Builder<'a> {
    sink: Sink<'a>,
    parts: &'a Parts,
    size: (u32, u32),
    start: Start,
    error: Option<String>,
}

impl Builder<'_> {
    #[track_caller]
    fn site() -> Site {
        let caller = std::panic::Location::caller();
        Site::new(Path::new(caller.file()), caller.line() as usize)
    }

    fn keep(&mut self, result: Result<(), String>) -> &mut Self {
        if let Err(error) = result
            && self.error.is_none()
        {
            self.error = Some(error);
        }
        self
    }

    /// The window's logical size.
    pub fn size(&mut self, width: u32, height: u32) -> &mut Self {
        self.size = (width, height);
        self
    }

    /// The machine: `"clean"`, `"from PART ARGS [then …]"` or a route.
    #[track_caller]
    pub fn start(&mut self, words: &str) -> &mut Self {
        let site = Self::site();
        let result = start(words, &site, self.parts)
            .map(|start| self.start = start)
            .map_err(|message| format!("{site}: {message}"));
        self.keep(result)
    }

    /// One journey line: a step, `do PART ARGS`, or `needs …`.
    #[track_caller]
    pub fn step(&mut self, line: &str) -> &mut Self {
        let origin = Origin::at(Self::site());
        let result = self.sink.line(&origin, line);
        self.keep(result)
    }

    /// `do NAME ARGS`.
    #[track_caller]
    pub fn part(&mut self, name: &str, args: &str) -> &mut Self {
        let origin = Origin::at(Self::site());
        let result = self.sink.line(&origin, &format!("do {name} {args}"));
        self.keep(result)
    }

    /// A checkpoint of typed asserts.
    #[track_caller]
    pub fn check(&mut self, name: &str, asserts: impl IntoIterator<Item = Assert>) -> &mut Self {
        let origin = Origin::at(Self::site());
        let result = self.sink.line(&origin, &format!("check {name}"));
        if result.is_ok()
            && let Some(Step {
                kind: StepKind::Check { asserts: into, .. },
                ..
            }) = self.sink.steps.last_mut()
        {
            into.extend(asserts);
        }
        self.keep(result)
    }
}

#[cfg(test)]
mod tests {
    use super::{Plan, Start};
    use crate::harness::journey::parts::Parts;
    use crate::harness::journey::script::{Area, Assert, GapKind, StepKind};
    use std::path::Path;

    fn library() -> Parts {
        let mut parts = Parts::default();
        parts
            .add_file(
                Path::new("lib.part"),
                "part open PROJECT:path\nclick \"{PROJECT.name}\" in reader\ncheck opened-{PROJECT.name}\n  text \"{PROJECT.package}\" in reader\n\
                 part twice PROJECT:path\ndo open {PROJECT}\ndo open {PROJECT}\n\
                 part loop-a\ndo loop-b\npart loop-b\ndo loop-a\n\
                 part gated\nneeds product \"a thing\"\nkey cmd-o\n",
            )
            .expect("library parses");
        parts
    }

    #[test]
    fn a_part_expands_in_place_with_its_checks_and_both_sites() {
        let parts = library();
        let plan = Plan::parse(
            "J",
            Path::new("J.journey"),
            "start clean\ndo twice frontends/rust/fixtures/toml_pin\n",
            &parts,
        )
        .expect("parses");
        assert!(matches!(plan.start, Start::Clean));
        assert_eq!(
            plan.steps.len(),
            4,
            "twice → open ×2 → a click and a check each"
        );
        let StepKind::Check { name, asserts } = &plan.steps[1].kind else {
            panic!("a check")
        };
        assert_eq!(name, "opened-toml_pin");
        assert_eq!(
            asserts[0],
            Assert::Text(vec!["toml-pin-fixture".to_owned()], Some(Area::Reader))
        );
        let origin = plan.steps[3].origin.to_string();
        assert_eq!(
            origin, "J.journey:2 > lib.part:7 > lib.part:3",
            "the use, the part that used it, the line that wrote it"
        );
    }

    #[test]
    fn an_error_inside_a_part_names_the_use_and_the_part_line() {
        let mut parts = library();
        parts
            .add_file(Path::new("bad.part"), "part bad\nclik \"x\"\n")
            .expect("parses as a part");
        let error = Plan::parse(
            "J",
            Path::new("J.journey"),
            "start clean\n\ndo bad\n",
            &parts,
        )
        .expect_err("a bad body line");
        assert!(error.starts_with("J.journey:3 > bad.part:2:"), "{error}");
        let error = Plan::parse(
            "J",
            Path::new("J.journey"),
            "start clean\ndo open no/such/place\n",
            &parts,
        )
        .expect_err("a mistyped argument");
        assert!(
            error.starts_with("J.journey:2:")
                && error.contains("PROJECT:path")
                && error.contains("lib.part:1"),
            "{error}"
        );
    }

    #[test]
    fn a_part_that_uses_itself_is_a_cycle_naming_the_chain() {
        let error = Plan::parse(
            "J",
            Path::new("J.journey"),
            "start clean\ndo loop-a\ncheck x\n",
            &library(),
        )
        .expect_err("a cycle");
        assert!(error.contains("part cycle: loop-a (used at J.journey:2) > loop-b (used at lib.part:9) > loop-a (used at lib.part:11)"), "{error}");
    }

    #[test]
    fn needs_gates_the_next_step_and_stand_in_acts_are_detours_only() {
        let parts = library();
        let plan = Plan::parse(
            "J",
            Path::new("J.journey"),
            "start clean\ndo gated\ncheck x\n",
            &parts,
        )
        .expect("parses");
        let gap = plan.steps[0].needs.as_ref().expect("gated");
        assert_eq!((gap.kind, gap.what.as_str()), (GapKind::Product, "a thing"));
        assert!(plan.steps[1].needs.is_none(), "one gate, one step");
        let error = Plan::parse(
            "J",
            Path::new("J.journey"),
            "start clean\ntheme glacier\ncheck x\n",
            &parts,
        )
        .expect_err("an injected setting");
        assert!(error.contains("may only be a detour"), "{error}");
        Plan::parse(
            "J",
            Path::new("J.journey"),
            "start clean\nkey cmd-, else theme glacier\ncheck x\n",
            &parts,
        )
        .expect("as a detour it parses");
        assert!(
            Plan::parse(
                "J",
                Path::new("J.journey"),
                "start clean\nneeds data \"x\"\n",
                &parts
            )
            .is_err(),
            "a gate with nothing after it"
        );
        assert!(
            Plan::parse(
                "J",
                Path::new("J.journey"),
                "start orbit\nrestart\ncheck x\n",
                &parts
            )
            .expect_err("fixture")
            .contains("production machine")
        );
    }

    #[test]
    fn start_from_binds_a_typed_recipe() {
        let plan = Plan::parse("J", Path::new("J.journey"), "start from open frontends/rust/fixtures/toml_pin then twice frontends/rust/fixtures/toml_pin\ncheck x\n", &library()).expect("parses");
        let Start::From(recipe) = &plan.start else {
            panic!("a state")
        };
        assert_eq!(
            recipe.to_string(),
            "open(frontends/rust/fixtures/toml_pin) then twice(frontends/rust/fixtures/toml_pin)"
        );
        assert_eq!(recipe.inputs().len(), 2);
        let state = recipe.plan(&library(), (1440, 900)).expect("expands");
        assert_eq!(state.steps.len(), 6);
        assert!(matches!(state.start, Start::Clean));
    }

    /// Every journey and part the repository keeps parses against the real
    /// parts: a broken journey fails here, not an hour into a run.
    #[test]
    fn every_journey_in_the_repository_parses_against_the_real_parts() {
        let parts = Parts::load(&Parts::dir()).expect("the repository's parts load");
        assert!(
            parts.get("install").is_some(),
            "the install part is where the journeys expect it"
        );
        let dir = Parts::dir().parent().expect("journeys dir").to_path_buf();
        let mut parsed = 0;
        for entry in std::fs::read_dir(&dir).expect("journeys dir") {
            let path = entry.expect("entry").path();
            if path
                .extension()
                .is_some_and(|extension| extension == "journey")
            {
                let text = std::fs::read_to_string(&path).expect("journey text");
                let name = path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .expect("stem");
                Plan::parse(name, &path, &text, &parts).unwrap_or_else(|error| panic!("{error}"));
                parsed += 1;
            }
        }
        assert!(
            parsed >= 2,
            "only {parsed} journeys found in {}",
            dir.display()
        );
    }

    #[test]
    fn a_plan_built_in_rust_is_the_plan_the_file_gives() {
        let parts = library();
        let text = "start clean\ndo open frontends/rust/fixtures/toml_pin\nkey cmd-k\ncheck end\n  text \"a\" in reader\n";
        let parsed = Plan::parse("J", Path::new("J.journey"), text, &parts).expect("parses");
        let built = Plan::build("J", &parts, |plan| {
            plan.start("clean")
                .part("open", "frontends/rust/fixtures/toml_pin")
                .step("key cmd-k")
                .check(
                    "end",
                    [Assert::Text(vec!["a".to_owned()], Some(Area::Reader))],
                );
        })
        .expect("builds");
        let shape = |plan: &Plan| {
            plan.steps
                .iter()
                .map(|step| format!("{} {:?}", step.text, step.kind))
                .collect::<Vec<_>>()
        };
        assert_eq!(shape(&parsed), shape(&built));
        assert!(
            built.steps[0].origin.to_string().contains("plan.rs:"),
            "a built step names its Rust line: {}",
            built.steps[0].origin
        );
        let error = Plan::build("J", &parts, |plan| {
            plan.part("open", "nowhere");
        })
        .expect_err("a bad argument in Rust");
        assert!(error.contains("plan.rs:"), "{error}");
    }
}
