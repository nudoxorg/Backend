/** Old-oracle parity fixture for TypeScript declarations and documentation. */
export interface Service<T> { run(value: T): T; }
export class Worker implements Service<string> {
  run(value: string): string { return value; }
}
export function execute(worker: Service<string>): string { return worker.run(""); }
