# Installed TypeScript projects

Nudox uses the TypeScript compiler API from the project’s installed `typescript` package. A
project does not need a globally installed `tsc` or a `NUDOX_TSC` setting. Install TypeScript in
the project or its workspace using the package manager already used there:

```sh
npm install --save-dev typescript
pnpm add --save-dev typescript
yarn add --dev typescript
```

The selected project must also have a Node runtime supported by its TypeScript release. An
explicit `NUDOX_TYPESCRIPT_NODE` value takes precedence. Otherwise, a recognized Nudox macOS app
launch admits the bundled Node only after the app manifest binds both the running executable and
Node payload by size and digest. Other launches resolve `node` from the process `PATH`, then check
finite platform locations. Setup canonicalizes the executable, records its version and bytes, and
rechecks that identity before publishing compiler results. Nudox does not install Node or override
the TypeScript release’s runtime compatibility checks. Compiler child processes receive only the
admitted Node path, not the host’s `PATH` or shell environment.

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
