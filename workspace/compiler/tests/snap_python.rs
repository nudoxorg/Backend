//! Snapshot tests for the **Python** language pipeline.
//!
//! Two groups of snapshots live here:
//!
//!   **Compile targets** — Python source → Pyrefly oracle → `ir::entry::Index`.
//!   Snapshots pin the Debug rendering of the produced IR so any change to
//!   lowering immediately shows up as a snapshot diff.
//!
//!   **Renderer** — take that same produced IR and render back to Python surface
//!   syntax via `compiler::render`. Snapshots pin the pretty-printed string so
//!   changes to the Python backend are caught immediately.
//!
//! # Determinism
//! `Index.entries_by_path` is an `FxHashMap` with non-deterministic iteration
//! order. Before snapshotting we always project into a `BTreeMap<String, &Entry>`
//! keyed by the path display string, giving a stable, sorted view.
//!
//! # Running / accepting
//! ```
//! INSTA_UPDATE=always buck2 test //workspace/compiler:snap_python
//! ```
//! (Replace `snap_python` with whatever `rust_tests` names the target derived
//! from this file — typically the file stem.)

use std::collections::BTreeMap;

use compiler::languages::python::context::PythonContext;
use compiler::render::{render_entry, Language, RenderCtx};
use ir::entry::{Index, NudoxPath};
use ir::kind::Entry;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Collect `index.entries_by_path` into a sorted `BTreeMap<String, &Entry>`.
///
/// Keys are path display strings: `NudoxPath::Local(p)` → `p.display()`,
/// `NudoxPath::External { path, dependency }` → `dependency::path`.
/// Using a `BTreeMap` makes the Debug snapshot deterministic regardless of the
/// underlying `FxHashMap` iteration order.
fn sorted_entries(index: &Index) -> BTreeMap<String, &Entry> {
    index
        .entries_by_path
        .iter()
        .map(|(path, entry)| {
            let key = match path {
                NudoxPath::Local(p) => p.display().to_string(),
                NudoxPath::External { path, dependency } => {
                    format!("{}::{}", dependency, path.display())
                }
            };
            (key, entry)
        })
        .collect()
}

/// Lower a snippet and return both the Index and its sorted projection.
fn lower(module_name: &str, source: &str) -> Index {
    let ctx = PythonContext::new();
    let handle = ctx.check_snippet(module_name, source);
    ctx.lower_handle(&handle)
}

/// A Python context for rendering.
fn py_ctx(width: usize) -> RenderCtx {
    RenderCtx::new(Language::Python).with_width(width).with_docs(true)
}

/// Render all entries in a sorted, deterministic order and join them with a
/// separator so the whole module surfaces in one snapshot.
fn render_all(index: &Index, cx: &RenderCtx) -> String {
    let sorted = sorted_entries(index);
    sorted
        .values()
        .map(|e| render_entry(e, cx))
        .collect::<Vec<_>>()
        .join("\n\n# ---\n\n")
}

// ===========================================================================
// COMPILE-TARGET SNAPSHOTS
// ===========================================================================

// ---------------------------------------------------------------------------
// 1. @dataclass with resolved field types (int, str)
// ---------------------------------------------------------------------------

const DATACLASS_BASIC: &str = "\
from dataclasses import dataclass

@dataclass
class Point:
    x: int
    y: int
    label: str
";

/// Pins the IR produced from a simple @dataclass with int/str fields.
#[test]
fn snap_compile_dataclass_basic() {
    let index = lower("snap_dataclass_basic", DATACLASS_BASIC);
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("py_dataclass_basic_ir", sorted);
}

// ---------------------------------------------------------------------------
// 2. @dataclass with list[X], dict[K,V], Optional/X|None fields
// ---------------------------------------------------------------------------

const DATACLASS_COMPLEX_FIELDS: &str = "\
from __future__ import annotations
from dataclasses import dataclass
from typing import Optional

@dataclass
class Container:
    items: list[int]
    mapping: dict[str, int]
    maybe: Optional[str]
    union_none: str | None
";

/// Pins the IR produced from a @dataclass with container and optional fields.
/// This exercises list[X], dict[K,V], Optional[T], and T|None lowering.
#[test]
fn snap_compile_dataclass_complex_fields() {
    let index = lower("snap_dataclass_complex_fields", DATACLASS_COMPLEX_FIELDS);
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("py_dataclass_complex_fields_ir", sorted);
}

// ---------------------------------------------------------------------------
// 3. Plain class with methods and instance attributes (not a dataclass)
// ---------------------------------------------------------------------------

const PLAIN_CLASS: &str = "\
class Counter:
    \"\"\"A simple counter.\"\"\"

    count: int

    def increment(self) -> None:
        self.count += 1

    def reset(self) -> None:
        self.count = 0

    def value(self) -> int:
        return self.count
";

/// Pins the IR from a plain (non-dataclass) class with instance attributes
/// and methods. Verifies that documentation, methods, and fields all survive.
#[test]
fn snap_compile_plain_class_with_methods() {
    let index = lower("snap_plain_class", PLAIN_CLASS);
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("py_plain_class_ir", sorted);
}

// ---------------------------------------------------------------------------
// 4. Subclassing / super_types
// ---------------------------------------------------------------------------

const SUBCLASS: &str = "\
from dataclasses import dataclass

class Animal:
    name: str

    def sound(self) -> str:
        return ''

@dataclass
class Dog(Animal):
    breed: str

    def sound(self) -> str:
        return 'woof'

@dataclass
class Cat(Animal):
    indoor: bool
";

/// Pins the IR from a base class and two subclasses.
/// Verifies that `super_types` is populated for subclasses.
#[test]
fn snap_compile_subclassing() {
    let index = lower("snap_subclassing", SUBCLASS);
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("py_subclassing_ir", sorted);
}

// ---------------------------------------------------------------------------
// 5. enum.Enum → SumType with variants
// ---------------------------------------------------------------------------

const ENUM: &str = "\
from enum import Enum

class Direction(Enum):
    NORTH = 'N'
    SOUTH = 'S'
    EAST = 'E'
    WEST = 'W'

class Status(Enum):
    PENDING = 0
    ACTIVE = 1
    INACTIVE = 2
";

/// Pins the IR from two Enum classes → SumType with named variants.
#[test]
fn snap_compile_enum_sum_type() {
    let index = lower("snap_enum", ENUM);
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("py_enum_ir", sorted);
}

// ---------------------------------------------------------------------------
// 6. typing.Protocol → TraitDef
// ---------------------------------------------------------------------------

const PROTOCOL: &str = "\
from typing import Protocol

class Drawable(Protocol):
    \"\"\"Anything that can be drawn.\"\"\"

    def draw(self, canvas: str) -> None: ...
    def resize(self, factor: float) -> None: ...

class Serializable(Protocol):
    def to_bytes(self) -> bytes: ...
    def from_bytes(self, data: bytes) -> None: ...
";

/// Pins the IR from typing.Protocol classes → TraitDef with required methods.
#[test]
fn snap_compile_protocol_traitdef() {
    let index = lower("snap_protocol", PROTOCOL);
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("py_protocol_ir", sorted);
}

// ---------------------------------------------------------------------------
// 7. Functions: multiple params, return annotations, default values
// ---------------------------------------------------------------------------

const FUNCTIONS: &str = "\
def add(a: int, b: int) -> int:
    return a + b

def greet(name: str, count: int = 1) -> str:
    return name * count

def maybe(value: int, fallback: str = 'none') -> str:
    return str(value) if value else fallback
";

/// Pins the IR from functions with multiple annotated params and defaults.
#[test]
fn snap_compile_functions_params_defaults() {
    let index = lower("snap_functions", FUNCTIONS);
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("py_functions_ir", sorted);
}

// ---------------------------------------------------------------------------
// 8. Functions: *args / **kwargs (variadic)
// ---------------------------------------------------------------------------

const VARIADIC_FUNCTIONS: &str = "\
def log(*messages: str) -> None:
    pass

def configure(**kwargs: int) -> None:
    pass

def mixed(first: int, *rest: str, **options: bool) -> str:
    return str(first)
";

/// Pins the IR from functions with *args and **kwargs parameters.
/// Verifies that variadic parameters survive lowering.
#[test]
fn snap_compile_variadic_functions() {
    let index = lower("snap_variadic_functions", VARIADIC_FUNCTIONS);
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("py_variadic_functions_ir", sorted);
}

// ---------------------------------------------------------------------------
// 9. @overload decorated functions
// ---------------------------------------------------------------------------

const OVERLOADS: &str = "\
from typing import overload

@overload
def process(x: int) -> int: ...
@overload
def process(x: str) -> str: ...
@overload
def process(x: bytes) -> bytes: ...
def process(x):
    return x
";

/// Pins the IR from @overload-decorated functions.
/// Verifies that multiple overloads collapse to a single entry with branches.
#[test]
fn snap_compile_overloaded_function() {
    let index = lower("snap_overloads", OVERLOADS);
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("py_overloads_ir", sorted);
}

// ---------------------------------------------------------------------------
// 10. Generics / TypeVar
// ---------------------------------------------------------------------------

const GENERICS: &str = "\
from typing import TypeVar, Generic

T = TypeVar('T')
K = TypeVar('K')
V = TypeVar('V')

class Box(Generic[T]):
    \"\"\"A generic container.\"\"\"
    value: T

class Pair(Generic[K, V]):
    first: K
    second: V

def identity(x: T) -> T:
    return x
";

/// Pins the IR from generic classes and functions using TypeVar.
/// Exercises generic_args and GenericParam lowering.
#[test]
fn snap_compile_generics_typevar() {
    let index = lower("snap_generics", GENERICS);
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("py_generics_ir", sorted);
}

// ---------------------------------------------------------------------------
// 11. Module-level constants / variables
// ---------------------------------------------------------------------------

const MODULE_CONSTANTS: &str = "\
MAX_SIZE: int = 1024
DEFAULT_NAME: str = 'world'
ENABLED: bool = True
PI: float = 3.14159
";

/// Pins the IR from module-level annotated constants/variables.
#[test]
fn snap_compile_module_constants() {
    let index = lower("snap_constants", MODULE_CONSTANTS);
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("py_module_constants_ir", sorted);
}

// ---------------------------------------------------------------------------
// 12. Docstrings (exercise documentation field)
// ---------------------------------------------------------------------------

const DOCSTRINGS: &str = "\
\"\"\"Module docstring.\"\"\"

def documented_func(x: int, y: str) -> bool:
    \"\"\"Check if x converts to y.

    Args:
        x: the integer to convert.
        y: the expected string form.

    Returns:
        True if str(x) == y.
    \"\"\"
    return str(x) == y

class Documented:
    \"\"\"A well-documented class.\"\"\"

    value: int

    def describe(self) -> str:
        \"\"\"Return a description.\"\"\"
        return str(self.value)
";

/// Pins the IR from sources with docstrings.
/// Verifies that Symbol.documentation and LiteralParameter.description survive.
#[test]
fn snap_compile_docstrings() {
    let index = lower("snap_docstrings", DOCSTRINGS);
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("py_docstrings_ir", sorted);
}

// ---------------------------------------------------------------------------
// 13. Empty class and class with only docstring
// ---------------------------------------------------------------------------

const EMPTY_CLASSES: &str = "\
class Empty:
    pass

class JustDoc:
    \"\"\"This class has only a docstring.\"\"\"
    pass
";

/// Pins the IR from empty classes and classes with only docstrings.
/// Edge case: these should still produce valid RecordType entries.
#[test]
fn snap_compile_empty_classes() {
    let index = lower("snap_empty_classes", EMPTY_CLASSES);
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("py_empty_classes_ir", sorted);
}

// ---------------------------------------------------------------------------
// 14. Nested / recursive types
// ---------------------------------------------------------------------------

const NESTED_TYPES: &str = "\
from __future__ import annotations
from dataclasses import dataclass
from typing import Optional

@dataclass
class TreeNode:
    value: int
    left: Optional[TreeNode]
    right: Optional[TreeNode]

@dataclass
class LinkedList:
    head: int
    tail: Optional[LinkedList]
";

/// Pins the IR from recursive/self-referential dataclass types.
/// Exercises Optional[Self] and forward references.
#[test]
fn snap_compile_nested_recursive_types() {
    let index = lower("snap_nested_types", NESTED_TYPES);
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("py_nested_types_ir", sorted);
}

// ===========================================================================
// RENDERER SNAPSHOTS
// ===========================================================================

// ---------------------------------------------------------------------------
// R1. @dataclass with int/str fields — render at width 80 (default)
// ---------------------------------------------------------------------------

/// Pins the Python surface rendered from a basic @dataclass IR.
#[test]
fn snap_render_dataclass_basic_w80() {
    let index = lower("snap_r_dataclass_basic", DATACLASS_BASIC);
    let cx = py_ctx(80);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("py_render_dataclass_basic_w80", rendered);
}

// ---------------------------------------------------------------------------
// R2. @dataclass with complex fields — render at width 40 and width 100
//     (exercises the pretty-printer's line-breaking behavior)
// ---------------------------------------------------------------------------

/// Pins Python surface at width 40 — should force field-per-line breaks.
#[test]
fn snap_render_dataclass_complex_w40() {
    let index = lower("snap_r_dataclass_complex_w40", DATACLASS_COMPLEX_FIELDS);
    let cx = py_ctx(40);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("py_render_dataclass_complex_w40", rendered);
}

/// Pins Python surface at width 100 — wide enough to inline most constructs.
#[test]
fn snap_render_dataclass_complex_w100() {
    let index = lower("snap_r_dataclass_complex_w100", DATACLASS_COMPLEX_FIELDS);
    let cx = py_ctx(100);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("py_render_dataclass_complex_w100", rendered);
}

// ---------------------------------------------------------------------------
// R3. Plain class with methods
// ---------------------------------------------------------------------------

/// Pins the Python surface rendered from a plain class with methods and docs.
#[test]
fn snap_render_plain_class() {
    let index = lower("snap_r_plain_class", PLAIN_CLASS);
    let cx = py_ctx(80);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("py_render_plain_class", rendered);
}

// ---------------------------------------------------------------------------
// R4. Subclassing
// ---------------------------------------------------------------------------

/// Pins the Python surface rendered from a class hierarchy.
#[test]
fn snap_render_subclassing() {
    let index = lower("snap_r_subclassing", SUBCLASS);
    let cx = py_ctx(80);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("py_render_subclassing", rendered);
}

// ---------------------------------------------------------------------------
// R5. Enum → SumType
// ---------------------------------------------------------------------------

/// Pins the Python surface rendered from Enum classes → sum-type dataclasses
/// plus union alias.
#[test]
fn snap_render_enum() {
    let index = lower("snap_r_enum", ENUM);
    let cx = py_ctx(80);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("py_render_enum", rendered);
}

// ---------------------------------------------------------------------------
// R6. Protocol → interface
// ---------------------------------------------------------------------------

/// Pins the Python surface rendered from Protocol classes.
/// Should emit `class Foo(Protocol):` with method stubs and docstrings.
#[test]
fn snap_render_protocol() {
    let index = lower("snap_r_protocol", PROTOCOL);
    let cx = py_ctx(80);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("py_render_protocol", rendered);
}

// ---------------------------------------------------------------------------
// R7. Functions with params and defaults
// ---------------------------------------------------------------------------

/// Pins the Python surface rendered from annotated functions.
#[test]
fn snap_render_functions() {
    let index = lower("snap_r_functions", FUNCTIONS);
    let cx = py_ctx(80);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("py_render_functions", rendered);
}

// ---------------------------------------------------------------------------
// R8. Variadic functions — width 40 and 100 to exercise line-breaking
// ---------------------------------------------------------------------------

/// Pins variadic function rendering at width 40 (narrow — forces breaks).
#[test]
fn snap_render_variadic_w40() {
    let index = lower("snap_r_variadic_w40", VARIADIC_FUNCTIONS);
    let cx = py_ctx(40);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("py_render_variadic_w40", rendered);
}

/// Pins variadic function rendering at width 100 (wide — inlines).
#[test]
fn snap_render_variadic_w100() {
    let index = lower("snap_r_variadic_w100", VARIADIC_FUNCTIONS);
    let cx = py_ctx(100);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("py_render_variadic_w100", rendered);
}

// ---------------------------------------------------------------------------
// R9. Generics / TypeVar
// ---------------------------------------------------------------------------

/// Pins the Python surface for generic classes and functions.
/// Exercises PEP 695 type parameters: `class Box[T]:`, `def identity[T]`.
#[test]
fn snap_render_generics() {
    let index = lower("snap_r_generics", GENERICS);
    let cx = py_ctx(80);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("py_render_generics", rendered);
}

// ---------------------------------------------------------------------------
// R10. Docstrings with docs enabled
// ---------------------------------------------------------------------------

/// Pins the Python surface with `with_docs(true)`, so docstrings appear as
/// triple-quoted blocks above each rendered entry.
#[test]
fn snap_render_docstrings_with_docs() {
    let index = lower("snap_r_docstrings", DOCSTRINGS);
    let cx = py_ctx(80); // already with_docs(true) via py_ctx
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("py_render_docstrings_with_docs", rendered);
}

// ---------------------------------------------------------------------------
// R11. Empty classes
// ---------------------------------------------------------------------------

/// Pins the Python surface for empty classes.
/// These should render as `class Empty: ...` (stub body).
#[test]
fn snap_render_empty_classes() {
    let index = lower("snap_r_empty_classes", EMPTY_CLASSES);
    let cx = py_ctx(80);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("py_render_empty_classes", rendered);
}

// ---------------------------------------------------------------------------
// R12. Recursive / nested types
// ---------------------------------------------------------------------------

/// Pins the Python surface for self-referential types.
/// Optional[TreeNode] must render as `Optional[TreeNode]` (not `Any`).
#[test]
fn snap_render_nested_types() {
    let index = lower("snap_r_nested_types", NESTED_TYPES);
    let cx = py_ctx(80);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("py_render_nested_types", rendered);
}

// ---------------------------------------------------------------------------
// R13. Wide dataclass — render at both width 40 and 100 for line-break coverage
// ---------------------------------------------------------------------------

const WIDE_RECORD: &str = "\
from __future__ import annotations
from dataclasses import dataclass
from typing import Optional

@dataclass
class Configuration:
    \"\"\"Full application configuration with many fields.\"\"\"
    host: str
    port: int
    max_connections: int
    timeout_seconds: float
    enable_tls: bool
    certificate_path: Optional[str]
    private_key_path: Optional[str]
    allowed_origins: list[str]
    extra_headers: dict[str, str]
";

/// Pins the wide Configuration record at 40 columns — forces multi-line layout.
#[test]
fn snap_render_wide_record_w40() {
    let index = lower("snap_r_wide_w40", WIDE_RECORD);
    let cx = py_ctx(40);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("py_render_wide_record_w40", rendered);
}

/// Pins the wide Configuration record at 100 columns — most fields inline.
#[test]
fn snap_render_wide_record_w100() {
    let index = lower("snap_r_wide_w100", WIDE_RECORD);
    let cx = py_ctx(100);
    let rendered = render_all(&index, &cx);
    insta::assert_snapshot!("py_render_wide_record_w100", rendered);
}
