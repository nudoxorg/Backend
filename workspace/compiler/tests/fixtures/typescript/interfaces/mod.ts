/**
 * Interfaces with call and index signatures, exercising TraitDef lowering.
 */

/**
 * A greeter interface carrying an ordinary method plus a call signature and
 * an index signature.
 */
export interface Greeter {
  /** An ordinary required method. */
  greet(name: string): string;

  /** A call signature: `greeter(name)`. */
  (name: string): string;

  /** An index signature: `greeter["key"]`. */
  [key: string]: unknown;
}
