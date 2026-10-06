# CLI and Claude Code quick start

The installed shell command is `nudox`. From the repository you want to work
with, index it once and search it directly:

```console
nudox add .
nudox search "error handling"
```

`add` accepts a project path; `.` means the current directory. It starts the
local owner if one is not already available and reuses the matching owner on
later commands. The first index can take time. `show` takes the exact
coordinate printed by `search`; do not substitute a filename or guessed name.
Pass that exact coordinate to `nudox show` when you need the signature and
documentation.
`index` is an alias for `add`, and `nudox --help` lists the command families.

The MCP server is a local stdio process. Install the matched
`backend-mcp` and `backend-locald` executables together, with `backend-mcp`
available on `PATH`, then register it once for Claude Code:

```console
claude mcp add --scope user --transport stdio nudox \
  -- "$(command -v backend-mcp)" --project '${CLAUDE_PROJECT_DIR:-.}'
claude mcp get nudox
```

The quoted project expression is expanded by Claude Code when it launches the
server, so the user-scoped registration follows the active Claude project.
The MCP server uses the `backend-locald` companion from the same installed
release when it needs to start the local owner. If the binaries are not on
`PATH`, add their installed `bin` directory to `PATH` before running the
registration command. In Claude Code, `/mcp` shows the live connection.

After connection, the MCP tool sequence is `backend.index` with the absolute
project path, then `backend.search` with the query. The server handshake gives
the selected project and launch directory; `backend://workspace/current`
reports them again if a result looks empty. `backend.package` looks up one
pinned registry package, while `backend.index_search` finds names in the local
registry index, including packages that are not on the project shelf.

An empty shelf is not evidence that a project has no declarations. Run the
index command for the project you intend to search, and use `backend.status`
when you need to distinguish an empty index from a capability that is not
configured.
