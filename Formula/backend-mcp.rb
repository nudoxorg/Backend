# frozen_string_literal: true

# Homebrew formula for the loopback MCP service.
#
# The default install pours the published v0.2.0 release binaries (macOS
# arm64). `brew install --HEAD` still compiles from a blank-state clone of
# the GitHub default branch (canonical), which carries this formula:
#
#   brew tap nudoxorg/backend https://github.com/nudoxorg/Backend.git
#   brew install --HEAD nudoxorg/backend/backend-mcp
#   brew services start backend-mcp
#
# The service listens on http://127.0.0.1:8741/mcp. The bearer token is
# created on first start at $(brew --prefix)/etc/backend-mcp.token.
# Add a project by calling backend.index with its absolute path. No project
# file is required. Daemon state stays in $(brew --prefix)/var/lib/backend-mcp.
# The v0.2.0 bottle is an older binary; --HEAD builds the session tool list.
# backend-mcp and backend-locald are installed as siblings; the MCP process
# finds the daemon beside its own executable.

# Loopback MCP service formula. The class comment is the Homebrew desc.
class BackendMcp < Formula
  desc "Loopback MCP service for the Nudox local code-intelligence daemon"
  homepage "https://github.com/nudoxorg/backend"
  license "MIT OR Apache-2.0"

  # Prebuilt service binaries from the v0.2.1 GitHub release.
  url "https://github.com/nudoxorg/Backend/releases/download/v0.2.1/backend-mcp-macos-arm64.tar.gz"
  version "0.2.1"
  sha256 "adba9dca17fb0646b3b6cbdb6b385a14a3dd0eed489f1c92c946216d369d7f55"

  # The GitHub default branch (canonical) carries the formula. `brew install
  # --HEAD` clones it into a fresh Homebrew build cell, so the compile does
  # not see a developer checkout or its Cargo target directory.
  head "https://github.com/nudoxorg/backend.git"

  depends_on "git" => :build if build.head?
  depends_on "rust" => :build if build.head?

  def install
    if build.head?
      # Thin LTO of the rust-analyzer closure (codegen-units = 1) is the
      # workspace release profile. Keep it, but do not fan out rustc jobs: a
      # blank machine with a small memory budget dies in LLVM before linking.
      ENV["CARGO_BUILD_JOBS"] = "1"
      system "cargo", "install", "--locked", "--root", prefix, "--path", "apps/locald"
      system "cargo", "install", "--locked", "--root", prefix, "--path", "apps/mcp"
    else
      bin.install "bin/backend-mcp"
      bin.install "bin/backend-locald"
    end
    (bin/"backend-mcp-service").write service_script
    chmod 0555, bin/"backend-mcp-service"
  end

  post_install_steps do
    mkdir_p "lib/backend-mcp", base: :var
    mkdir_p "log", base: :var
  end

  def caveats
    <<~EOS
      Start the loopback MCP service:
        brew services start backend-mcp
      Endpoint: http://127.0.0.1:8741/mcp
      First start creates the bearer token:
        #{etc}/backend-mcp.token
      Authorization: Bearer $(cat #{etc}/backend-mcp.token)

      HTTP mode does not index on startup. After initialize, call
      backend.index with the absolute project path. That adds the package.
      backend.packages lists the shelf. Daemon state stays in
      #{var}/lib/backend-mcp.
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
      state_dir="#{var}/lib/backend-mcp"
      project_file="${BACKEND_MCP_PROJECT_FILE:-$etc_dir/backend-mcp.project}"
      token_file="${BACKEND_MCP_TOKEN_FILE:-$etc_dir/backend-mcp.token}"
      bind="${BACKEND_MCP_HTTP:-127.0.0.1:8741}"
      mkdir -p "$state_dir"

      # backend.index admits the directory. A project path only sets the
      # default used when that tool is called without a path argument.
      project_args=()
      if [[ -n "${BACKEND_MCP_PROJECT:-}" ]]; then
        if [[ ! -d "$BACKEND_MCP_PROJECT" ]]; then
          echo "backend-mcp-service: project is not a directory: $BACKEND_MCP_PROJECT" >&2
          exit 1
        fi
        project_args=(--project "$BACKEND_MCP_PROJECT")
      elif [[ -f "$project_file" ]]; then
        IFS= read -r BACKEND_MCP_PROJECT < "$project_file"
        if [[ ! -d "$BACKEND_MCP_PROJECT" ]]; then
          echo "backend-mcp-service: project is not a directory: $BACKEND_MCP_PROJECT" >&2
          exit 1
        fi
        project_args=(--project "$BACKEND_MCP_PROJECT")
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

      exec "$opt_bin/backend-mcp" --http "$bind" --workspace "$state_dir" "${project_args[@]}"
    SH
  end

  test do
    assert_match "usage:", shell_output("#{bin}/backend-mcp --help")
  end
end
