/**
 * Fixture exercising IR capabilities that exceed deno_doc's output:
 * literal types with values, template-literal types, `typeof` type queries,
 * named-tuple labels, TC39 `accessor`, static-block synthesis, constructor
 * `this.x` field synthesis, and default-exported functions.
 */

// ── Literal types ─────────────────────────────────────────────────────────────

/** String literal type — deno_doc collapses these to bare `string`. */
export type Lit = "foo";

/** Numeric literal type — deno_doc collapses to bare `number`. */
export type Num = 42;

/** Boolean literal type — deno_doc collapses to bare `boolean`. */
export type Flag = true;

// ── Template-literal type ─────────────────────────────────────────────────────

/** Template-literal — deno_doc flattens to `string`. */
export type Tmpl = `id-${string}`;

// ── typeof type query ─────────────────────────────────────────────────────────

/** The value whose type we query. */
export const base = { a: 1 };

/** `typeof base` — deno_doc stringifies to a plain name reference. */
export type Q = typeof base;

// ── Named tuple ───────────────────────────────────────────────────────────────

/** Named-tuple — deno_doc strips labels and emits a plain Tuple. */
export type Pair = [first: string, second: number];

// ── Class with accessor, static block, and constructor this.x synthesis ───────

/**
 * A class that exercises three upgrade paths:
 *  - `accessor y` (TC39 auto-accessor)
 *  - `get x()` getter method
 *  - `static { }` block (synthetic `__static` member)
 *  - constructor-body `this.z = 1` field synthesis
 */
export class Rich {
  /** TC39 auto-accessor: surfaces as a field with `accessor` decorator. */
  accessor y: number = 0;

  constructor() {
    /** `this.z` is not a PropertyDefinition — synthesised from ctor body. */
    this.z = 1;
  }

  /** Getter method — lowers as Entry::Function named `x`. */
  get x(): number {
    return this.y;
  }

  /** Static initialiser block — synthetic Entry::Function named `__static`. */
  static {
    // side-effect initialisation
  }
}

// ── Default export ────────────────────────────────────────────────────────────

/** Named default export — deno_doc silently drops default-exported decls. */
export default function run(): void {}
