/**
 * Fixture exercising every SymbolKind mapping in the TypeScript producer.
 *
 * Each declaration maps to a distinct `ir::kind::Entry` variant; the
 * `ts_kind_mapping` test asserts the full table so a silent placeholder
 * regression is immediately caught.
 */

/** A plain function → Entry::Function. */
export function doWork(x: number): number {
  return x * 2;
}

/** A class → Entry::RecordType. */
export class Widget {
  /** Private brand field — must keep `#` prefix + Private visibility. */
  #brand: string = "w";
  constructor(public readonly id: string) {}
}

/** void is unit Tuple; null is a named reference — they must not collapse. */
export type VoidOnly = void;
export type NullOnly = null;

/** An interface → Entry::TraitDef. */
export interface Printable {
  print(): void;
}

/** A type alias → Entry::TypeAlias. */
export type StringOrNumber = string | number;

/** An enum → Entry::SumType. */
export enum Direction {
  Up = 0,
  Down = 1,
  Left = 2,
  Right = 3,
}

/** A const enum — variants flagged `[const]` in documentation. */
export const enum HttpStatus {
  Ok = 200,
  NotFound = 404,
}

/** A const declaration → Entry::Constant. */
export const MAX_SIZE: number = 100;

/** A let declaration → Entry::Variable. */
export let currentCount: number = 0;

/** A namespace → Entry::Module. */
export namespace utils {
  export function identity<T>(x: T): T {
    return x;
  }
}
