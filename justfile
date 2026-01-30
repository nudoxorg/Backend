#!/usr/bin/env just
 
# --- Settings --- #
set shell                     := ["nu", "-c"]
set positional-arguments      := true
set allow-duplicate-variables := true
set windows-shell             := ["nu", "-c"]
set dotenv-load               := true

# --- Variables --- #
project_root      := justfile_directory()
output_directory  := project_root + "/dist"
build_directory   := `(cargo metadata --format-version 1 | from json).target_directory`
system            := `rustc --version --verbose |  grep '^host:' | awk '{print $2}'`
main_package      := "nudox"

# ▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰ #
#      Recipes      #
# ▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰ #

[doc('List all available recipes')]
[default]
list:
	@just --list

[doc('Explain an issue in detail')]
[no-cd]
explain_issue issue_id:
	#!/usr/bin/env nu
	let root = '{{ project_root }}'
	let current_dir = (pwd | path expand)
	# Check if we're in the root directory
	if $current_dir == $root {
		rad issue show {{ issue_id }} --verbose
	    exit 0
	} 
	# Get the current git project root
	let current_project = (git rev-parse --show-toplevel | complete | get stdout | str trim)
	# Extract just the directory name (basename) from the full path
	let project_name = ($current_project | path basename)
	# Read and parse .gitmodules file
	let gitmodules_path = $"($root)/.gitmodules"
	# Extract the rad_id from .gitmodules
	let rad_id = (
	    open $gitmodules_path 
	    | from ini 
	    | get $'submodule "($project_name)"'
	    | get url 
	    | str replace --all "//" ""
	    | str substring 0..32
	)
	# Run rad issue show command with verbose output
	rad issue show {{ issue_id }} --verbose --repo $rad_id

[doc('Create a new issue')]
[no-cd]
create_issue title description="":
	#!/usr/bin/env nu
	let root = '{{ project_root }}'
	let current_dir = (pwd | path expand)
	# Check if we're in the root directory
	if $current_dir == $root {
		rad issue open --title "{{ title }}" --description "{{ description }}"
	    exit 0
	} 
	# Get the current git project root
	let current_project = (git rev-parse --show-toplevel | complete | get stdout | str trim)
	# Extract just the directory name (basename) from the full path
	let project_name = ($current_project | path basename)
	# Read and parse .gitmodules file
	let gitmodules_path = $"($root)/.gitmodules"
	# Extract the rad_id from .gitmodules
	let rad_id = (
	    open $gitmodules_path 
	    | from ini 
	    | get $'submodule "($project_name)"'
	    | get url 
	    | str replace --all "//" ""
	    | str substring 0..32
	)
	# Run rad issue open command
	rad issue open --title "{{ title }}" --description "{{ description }}" --repo $rad_id

[doc('List issues')]
[no-cd]
issues:
	#!/usr/bin/env nu

	let root = '{{ project_root }}'
	let current_dir = (pwd | path expand)

	# Check if we're in the root directory
	if $current_dir == $root {
		rad issue
	    exit 0
	} 

	# Get the current git project root
	let current_project = (git rev-parse --show-toplevel | complete | get stdout | str trim)

	# Extract just the directory name (basename) from the full path
	let project_name = ($current_project | path basename)

	# Read and parse .gitmodules file
	let gitmodules_path = $"($root)/.gitmodules"

	# Extract the rad_id from .gitmodules
	let rad_id = (
	    open $gitmodules_path 
	    | from ini 
	    | get $'submodule "($project_name)"'
	    | get url 
	    | str replace --all "//" ""
	    | str substring 0..32
	)

	# Run rad issue command
	rad issue --repo $rad_id	

# --- Build & Check --- #
[doc('Check workspace for compilation and syntax errors')]
[group('build')]
check package=(main_package) target=(system):
	@echo "🔎 Checking workspace..."
	cargo check --workspace
	cargo clippy --workspace --bin '{{ package }}' --target '{{ target }}'
	typos check 

[doc('Build workspace in debug mode')]
[group('build')]
build package=(main_package) target=(system):
	@echo "🔨 Building workspace (debug)..."
	cargo build --workspace --bin '{{ package }}' --target '{{ target }}'

[doc('Build workspace in release mode')]
[group('build')]
build-release target=(system) package=(main_package):
	@echo "🚀 Building workspace (release) for {{ target }}…"
	cargo build --workspace --release --bin '{{ package }}' --target '{{ target }}'

# --- Packaging --- #
[doc('Package release binary with completions for distribution')]
[group('packaging')]
package target=(system):
### IMPORT: scripts/build-error.nu 1 ###
### IMPORT: scripts/package.nu 1 ###

[doc('Generate checksums for distribution files')]
[group('packaging')]
checksum directory=(output_directory):
### IMPORT: scripts/build-error.nu 1 ###
### IMPORT: scripts/checksum.nu 1 ###

[doc('Compress all release packages into tar.gz archives')]
[group('packaging')]
compress directory=(output_directory):
### IMPORT: scripts/build-error.nu 1 ###
### IMPORT: scripts/compress.nu 1 ###

[doc('Complete release pipeline: build, checksum, and compress')]
[group('packaging')]
release: build-release
	just checksum

# --- Execution --- #
[doc('Run application in debug mode')]
[group('execution')]
run package=(main_package) +args="":
	@echo "▶️ Running {{ package }} (debug)..."
	cargo run --bin '{{ package }}' -- '$@'

[doc('Run application in release mode')]
[group('execution')]
run-release package=(main_package) +args="":
	@echo "▶️ Running '{{ package }}' (release)..."
	cargo run --bin '{{ package }}' --release -- '$@'

# --- Testing --- #
[doc('Run all workspace tests')]
[group('testing')]
test:
	@echo "🧪 Running workspace tests..."
	cargo test --workspace

[doc('Run workspace tests with additional arguments')]
[group('testing')]
test-with +args:
	@echo "🧪 Running workspace tests with args: '$@'"
	cargo test --workspace -- '$@'

# --- Code Quality --- #
[doc('Format all Rust code in the workspace')]
[group('quality')]
fmt:
	@echo "💅 Formatting Rust code..."
	cargo fmt 

[doc('Check if Rust code is properly formatted')]
[group('quality')]
fmt-check:
	@echo "💅 Checking Rust code formatting..."
	cargo fmt 

[doc('Lint code with Clippy in debug mode')]
[group('quality')]
lint:
	@echo "🧹 Linting with Clippy (debug)..."
	cargo clippy --workspace -- -D warnings

[doc('Automatically fix Clippy lints where possible')]
[group('quality')]
lint-fix:
	@echo "🩹 Fixing Clippy lints..."
	cargo clippy --workspace --fix --allow-dirty --allow-staged

# --- Documentation --- #
[doc('Generate project documentation')]
[group('common')]
doc:
	@echo "📚 Generating documentation..."
	cargo doc --workspace --no-deps

[doc('Generate and open project documentation in browser')]
[group('common')]
doc-open: doc
	@echo "📚 Opening documentation in browser..."
	cargo doc --workspace --no-deps --open

# --- Maintenance --- #
[doc('Extract release notes from changelog for specified tag')]
[group('common')]
create-notes raw_tag outfile changelog:
### IMPORT: scripts/build-error.nu 1 ###
### IMPORT: scripts/create-notes.nu 1 ###

[doc('Update Cargo dependencies')]
[group('maintenance')]
update:
	@echo "🔄 Updating dependencies..."
	cargo update

[doc('Clean build artifacts')]
[group('maintenance')]
clean:
	@echo "🧹 Cleaning build artifacts..."
	cargo clean

# --- Installation --- #
[doc('Build and install binary to system')]
[group('installation')]
install package=(main_package): build-release
	@echo "💾 Installing {{ main_package }} binary..."
	cargo install --bin '{{ package }}'

[doc('Force install binary')]
[group('installation')]
install-force package=(main_package): build-release
	@echo "💾 Force installing {{ main_package }} binary..."
	cargo install --bin '{{ package }}' --force

# --- Aliases --- #
alias b    := build
alias br   := build-release
alias c    := check
alias t    := test
alias f    := fmt
alias l    := lint
alias lf   := lint-fix
alias cl   := clean
alias up   := update
alias i    := install
alias ifo  := install-force
alias rr   := run-release
