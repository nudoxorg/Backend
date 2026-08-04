/**
 * Generics, collections, and known-type mapping fixture.
 *
 * Exercises Box<T>, Vec<T>, Option<T>, HashMap<K,V>, HashSet<T> wrappers
 * so the renderer known-type table is covered for TypeScript output.
 */

/** A generic box wrapping a single value. */
export class Box<T> {
  /** The contained value. */
  value: T;

  constructor(v: T) {
    this.value = v;
  }

  /** Unwrap the value. */
  unwrap(): T {
    return this.value;
  }
}

/** A generic stack built on an array (Vec<T> analogue). */
export interface Stack<T> {
  /** Push an element. */
  push(item: T): void;
  /** Pop the top element. */
  pop(): T | null;
  /** Peek without removing. */
  peek(): T | null;
  /** Number of elements in the stack. */
  readonly size: number;
}

/** A key/value store (HashMap<K,V> analogue). */
export interface Store<K, V> {
  get(key: K): V | null;
  set(key: K, value: V): void;
  delete(key: K): boolean;
}

/** Tag set (HashSet<T> analogue). */
export interface TagSet<T> {
  add(tag: T): void;
  has(tag: T): boolean;
  remove(tag: T): boolean;
}

/**
 * A function accepting an optional parameter and returning a tuple-like type.
 * @param items - the items to process
 * @param limit - cap the output (optional)
 */
export function process<T>(items: T[], limit?: number): [T[], number] {
  const out = limit !== undefined ? items.slice(0, limit) : items;
  return [out, out.length];
}

/** A variadic (rest-param) helper. */
export function merge<T>(...sources: T[][]): T[] {
  return ([] as T[]).concat(...sources);
}
