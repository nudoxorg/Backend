/**
 * Union, intersection, and inline-object-type fixture.
 *
 * Exercises Type::Union, Type::Intersection, and structural/inline-object
 * types in function signatures and type aliases.
 */

/** A discriminated union of primitive types. */
export type StringOrNumber = string | number;

/** A nullable string (union with null). */
export type MaybeString = string | null;

/** A three-way union. */
export type Tristate = "on" | "off" | "unknown";

/** An intersection type combining two interfaces. */
export interface Named {
  name: string;
}

export interface Aged {
  age: number;
}

/** Intersection of Named and Aged. */
export type NamedAndAged = Named & Aged;

/**
 * A function that accepts a union-typed parameter and returns an inline-object.
 *
 * The inline return object type exercises structural/anonymous object rendering.
 */
export function parse(input: string | number): { value: string; raw: string | number } {
  return { value: String(input), raw: input };
}

/**
 * Long union signature used for two-width render snapshots (40 vs 100 cols).
 *
 * This signature is intentionally verbose to trigger line-breaking at 40 cols
 * while fitting on one line at 100 cols.
 */
export function classify(
  input: string | number | boolean | null | undefined,
): "string" | "number" | "boolean" | "nil" {
  if (input === null || input === undefined) return "nil";
  return typeof input as "string" | "number" | "boolean";
}
