# Direct native TypeScript projects

The default local TypeScript authority is the TSZ compiler linked into Nudox. It loads project
configuration, selects roots, resolves modules and checks the resulting programs in Rust. A
project does not need a separately configured `tsc`, Compiler API helper, Node runtime, or an
installed `typescript` package to activate this authority. Install the project's declared
application dependencies with its existing package manager so imported packages and declarations
are present.

Standard library declarations are versioned data linked with TSZ. Their exact bytes, resolved
compiler options and native compiler implementation identity participate in each admitted
program's semantic identity. A host/global TypeScript installation cannot replace that compiler.
An explicitly selected external compiler/report-program configuration retains its separate
compatibility path and strict portable identity checks.

For npm and Yarn workspaces, discovery uses the nearest `package.json` declaring `workspaces`.
For pnpm, it uses the nearest valid `pnpm-workspace.yaml`. Modules, package metadata and symlink
realpaths are admitted through the captured workspace filesystem capability, including hoisted
and pnpm dependencies within that workspace. Resolution does not search beyond its admitted
boundary. Yarn Plug'n'Play remains outside the supported node_modules resolution model.

Distinct `tsconfig*.json` files remain separate program candidates. Angular `angular.json` build
targets and configurations retain their explicit `tsConfig` candidates. TSZ loads relative and
installed-package `extends`, compiler options, `files`/`include`/`exclude` roots and exact module
requests. Project references retain separate program nodes and solution containers. Configured
exclusion retains the captured source's provenance without granting semantic authority to it.

Every program retains its positive file reads, missing paths, directory enumerations and realpath
observations. The compiler revalidates that closure before publication. Changed configuration,
dependency metadata, source bytes or standard library data invalidates its semantic identity.
Cancellation, deadline and finite per-file work allowances remain checked; an interrupted image
cannot supply a fresh query session or be relabeled complete.

The 2026-10-15 direct-authority source controls are authored but **UNRUN** until the gated native
build and regression execution produce receipts. This document describes the source contract and
does not establish deployment, runtime equivalence, performance or full-report recovery.
