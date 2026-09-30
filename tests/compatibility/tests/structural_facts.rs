//! Per-language proof that the structural lane observes the declaration
//! facts a reader weighs: whether a declaration is deprecated (with the
//! source's own `since` and words), and what an implementor of a contract
//! owes for each member.
//!
//! Every fixture goes through the real compiled tags query of the real
//! frontend. The assertions read the facts back as rendered text, so an
//! erased note, a lost `since`, or an obligation read from the wrong node
//! fails here rather than on a page.

use backend_compile::{DeclarationFacts, Fact, SourceDeclaration, SyntaxError, SyntaxFrontend};
use std::{error::Error, path::Path};

fn analyzed(
    frontend: &Result<SyntaxFrontend, SyntaxError>,
    path: &str,
    source: &str,
) -> Result<Vec<SourceDeclaration>, Box<dyn Error>> {
    let frontend = frontend.as_ref().map_err(ToString::to_string)?;
    let analysis = frontend.analyze(Path::new(path), source.as_bytes())?;
    Ok(analysis.declarations().to_vec())
}

/// Renders one declaration's facts as the assertions read them.
fn rendered(facts: &DeclarationFacts) -> String {
    let deprecation = match &facts.deprecation {
        Fact::Unobserved => "deprecation unobserved".to_owned(),
        Fact::Absent => "current".to_owned(),
        Fact::Present(notice) => format!(
            "deprecated since {:?} note {:?}",
            notice.since(),
            notice.note()
        ),
    };
    let obligation = match &facts.obligation {
        Fact::Unobserved => "obligation unobserved".to_owned(),
        Fact::Absent => "owes nothing".to_owned(),
        Fact::Present(obligation) => obligation.name().to_owned(),
    };
    format!("{deprecation}, {obligation}")
}

/// Requires the exact rendered facts of each named declaration.
fn assert_facts(
    declarations: &[SourceDeclaration],
    expected: &[(&str, &str, &str)],
) -> Result<(), Box<dyn Error>> {
    let mut wrong = Vec::new();
    for (kind, name, facts) in expected {
        let Some(found) = declarations
            .iter()
            .find(|declaration| declaration.name() == *name && declaration.kind().name() == *kind)
        else {
            wrong.push(format!("no {kind} named {name}"));
            continue;
        };
        let actual = rendered(found.facts());
        if actual != *facts {
            wrong.push(format!("{kind} {name}: {actual}\n    expected {facts}"));
        }
    }
    if wrong.is_empty() {
        return Ok(());
    }
    let rows = declarations
        .iter()
        .map(|declaration| {
            format!(
                "{} {}: {}",
                declaration.kind().name(),
                declaration.name(),
                rendered(declaration.facts())
            )
        })
        .collect::<Vec<_>>();
    Err(format!("{}\nextracted:\n  {}", wrong.join("\n"), rows.join("\n  ")).into())
}

fn signature<'a>(
    declarations: &'a [SourceDeclaration],
    name: &str,
) -> Result<&'a str, Box<dyn Error>> {
    declarations
        .iter()
        .find(|declaration| declaration.name() == name)
        .map(SourceDeclaration::signature)
        .ok_or_else(|| std::io::Error::other(format!("no declaration named {name}")).into())
}

fn documentation<'a>(
    declarations: &'a [SourceDeclaration],
    kind: &str,
    name: &str,
) -> Result<&'a str, Box<dyn Error>> {
    declarations
        .iter()
        .find(|declaration| declaration.name() == name && declaration.kind().name() == kind)
        .map(SourceDeclaration::documentation)
        .ok_or_else(|| format!("no {kind} named {name}").into())
}

const RUST_SOURCE: &str = r#"//! The crate's own documentation.

/// Makes one.
///
/// # Errors
/// Fails when the input is empty.
#[deprecated(since = "1.2.0", note = "use `fresh`")]
pub fn stale() {}

pub trait Service {
    type Output;
    const LIMIT: u32;
    const DEFAULT: u32 = 3;
    fn execute(&self) -> Self::Output;
    fn describe(&self) -> String {
        String::new()
    }
}

pub struct Plain {
    #[deprecated = "read `name` instead"]
    pub label: u8,
}

impl Plain {
    pub fn run(&self) {}
}
"#;

#[test]
fn rust_reads_deprecated_attributes_and_trait_obligations() -> Result<(), Box<dyn Error>> {
    let declarations = analyzed(
        &backend_frontend_rust::syntax_frontend(),
        "src/lib.rs",
        RUST_SOURCE,
    )?;
    assert_facts(
        &declarations,
        &[
            (
                "function",
                "stale",
                r#"deprecated since Some("1.2.0") note Some("use `fresh`"), owes nothing"#,
            ),
            (
                "field",
                "label",
                r#"deprecated since None note Some("read `name` instead"), owes nothing"#,
            ),
            ("type", "Output", "current, required"),
            ("constant", "LIMIT", "current, required"),
            ("constant", "DEFAULT", "current, provided"),
            ("method", "execute", "current, required"),
            ("method", "describe", "current, provided"),
            ("method", "run", "current, owes nothing"),
        ],
    )?;
    // A Markdown heading inside a `///` comment is text, not a comment marker.
    assert_eq!(
        documentation(&declarations, "function", "stale")?,
        "Makes one.\n\n# Errors\nFails when the input is empty."
    );
    Ok(())
}

const JAVA_SOURCE: &str = r#"
public interface Shape {
    double area();
    default String label() { return null; }
    static Shape unit() { return null; }
}

public abstract class Base {
    public abstract void run();
    public void stop() {}
}

public class Old {
    /**
     * Makes one.
     * @deprecated use {@link Fresh} instead
     */
    @Deprecated(since = "9", forRemoval = true)
    public void make() {}
}
"#;

#[test]
fn java_reads_deprecated_annotations_and_interface_defaults() -> Result<(), Box<dyn Error>> {
    let declarations = analyzed(
        &backend_frontend_java::syntax_frontend(),
        "src/Shape.java",
        JAVA_SOURCE,
    )?;
    assert_facts(
        &declarations,
        &[
            ("method", "area", "current, required"),
            ("method", "label", "current, provided"),
            ("method", "unit", "current, owes nothing"),
            ("method", "run", "current, required"),
            ("method", "stop", "current, provided"),
            (
                "method",
                "make",
                r#"deprecated since Some("9") note Some("use {@link Fresh} instead"), owes nothing"#,
            ),
        ],
    )
}

const CSHARP_SOURCE: &str = r#"
public interface IShape
{
    double Area();
    string Label() => "shape";
    int Sides { get; }
}

public abstract class Base
{
    public abstract void Run();
    public virtual void Stop() {}
}

public class Old
{
    /// <summary>Makes one.</summary>
    /// <returns>The made thing.</returns>
    /// <exception cref="T:System.ArgumentNullException">When nothing is given.</exception>
    [Obsolete("use Fresh", true)]
    public int Make() { return 1; }
}
"#;

#[test]
fn csharp_reads_obsolete_and_default_interface_members() -> Result<(), Box<dyn Error>> {
    let declarations = analyzed(
        &backend_frontend_csharp::syntax_frontend(),
        "src/Shape.cs",
        CSHARP_SOURCE,
    )?;
    assert_facts(
        &declarations,
        &[
            ("method", "Area", "current, required"),
            ("method", "Label", "current, provided"),
            ("property", "Sides", "current, required"),
            ("method", "Run", "current, required"),
            ("method", "Stop", "current, provided"),
            (
                "method",
                "Make",
                r#"deprecated since None note Some("use Fresh"), owes nothing"#,
            ),
        ],
    )?;
    // The returns and exception elements survive as tag lines.
    assert_eq!(
        documentation(&declarations, "method", "Make")?,
        "Makes one.\n@returns The made thing.\n@throws System.ArgumentNullException When nothing is given."
    );
    Ok(())
}

const TYPESCRIPT_SOURCE: &str = r"
export interface Shape {
  area(): number;
  label?(): string;
  sides: number;
  color?: string;
}

export abstract class Base {
  abstract run(): void;
  stop(): void {}
}

/**
 * Makes one.
 * @deprecated use `fresh` instead
 */
export function make(): number {
  return 1;
}
";

#[test]
fn typescript_reads_jsdoc_deprecation_and_optional_members() -> Result<(), Box<dyn Error>> {
    let declarations = analyzed(
        &backend_frontend_typescript::syntax_frontend(),
        "src/shape.ts",
        TYPESCRIPT_SOURCE,
    )?;
    assert_facts(
        &declarations,
        &[
            ("method", "area", "current, required"),
            ("method", "label", "current, optional"),
            ("property", "sides", "current, required"),
            ("property", "color", "current, optional"),
            ("method", "run", "current, required"),
            ("method", "stop", "current, provided"),
            (
                "function",
                "make",
                r#"deprecated since None note Some("use `fresh` instead"), owes nothing"#,
            ),
        ],
    )
}

const PYTHON_SOURCE: &str = r#"
from abc import ABC, abstractmethod
from typing import Protocol
from typing_extensions import deprecated


class Shape(ABC):
    @abstractmethod
    def area(self) -> float: ...

    def label(self) -> str:
        return "shape"


class Drawable(Protocol):
    def draw(self) -> None: ...


class Plain:
    def run(self) -> None:
        pass


@deprecated("use fresh")
def make():
    """Makes one.

    Raises:
        ValueError: when empty.
    """
    return 1
"#;

#[test]
fn python_reads_deprecated_decorators_and_abstract_methods() -> Result<(), Box<dyn Error>> {
    let declarations = analyzed(
        &backend_frontend_python::syntax_frontend(),
        "shapes.py",
        PYTHON_SOURCE,
    )?;
    assert_facts(
        &declarations,
        &[
            ("method", "area", "current, required"),
            ("method", "label", "current, provided"),
            ("method", "draw", "current, required"),
            ("method", "run", "current, owes nothing"),
            (
                "function",
                "make",
                r#"deprecated since None note Some("use fresh"), owes nothing"#,
            ),
        ],
    )
}

const GO_SOURCE: &str = r"package shapes

// Shape measures itself.
type Shape interface {
	Area() float64
}

// Old makes one.
//
// Deprecated: use New.
func Old() int { return 1 }
";

#[test]
fn go_reads_the_deprecated_paragraph_and_interface_methods() -> Result<(), Box<dyn Error>> {
    let declarations = analyzed(
        &backend_frontend_go::syntax_frontend(),
        "shapes.go",
        GO_SOURCE,
    )?;
    assert_facts(
        &declarations,
        &[
            ("method", "Area", "current, required"),
            (
                "function",
                "Old",
                r#"deprecated since None note Some("use New."), owes nothing"#,
            ),
        ],
    )
}

const CPP_SOURCE: &str = r#"
class Shape {
public:
    virtual double area() const = 0;
    virtual const char* label() const { return "shape"; }
    int sides() const { return 0; }
};

[[deprecated("use fresh")]] int make();
"#;

#[test]
fn cpp_reads_deprecated_attributes_and_pure_virtuals() -> Result<(), Box<dyn Error>> {
    let declarations = analyzed(
        &backend_frontend_clang::syntax_frontend(),
        "shape.hpp",
        CPP_SOURCE,
    )?;
    assert_facts(
        &declarations,
        &[
            ("method", "area", "current, required"),
            ("method", "label", "current, provided"),
            ("method", "sides", "current, owes nothing"),
            (
                "function",
                "make",
                r#"deprecated since None note Some("use fresh"), owes nothing"#,
            ),
        ],
    )
}

#[test]
fn clang_keeps_complete_multiline_signatures_at_ast_boundaries() -> Result<(), Box<dyn Error>> {
    let c = analyzed(
        &backend_frontend_clang::syntax_frontend(),
        "callables.c",
        r#"
#include <stddef.h>
#define BACKEND_LIMIT 32

[[nodiscard]] const char *lookup(
    const char *key,
    /* Keep this comment within the declarator source range. */
    int (*compare)(const char *left, const char *right),
    void *context
);

int map_values(
    int (*callback)(int value),
    int value
) {
    return callback(value);
}
"#,
    )?;
    assert_eq!(
        signature(&c, "lookup")?,
        r#"[[nodiscard]] const char *lookup(
    const char *key,
    /* Keep this comment within the declarator source range. */
    int (*compare)(const char *left, const char *right),
    void *context
);"#
    );
    assert_eq!(
        signature(&c, "map_values")?,
        r#"int map_values(
    int (*callback)(int value),
    int value
)"#
    );

    let cpp = analyzed(
        &backend_frontend_clang::syntax_frontend(),
        "reader.hpp",
        r#"
class Reader {
public:
    virtual int call(
        int (*callback)(const char *text),
        void *context
    ) const = 0;
};
"#,
    )?;
    assert_eq!(
        signature(&cpp, "call")?,
        r#"virtual int call(
        int (*callback)(const char *text),
        void *context
    ) const = 0;"#
    );
    Ok(())
}
