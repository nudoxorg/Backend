interface X<T> {
  p: T extends string ? { value: T } : { value: number };
}
export const x: X<unknown> = null as never;
