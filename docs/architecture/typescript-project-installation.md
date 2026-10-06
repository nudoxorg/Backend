# Installed TypeScript projects

Nudox uses the TypeScript compiler API from the project’s installed `typescript` package. A
project does not need a globally installed `tsc` or a `NUDOX_TSC` setting. Install TypeScript in
the project or its workspace using the package manager already used there:

```sh
npm install --save-dev typescript
pnpm add --save-dev typescript
yarn add --dev typescript
```

The selected project must also have a Node runtime supported by its TypeScript release. Nudox uses an explicitly configured
`NUDOX_TYPESCRIPT_NODE` when present; otherwise it resolves `node` from the host’s ordinary
`PATH`, then checks its finite platform locations. Host setup canonicalizes the executable and
records its version and bytes; it does not install Node or override the TypeScript release’s own
runtime compatibility checks. Compiler child processes receive only that admitted Node path, not
the host’s `PATH` or shell environment.

For npm and Yarn workspaces, Nudox stops project discovery at the nearest `package.json` that
declares `workspaces`. For pnpm, it uses the nearest valid `pnpm-workspace.yaml`. It looks for the
workspace’s installed `node_modules/typescript` and uses the package’s direct `bin/tsc` script; it
never executes `node_modules/.bin/tsc`. A bounded regular package-manager shim is only a discovery
marker. Yarn Plug’n’Play projects are reported as unsupported until PnP resolution is implemented.

Project configuration files are captured and checked as part of admission. Distinct `tsconfig*.json`
files remain separate program candidates, and relative or installed-package `extends` and project
reference configs are captured with their parent edges. For Angular workspaces, the `tsConfig`
selected by each build target and build configuration in `angular.json` is also retained as an
explicit candidate. Configuration and source files resolved by the compiler must remain inside
the admitted workspace or compiler module root; changes to those files or to the selected Node or
TypeScript package invalidate the admitted project before its result can be published.
