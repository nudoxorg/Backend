export const n: number = 1;
export const inferred = 7;
export const union: string | number = "x";
export interface Holder<T> { value: T; }
export const applied: Holder<number> = { value: 1 };
export const table: Map<string, number> = new Map();
export function total(x: number): number { return x; }
export const fn = () => total(2);
declare function g(x: string): string;
declare function g(x: number): number;
export const callOne = g("a");
export const callTwo = g(2);
export class Box { self(): this { return this; } }
export const made = new Box().self();
export const list: number[] = [];
