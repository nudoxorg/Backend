# Installed CLI/MCP onboarding checkpoint

Base: canonical821e2a16e3e51f0e61b3359ec602d890d883c93b, separately audited against current validation sourcefd86d2b7b2976dd7c81411242406890738af9d31. Root's actual35 help-only probes passed4/4; that is not end-to-end indexing or MCP owner acceptance. This branch does not mutate either validation source or its artifacts.

Concrete gap: CLI quick-start registration depended on `command -v backend-mcp`, so an installer invocation from a fresh shell could print a path but provide no usable absolute registration command. No local setup generator existed. The guide also showed add followed immediately by search although publication may still be pending. HTTP mode already requires explicit `--http`; stdio does not start an HTTP listener.

`nudox mcp-config [--project PATH]` is intercepted before workspace discovery. It only observes this running CLI's matched executable companions and the selected project directory, then prints complete stdio mcpServers JSON plus POSIX-shell Claude Code/Codex instructions; `--format json` prints only JSON. It never runs a companion, opens a workspace, reads/writes client configuration, removes an existing entry or indexes. Repeating generation is idempotent. The JSON explicitly names the selected absolute project and public MCP launcher; it does not depend on client cwd or build-time paths. No BACKEND_LOCALD_BIN override is saved: the MCP runtime canonicalizes its own executable and discovers its matched sibling when a tool needs the owner, preserving the pairing even if stable aliases are upgraded later. Existing explicit ambient overrides retain their normal runtime semantics. Claude local registration changes to that selected project in a quoted subshell. Existing entries must be inspected before the user chooses to apply a registration command.

The current Linux release source has flat bin executables. macOS investor bundles also contain nudox-mcp/nudox-locald environment wrappers, preferred when present. Stable aliases (including custom prefixes and Homebrew paths) are used only when their CLI, MCP and locald targets match this already-selected bundle. Foreign, absent or nonexecutable companions refuse with the exact candidate path. Unknown internal .libexec/.elf images fail helpfully rather than inventing a parent-layout heuristic or registering raw payloads. No new packaging layout, compiler or environment authority is introduced. Installer output is unchanged: its independently published scripts can install older public releases lacking this new command, so unconditional generator hints would be misleading. The generator is advertised by its matching CLI help and these source-versioned docs.

The normal quick start now waits with add . --wait and requires inspecting the terminal receipt. Generated output and the guide distinguish client-launched stdio / first-tool daemon startup from a separately kept-running loopback HTTP process with the selected Bearer credential. Registration, initialize, tools/list and help never constitute compiler/index acceptance.

Meaningful backend-cli --lib selectors (new bodies UNRUN):

- mcp_config::tests::installed_setup_json_and_shell_keep_exact_paths_without_executing_or_writing_config
- mcp_config::tests::stable_installed_aliases_follow_upgrade_and_never_borrow_a_different_running_release
- mcp_config::tests::macos_bundle_uses_public_environment_wrappers_and_homebrew_aliases
- mcp_config::tests::missing_nonexecutable_or_foreign_companions_refuse_without_path_fallback
- mcp_config::tests::unknown_internal_payload_is_never_registered_as_a_public_launcher
- process::help_tests::special_command_help_works_after_global_options
- tests::help_is_grouped_by_domain_and_names_every_domain

The first fixture parses the complete generated JSON and the real printed shell commands with adversarial paths (spaces, apostrophes, quotes, dollar syntax and Unicode). A fake Claude shell function records actual cwd/argv, proving the emitted local registration targets the selected project; no Claude/Codex/MCP/daemon is launched. Other filesystem fixtures prove upgrade alias stability, refusal to borrow a different currently selected release, macOS wrapper selection and companion refusal. Existing client config bytes remain unchanged. tempfile is an existing workspace/lockfile dependency, newly listed only for CLI test fixtures.

Source checks: Rust2024 parse-only, git diff --check, Python installer AST parse, and a bounded experiment executing the exact fake-client shell literal extracted from the Rust source passed. An initial source experiment caught an incorrectly escaped test printf delimiter; it was corrected before freezing. Rust compilation and the seven native bodies remain UNRUN. Source-only Luna review is required on the final head; Root owns future native admission, installed validation and merge.

Registration syntax was checked against official docs: https://code.claude.com/docs/en/mcp and https://learn.chatgpt.com/docs/extend/mcp?surface=cli. The generator records an explicit project rather than relying on client-specific variable interpolation. Codex uses its emitted CLI instruction, not the mcpServers JSON format.
