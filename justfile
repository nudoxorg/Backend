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
system            := `rustc --version --verbose |  grep '^host:' | awk '{print $2}'`
main_package      := "nudox"

# ▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰ #
#      Recipes      #
# ▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰▰ #

[doc('List all available recipes')]
[default]
list:
	@just --list

bump:
	#!/usr/bin/env nu

	let prompt = (r##'
	Provided is a diff, please use it to create a concise commit message that adheres to the conventional commit standards.
	Write a commit message in the Conventional Commits format. Use the structure:
	    <type>(<optional scope>): <short description>
    
	    <optional body>
    
	    <optional footer>

    
		avaible types:
		build: The commit introduces a change that affect the build system or external dependencies.
		chore: The commit includes necessary technical tasks to take care of the product or repository, but it's not related to any specific feature or user story. These tasks are like routine maintenance, such as releasing the product or updating code for the repository.
		ci: The commit involves changes to the continuous integration (CI) configuration or scripts used to automate build, testing, and deployment processes.
		docs: The commit updates or adds documentation, such as README files, comments, or user guides, without affecting the code's functionality.
		feat: The commit introduces a new feature or enhancement to the product or codebase.
		fix: The commit addresses and resolves a bug, error, or issue in the codebase.
		perf: The commit makes code changes aimed at improving performance or optimizing existing functionality.
		refactor: The commit involves code refactoring, which means restructuring or reorganizing the code without changing its external behavior.
		revert: The commit undoes a previous commit, reverting the codebase to a previous state.
		style: The commit deals with code style changes, such as formatting, indentation, or code comment adjustments, without affecting the code's functionality.
		test: The commit includes changes related to testing, such as adding or modifying test cases, test suites, or testing infrastructure.

		Optionally, include a body for more details in bullet points.

		Optionally, fill in the footer with supplementary information, these can be one of four things:

		Fixes: <issue number> or Closes: <issue number> Links the commit to the specified issue and automatically closes it when the commit is merged into the default branch.
		Optionally, in the footer, use BREAKING CHANGE: followed by a detailed explanation of the breaking change.
		Refs: <pull request number> Links the commit to the specified pull request.
		Co-authored-by: <name> [email] Credits other contributors who collaborated on the commit.

		The scope should be inspired by the filename of the files edited.

		Just return the commit message, do not include any other text.
		DO NOT include thinking tokens, explanations, or any preamble.
	'## | str trim)

	let diff = (git --no-pager diff --cached)

	if ($diff | str trim | is-empty) {
	    print "No staged changes to commit"
	    exit 1
	}

	let full_prompt = $"($prompt)\n\nHere is the diff:\n($diff)"

	let llm_output = ($full_prompt | ollama run qwen3:1.7b | str trim)

	# Filter out lines that look like thinking tokens or metadata
	let commit_message = if ($llm_output | str contains 'done thinking') {
	let lines = ($llm_output | lines)
	let thinking_end_idx = ($lines | enumerate | where {|it| $it.item | str contains 'done thinking'} | first | get index)
	    $lines | skip ($thinking_end_idx + 1) | str join "\n" | str trim
	} else {
	    $llm_output
	}

	$commit_message | git commit --edit --file -

[doc('List patches')]
[no-cd]
patches filter="--open":
	#!/usr/bin/env nu
	let root = '{{ project_root }}'
	let current_dir = (pwd | path expand)
	# Check if we're in the root directory
	if $current_dir == $root {
		rad patch list {{ filter }}
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
	    | get $'submodule "($project_name)"'
	    | get url 
	    | str replace --all "//" ""
	    | str substring 0..32
	)
	# Run rad patch list command
	rad patch list {{ filter }} --repo $rad_id

[doc('Show patch details')]
[no-cd]
show_patch patch_id verbose="":
	#!/usr/bin/env nu
	let root = '{{ project_root }}'
	let current_dir = (pwd | path expand)
	let verbose_flag = if "{{ verbose }}" == "true" { "--verbose" } else { "" }
	# Check if we're in the root directory
	if $current_dir == $root {
		rad patch show {{ patch_id }} $verbose_flag
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
	    | get $'submodule "($project_name)"'
	    | get url 
	    | str replace --all "//" ""
	    | str substring 0..32
	)
	# Run rad patch show command
	rad patch show {{ patch_id }} $verbose_flag --repo $rad_id

[doc('Checkout a patch')]
[no-cd]
checkout_patch patch_id:
	#!/usr/bin/env nu
	let root = '{{ project_root }}'
	let current_dir = (pwd | path expand)
	# Check if we're in the root directory
	if $current_dir == $root {
		rad patch checkout {{ patch_id }}
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
	    | get $'submodule "($project_name)"'
	    | get url 
	    | str replace --all "//" ""
	    | str substring 0..32
	)
	# Run rad patch checkout command
	rad patch checkout {{ patch_id }} --repo $rad_id

[doc('Archive a patch')]
[no-cd]
archive_patch patch_id:
	#!/usr/bin/env nu
	let root = '{{ project_root }}'
	let current_dir = (pwd | path expand)
	# Check if we're in the root directory
	if $current_dir == $root {
		rad patch archive {{ patch_id }}
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
	    | get $'submodule "($project_name)"'
	    | get url 
	    | str replace --all "//" ""
	    | str substring 0..32
	)
	# Run rad patch archive command
	rad patch archive {{ patch_id }} --repo $rad_id

[doc('Review a patch')]
[no-cd]
review_patch patch_id accept_or_reject:
	#!/usr/bin/env nu
	let root = '{{ project_root }}'
	let current_dir = (pwd | path expand)
	let action = if "{{ accept_or_reject }}" == "accept" { "--accept" } else { "--reject" }
	# Check if we're in the root directory
	if $current_dir == $root {
		rad patch review {{ patch_id }} $action 
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
	    | get $'submodule "($project_name)"'
	    | get url 
	    | str replace --all "//" ""
	    | str substring 0..32
	)
	# Run rad patch review command
	rad patch review $action --repo $rad_id {{ patch_id }}

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
