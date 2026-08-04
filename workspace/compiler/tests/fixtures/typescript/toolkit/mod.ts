/**
 * A toolkit module exercising namespaces, classes with private `#fields`,
 * and a default export.
 */

/** A namespace grouping toolkit helpers. */
export namespace toolkit {
  /** The toolkit version. */
  export const VERSION: string = "1.0.0";

  /** A helper reachable through the namespace. */
  export function help(): string {
    return "help";
  }
}

/** Incrementally assembles a path from segments. */
export class Builder {
  #segments: string[] = [];

  /** Append a segment. */
  push(segment: string): this {
    this.#segments.push(segment);
    return this;
  }

  /** Materialize the accumulated path. */
  build(): string {
    return this.#segments.join("/");
  }
}

/** Install the toolkit (default export). */
export default function install(): void {
  // no-op
}
