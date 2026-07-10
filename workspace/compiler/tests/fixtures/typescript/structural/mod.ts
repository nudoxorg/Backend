/**
 * TypeScript structural types (keyof / mapped / conditional) as first-class IR.
 */

/** A plain record used as the basis for the structural type aliases. */
export interface Person {
  name: string;
  age: number;
}

/** `keyof` lowers to a `Type::TypeOperator`. */
export type PersonKeys = keyof Person;

/** A mapped type lowers to `Type::Mapped`. */
export type ReadonlyPerson = {
  readonly [K in keyof Person]: Person[K];
};

/** A conditional type lowers to `Type::Conditional`. */
export type IsString<T> = T extends string ? true : false;
