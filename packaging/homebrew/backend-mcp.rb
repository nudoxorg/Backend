# frozen_string_literal: true

# Homebrew formula for the loopback MCP service.
#
# Blank-state install (does not reuse a developer target directory):
#
#   brew update
#   brew install --HEAD ./packaging/homebrew/backend-mcp.rb
#   echo /absolute/path/to/project > "$(brew --prefix)/etc/backend-mcp.project"
#   brew services start backend-mcp
#
# The service listens on http://127.0.0.1:8741/mcp. The bearer token is
# created on first start at $(brew --prefix)/etc/backend-mcp.token.
# backend-mcp and backend-locald are installed as siblings; the MCP process
# finds the daemon beside its own executable.

# Loopback MCP service formula. The class comment is the Homebrew desc.
class BackendMcp < Formula
  desc "Loopback MCP service for the Nudox local code-intelligence daemon"
  homepage "https://github.com/nudoxorg/backend"
  license "MIT OR Apache-2.0"

  # Moving integration branch. `brew install --HEAD` clones this branch into
  # a fresh Homebrew build cell, so the compile does not see a developer
  # checkout or its Cargo target directory.
  head "https://github.com/nudoxorg/backend.git", branch: "jimmy/merge-open-prs-088a"

  depends_on "git" => :build
  depends_on "rust" => :build

  def install
    # Thin LTO of the rust-analyzer closure (codegen-units = 1) is the
    # workspace release profile. Keep it, but do not fan out rustc jobs: a
    # blank machine with a small memory budget dies in LLVM before linking.
    ENV["CARGO_BUILD_JOBS"] = "1"
    system "cargo", "install", "--locked", "--root", prefix, "--path", "apps/locald"
    system "cargo", "install", "--locked", "--root", prefix, "--path", "apps/mcp"
    (bin/"backend-mcp-service").write service_script
    chmod 0555, bin/"backend-mcp-service"
  end

  post_install_steps do
    mkdir_p "lib/backend-mcp", base: :var
    mkdir_p "log", base: :var
  end

  def caveats
    <<~EOS
      Write the absolute project path to index, one line, no quotes:
        #{etc}/backend-mcp.project
      First start creates the bearer token:
        #{etc}/backend-mcp.token
      Start the loopback MCP service:
        brew services start backend-mcp
      Endpoint: http://127.0.0.1:8741/mcp
      Authorization: Bearer $(cat #{etc}/backend-mcp.token)

      HTTP mode does not index on startup. Call the backend.index tool
      (or backend.packages) after initialize. State is written to
      <project>/.backend/v2, which this product ignores on later scans.
    EOS
  end

  service do
    run [opt_bin/"backend-mcp-service"]
    keep_alive true
    working_dir var/"lib/backend-mcp"
    log_path var/"log/backend-mcp.log"
    error_log_path var/"log/backend-mcp.log"
    environment_variables BACKEND_MCP_HTTP: "127.0.0.1:8741"
  end

  def service_script
    <<~SH
      #!/bin/bash
      set -euo pipefail
      etc_dir="#{etc}"
      opt_bin="#{opt_bin}"
      project_file="${BACKEND_MCP_PROJECT_FILE:-$etc_dir/backend-mcp.project}"
      token_file="${BACKEND_MCP_TOKEN_FILE:-$etc_dir/backend-mcp.token}"
      bind="${BACKEND_MCP_HTTP:-127.0.0.1:8741}"

      if [[ -z "${BACKEND_MCP_PROJECT:-}" ]]; then
        if [[ ! -f "$project_file" ]]; then
          echo "backend-mcp-service: write the project path to $project_file" >&2
          exit 1
        fi
        IFS= read -r BACKEND_MCP_PROJECT < "$project_file"
      fi
      if [[ ! -d "$BACKEND_MCP_PROJECT" ]]; then
        echo "backend-mcp-service: project is not a directory: $BACKEND_MCP_PROJECT" >&2
        exit 1
      fi

      if [[ -z "${BACKEND_MCP_TOKEN:-}" ]]; then
        if [[ ! -s "$token_file" ]]; then
          mkdir -p "$(dirname "$token_file")"
          umask 077
          od -An -N32 -tx1 /dev/urandom | tr -d '[:space:]' > "$token_file"
          chmod 600 "$token_file"
        fi
        BACKEND_MCP_TOKEN="$(tr -d '[:space:]' < "$token_file")"
      fi
      export BACKEND_MCP_TOKEN

      exec "$opt_bin/backend-mcp" --http "$bind" --project "$BACKEND_MCP_PROJECT"
    SH
  end

  test do
    assert_match "usage:", shell_output("#{bin}/backend-mcp --help")
  end
end
