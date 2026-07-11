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
  constructor(public readonly id: string) {}
}

/** An interface → Entry::TraitDef. */
export interface Printable {
  print(): void;
}

/** A type alias → Entry::TypeAlias. */
export type StringOrNumber = string | number;

/** An enum → Entry::SumType. */
export enum Direction {
  Up,
  Down,
  Left,
  Right,
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
