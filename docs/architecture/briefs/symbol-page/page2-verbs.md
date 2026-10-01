# VERBS: one grammar for seven languages

The symbol page speaks in **verbs**. Every relation the engine knows, in any of the seven languages, is normalised to one verb. The verb sets:

- its **direction**: in = where it comes from, out = where it goes;
- the **question** it answers, which decides the section it lives in;
- its row in the sentence.

The same verb reads the same everywhere. What differs by language is only the *spelling*, and the page shows the spelling when you rest on a verb (`page2.js` `VERBS`, and the still `shots/page2/rel-hover-verb-value.png`).

Implementation: `page2/extract2.mjs` `relations()` maps the world's edge bits (`has takes gives is derives calls uses type impl`) onto these verbs. `page2/source.mjs` adds the verbs the world has no edge for (becomes, acts like, fails with, asked for by, gated by, deprecated).

## 1. Relation verbs

| Verb | Dir | Question → section | Rust | TypeScript / JS | Python | Go | Java | C# | C++ |
|---|---|---|---|---|---|---|---|---|---|
| **is** | — | what is it → the sentence and the anatomy | the kind: enum / struct / trait / fn; `impl Tr for T` and `#[derive]` feed *can* | `class` / `interface` / union / `type` | `class` / `Enum` / `Protocol` / `def` | `type T struct` / `interface` / `func` | `class` / `interface` / `enum` / `record` | `class` / `struct` / `interface` / `enum` / `record` | `class` / `struct` / `enum class` / `concept` |
| **one of** | — | what is it → the *fork* anatomy | enum variants | union members; string-literal unions | `Enum` members; `Union[...]`; `Literal[...]` | a `const` block of one named type (iota) | `enum` constants; `sealed … permits` | `enum` members | `enum class` members; `std::variant<…>` |
| **holds** | — | what is it → the *holds* anatomy | struct and variant fields | properties | dataclass / attrs / typed class fields | struct fields | fields and record components | fields and properties | data members |
| **takes · gives** | — | what is it → the *pipe* anatomy | params · return (`-> T`) | params · return; `Promise<T>` reads "gives, later" | params · return annotation | params · results | params · return | params · return; `Task<T>` "gives, later" | params · return |
| **asks for** | — | what is it → the *contract* anatomy | a trait's required methods (no body) | interface members | `@abstractmethod`; Protocol members | interface methods (all are required: no "you get" group) | abstract / interface methods without `default` | interface members without a default body | pure `virtual … = 0` |
| **gives for free** | — | what is it → the contract ("you get") | provided methods | abstract-class concrete methods | ABC concrete methods | — (the section is omitted) | `default` methods | default interface methods | non-pure virtuals in the base |
| **comes from** | in | how do I get one → **Getting one** | fns returning T; `impl From<X> for T`; `Default` | `new T()`; functions returning T; factories | `T(...)`; `@classmethod` constructors; fns returning T | `NewT()`; fns returning T or `*T` | constructors; static factories | constructors; `implicit operator T`; factories | constructors; converting ctors; fns returning T |
| **done by** | in | who uses it → **Who uses it** (a trait's implementors: yours / here / elsewhere) | `impl Tr for X`; `#[derive(Tr)]` | `class X implements I`; structurally matching types | subclasses; Protocol matches | any type with the method set (implicit) | `implements` / `extends` | `: I` | public derivation |
| **called by** | in | who uses it | call sites | call sites | call sites | call sites | call sites | call sites | call sites |
| **taken by** | out | who uses it | a param of type T, `&T`, `Option<&T>` | a param of type T | a param annotated T | a param T or `*T` | a param T | a param T | a param `T`, `const T&`, `T*` |
| **held by** | out | who uses it | a field or variant of type T | a property of type T | an attribute of type T | a struct field T | a field T | a field or property T | a data member T |
| **called on by** | out | who uses it | calls to its methods (`x.m()`, `T::m`) | `x.m()` | `x.m()` | `x.M()` | `x.m()` | `x.M()` | `x.m()`, `x->m()` |
| **asked for by** | out | who uses it | bounds: `<T: Tr>`, `where T: Tr` | `<T extends I>` | `TypeVar(bound=P)` | type-parameter constraints `[T I]` | `<T extends I>` | `where T : I` | `template<C T>`, `requires C<T>` |
| **used by** | out | who uses it | any other mention (a path, a cast, a type argument) | same | same | same | same | same | same |
| **calls** | out | what is it → the anatomy's foot ("inside, it calls") | callees | callees | callees | callees | callees | callees | callees |
| **becomes** | out | what can I do → **What it does** | `impl From<T> for X`; `Into`; `Display` → text | `toString()`; `toJSON()` | `__str__`, `__repr__`, `__iter__`; conversion methods | `String()` (Stringer); methods returning another type | `toString()`; conversion methods | `explicit` / `implicit operator X`; `ToString()` | conversion operators `operator X()` |
| **acts like** | out | what can I do → **What it does** (one line that folds open) | `Deref<Target = X>` (methods from Deref) | `interface B extends A`; mixins | the MRO parent; `__getattr__` delegation | an embedded struct (promoted fields and methods) | the superclass | the base class | public base class; `operator->` / `operator*` on smart pointers |
| **fails with** | out | what can go wrong → **When it fails** | `Result<_, E>` | `throws` (JSDoc `@throws`); a rejected Promise | `raise E` in the body; docstring `Raises:` | the last result `error` | `throws E` | XML doc `<exception cref="E">` | `throw E`; `noexcept(false)` |
| **panics** | — | what can go wrong | `# Panics`; `panic!` / `.expect` in the body | throws not declared anywhere | `assert` / unconditional `raise` | `panic(…)` | unchecked exceptions | exceptions not documented | `std::terminate`; UB |
| **you promise** | — | what can go wrong | `unsafe fn` plus `# Safety` | — | — | — | — | `unsafe` blocks | preconditions ("the behaviour is undefined if …") |
| **since · changed** | — | what changed → **What changed** | release API diffs (releases.mjs); std's `#[stable(since)]` | npm version diffs; `@since` | PyPI diffs; `versionadded` / `versionchanged` | module version diffs | `@since`; Maven version diffs | NuGet version diffs | release diffs |
| **lives in** | — | the marks line (path + re-export alias) | module path; `pub use` re-exports | module / export path | `package.module` | package import path | package | namespace | namespace |

## 2. Qualifiers (apply to any row or item)

| Qualifier | Mark | Rust | TS / JS | Python | Go | Java | C# | C++ |
|---|---|---|---|---|---|---|---|---|
| **gated** (on only with a feature) | key + feature name. It shows **only when off for you**; the card gives the predicate, why it is on ("through serde_json’s std") and the line to add. | `#[cfg(feature = "x")]`, `doc(cfg(...))` | conditional exports; `optionalDependencies` | `extras_require`; `TYPE_CHECKING` | build tags `//go:build` | Maven / Gradle profiles | `#if SYMBOL` | `#ifdef` / `#if defined(...)` |
| **deprecated** | the name is struck through, plus a strike mark; the card gives since and note | `#[deprecated(since, note)]` | `@deprecated` | `@deprecated`; `warnings.warn(DeprecationWarning)` | `// Deprecated:` | `@Deprecated` (`forRemoval`, `since`) | `[Obsolete("…")]` | `[[deprecated("…")]]` |
| **when** (a condition from an impl block) | a group heading in words ("when its items copy freely") with the exact header under ⌥ | `impl<A> X<A> where A::Item: Copy` | overloads with `this: X<T>` | `@overload` with narrower types | — | bounded generic methods | extension methods on constrained types | `requires` on members |

## 3. Arrival: how a capability arrives (the "can" line). This extends `caps`; it adds no new vocabulary.

| Glyph | Arrival | Rust | TS / JS | Python | Go | Java | C# | C++ |
|---|---|---|---|---|---|---|---|---|
| hollow ◇ | **derived** | `#[derive(..)]` | decorators that add members | `@dataclass(order=True)`, `@total_ordering` | — | Lombok / records' generated members | records' generated members | `= default` |
| solid ◆ | **written** | `impl Tr for T { … }` | `implements` with bodies | methods defined in the class | methods on the type | `implements` with bodies | `: I` with bodies | overrides |
| dashed ◇ | **through another trait** (a blanket impl) | `impl<T: Display> ToString for T` | — | inherited from a mixin | — | inherited `default` methods | inherited default interface methods | CRTP / templates |
| dotted ◇ | **from its parts** (auto) | `Send`, `Sync`, `Unpin`, `UnwindSafe`, `RefUnwindSafe`, `Freeze` | — | — | — | — | — | trivially copyable / movable traits |

**Omission rule.** A section, group or cap that the language cannot have is **not drawn**. It is not drawn empty and not drawn with a dash. Examples:

- Go has no "gives for free" group.
- Java, C# and Python have no dashed or dotted caps.
- TypeScript has no "you promise".

"The usual N" counts only what the language has.

## 4. What the prototype's world supplies today, and what it doesn't

| Verb | Source in page2 | Honest state |
|---|---|---|
| comes from, taken by, held by, called on by, called by, calls, used by, done by | world.json edges (`gives`, `takes`, `type`, `calls`, `uses`, `impl`/`derives`) | real; Rust only (the world is Rust) |
| asked for by | the world's `gen` / `wh` strings, parsed for the trait name (`boundUsers`) | real; bound spelling is lost for types (the extractor strips type-level bounds) |
| becomes | parsed `impl From<T> for X` (rustsrc.mjs) | real for the five items; the world has no edge |
| acts like | the parsed `Deref` impl, `type Target`, plus rust-src slice methods with `#[stable(since)]` | real (125 methods, Rust 1.0.0–1.94.0) |
| fails with | the pipe's `Result<_, E>` plus the error type's `classify()` body and `ErrorCode` Display strings | real for serde_json::Error; kinds come from the source, not the index |
| panics, you promise | `# Panics` / `# Safety` doc sections; `.expect("…")` in Index/IndexMut bodies | real; body scanning is limited to Index / IndexMut |
| since · changed | releases.json (toml, smallvec), with paths folded to their declared path and parameter renames ignored | toml: ≤ 0.5.11 (the oldest release read); smallvec: ≤ 1.15.1; **serde_json / serde_core: unknown**, and the page says so |
