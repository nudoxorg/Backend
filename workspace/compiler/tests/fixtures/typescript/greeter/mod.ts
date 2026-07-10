/**
 * A tiny greeting module used to exercise export vs. private lowering.
 */

/** The default greeting used when none is supplied. */
export const DEFAULT_GREETING: string = "Hello";

/** A private greeting that must never leak into the public surface. */
const SECRET_GREETING: string = "psst";

/** Greets a subject by name. */
export function greet(name: string): string {
  return `${DEFAULT_GREETING}, ${name}!`;
}

/** A greeter that can be reused across calls. */
export class Greeter {
  constructor(private readonly prefix: string = DEFAULT_GREETING) {}

  greet(name: string): string {
    return `${this.prefix}, ${name}!`;
  }
}

/** A private greeter that stays internal. */
class WhisperGreeter {
  whisper(name: string): string {
    return `(${SECRET_GREETING} ${name})`;
  }
}
