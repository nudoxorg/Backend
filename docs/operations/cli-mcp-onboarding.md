# CLI and Claude Code quick start

The full installer provides the shell command `nudox`. A source build provides
`backend-cli` only when that binary was built; it does not create a `nudox` alias.
Use your actual CLI executable, with its matching `backend-locald` companion.
With the full installation, index and search from the repository:

```console
nudox add . --wait
nudox search "error handling"
```

`add` accepts a project path; `.` means the current directory. It starts the
local owner if one is not already available and reuses the matching owner on
later commands. The first index can take time. `show` takes the exact
coordinate printed by `search`; do not substitute a filename or guessed name.
Pass that exact coordinate to `nudox show` when you need the signature and
documentation.
`index` is an alias for `add`, and `nudox --help` lists the command families.

On a release whose `nudox --help` lists `mcp-config`, run the read-only setup
generator from the repository you want to use:

```console
nudox mcp-config
nudox mcp-config --format json
```

If the installer’s `bin` directory is not on `PATH` yet, use the absolute
`nudox` path printed at installation, followed by `mcp-config`. The generator
prints complete stdio JSON and shell-quoted registration instructions for
Claude Code and Codex. It records this repository’s absolute path; use
`--project /absolute/other/repository` to select another one. It discovers MCP
and its local daemon in the running CLI’s matched bundle, prefers the public
macOS environment wrappers, and retains stable installed aliases only when
all selected companions resolve to that bundle. It never emits a guessed
executable from a different `PATH` installation or an unknown internal payload
layout. No build-time paths are embedded.

The generator does not read or write client configuration, start an owner,
create workspace state, or index the repository. Repeating it prints the same
setup. Inspect an existing `nudox` entry with `claude mcp get nudox` or
`codex mcp list` before applying a registration command; it does not emit a
remove or replacement command. Claude’s generated registration is local to
the current project. Codex’s generated registration explicitly records the
selected project; choose a distinct connector name if you retain several
project-specific registrations. The JSON block is for clients supporting
`mcpServers`; Codex uses the printed CLI command rather than that JSON format.

For an older installed release without `mcp-config`, use the absolute public
MCP executable path printed by its installer, rather than depending on `PATH`.
Replace both example paths with your actual installed command and repository:

```console
claude mcp add --scope local --transport stdio nudox -- "/absolute/installed/bin/backend-mcp" --project "/absolute/repository"
```

Run that local-scope registration from the intended repository. This manual
setup requires the matching `backend-locald` companion installed beside MCP.

For stdio, the MCP client launches the server process and the first
owner-backed tool request starts the matched local daemon when necessary.
Initialization and `tools/list` alone do not start an owner. There is no HTTP
listener in this setup. In Claude Code, `/mcp` shows the live connection.
The complete generated JSON is also usable by clients with a configuration
editor; keep their existing unrelated entries when adding the `nudox` entry.

After connection, call `backend.index` with the absolute project path, copy
`index_poll.call` while work is pending, and inspect its terminal receipt
before `backend.search` with the query. The server handshake gives
the selected project and launch directory; `backend://workspace/current`
reports them again if a result looks empty. `backend.package` looks up one
pinned registry package, while `backend.index_search` finds names in the local
registry index, including packages that are not on the project shelf.

An empty shelf is not evidence that a project has no declarations. Run the
index command for the project you intend to search, and use `backend.status`
when you need to distinguish an empty index from a capability that is not
configured.

MCP does not require the CLI. Use `backend.index` with the absolute project path,
copy `index_poll.call` while work is pending, and inspect the terminal outcome
before searching. `index_poll.shell` is optional: it is supplied only for an
observed CLI in the serving bundle and a reproducible default authority
attachment. The name `nudox` in `claude mcp add` is the connector's name.

HTTP is a separate, explicitly running transport. Use the absolute public MCP
launcher printed in the generated setup:

```console
/absolute/installed/bin/backend-mcp --project /absolute/project --http 127.0.0.1:41449
```

Keep that process running and point the client at
`http://127.0.0.1:41449/mcp`. Neither CLI indexing nor stdio registration
starts this HTTP listener. The generated setup does not edit a service manager
or register HTTP autostart. For HTTP, `BACKEND_MCP_TOKEN` takes precedence over the private
`mcp-http-token` file in the selected workspace. Without the environment
variable, an existing admitted token file is reused; a new token is generated
only when that file is absent. Switching debug/release executables alone does
not rotate the persisted token. Switching from an environment token to a
workspace token, or selecting another workspace, can select a different
credential. Keep the server's selected token consistent with the client's
Bearer token. The startup authentication hint identifies the token source
without printing the token.

Registration syntax references: [Claude Code MCP setup](https://code.claude.com/docs/en/mcp)
and [OpenAI’s Codex MCP documentation](https://learn.chatgpt.com/docs/extend/mcp?surface=cli).
A successful setup command, handshake or help response does not establish
compiler publication or end-to-end indexing/search acceptance.
