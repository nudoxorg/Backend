/**
 * The declaration root resolved from `package.json` `types: "mod.ts"`.
 */

/** A constant exported from the resolved entry point. */
export const ENTRY: string = "entry";

/** A function exported from the resolved entry point. */
export function fromTypesField(): string {
  return ENTRY;
}
