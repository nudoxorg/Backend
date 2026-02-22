package release

// Duration
//
// A Go-style duration string accepted by Concourse (e.g., "1h30m", "10s").
// Composed of one or more unit segments: ns, us, µs, ms, s, m, h.
#Duration: =~"^([0-9]+(ns|us|µs|ms|s|m|h))+$"

// Check Every
//
// Controls how frequently Concourse polls a resource for new versions.
// Accepts any valid #Duration string or the special value "never" to
// disable automatic checking entirely.
#CheckEvery: #Duration | "never"

// Version Config
//
// Selects which version of a resource to use. The string sentinels "every"
// and "latest" map directly to Concourse's built-in behaviours; any other
// value is treated as a pinned version map of string key/value pairs.
#VersionConfig: "every" | "latest" | {[=~".*"]: string}

// Inputs Config
//
// Controls which artifacts are provided to a put step's container.
// "all" passes every artifact in the build plan, "detect" infers
// inputs from the put params, and a list of strings names specific
// artifacts explicitly.
#InputsConfig: "all" | "detect" | [...string]

// Load Var Format
//
// Supported serialisation formats for the load_var step. "detect"
// infers the format from the file extension.
#LoadVarFormat: "detect" | "json" | "yaml" | "yml" | "raw"

// Container Limits
//
// Hardware resource constraints placed on a task container.
#ContainerLimits: close({
	// CPU
	//
	// CPU share limit as an integer (e.g., 512).
	cpu?: int & >=0

	// Memory
	//
	// Memory limit in bytes.
	memory?: int & >=0
})

// Build Log Retention
//
// Policy controlling how many historical build logs Concourse retains
// for a job. Fields are independent and all optional; Concourse applies
// whichever constraints are present.
#BuildLogRetention: close({
	// Builds
	//
	// Absolute number of builds whose logs to keep.
	builds?: int & >=0

	// Minimum Succeeded Builds
	//
	// Floor on the number of successful builds to retain, regardless
	// of the builds or days limits.
	minimum_succeeded_builds?: int & >=0

	// Days
	//
	// Number of days for which to keep build logs.
	days?: int & >=0
})

// Display Config
//
// Visual configuration applied to the pipeline in the Concourse web UI.
#DisplayConfig: close({
	// Background Image
	//
	// URL to an image rendered behind the pipeline graph.
	background_image?: string
})

// Var Source Config
//
// References an external credential manager or variable source.
// The shape of config is fully determined by the named type.
#VarSourceConfig: close({
	// Name
	//
	// Identifier used to reference this var source elsewhere in the pipeline.
	name!: string

	// Type
	//
	// The credential manager type (e.g., vault, credhub, dummy).
	type!: string

	// Config
	//
	// Source-specific connection configuration passed verbatim to the
	// credential manager plugin.
	config!: null | bool | number | string | [...] | {
		...
	}
})

// Group Config
//
// Defines a named subset of jobs and resources shown together in the
// Concourse web UI. Groups have no effect on execution order or
// scheduling.
#GroupConfig: close({
	// Name
	//
	// Unique name of the group as it appears in the UI.
	name!: string

	// Jobs
	//
	// Jobs belonging to this group.
	jobs?: [...string]

	// Resources
	//
	// Resources belonging to this group.
	resources?: [...string]
})

// Image Resource
//
// Defines a container image used by a task or resource type. Unlike a
// pipeline-level resource, this is fetched anonymously and has no name.
// Mirrors Task.dhall ImageResource.
#ImageResource: close({
	// Type
	//
	// The resource type used to fetch the image (e.g., registry-image).
	type!: string

	// Source
	//
	// Source configuration passed to the resource type.
	source!: {
		{[=~".*"]: null | bool | number | string | [...] | {
			...
		}}
		...
	}

	// Params
	//
	// Parameters for the image fetch operation.
	params?: {
		{[=~".*"]: null | bool | number | string | [...] | {
			...
		}}
		...
	}

	// Version
	//
	// Pinned version of the image to fetch.
	version?: {
		{[=~".*"]: string}
		...
	}
})

// Task Run Config
//
// Specifies the executable entry point for a task container.
// Mirrors Task.dhall Run.
#TaskRunConfig: close({
	// Path
	//
	// The command or script to execute.
	path!: string

	// Args
	//
	// Arguments passed to the command.
	args?: [...string]

	// Dir
	//
	// Working directory within the container.
	dir?: string

	// User
	//
	// User account under which the command runs.
	user?: string
})

// Task Input Config
//
// Declares an artifact the task expects to have mounted.
// Mirrors Task.dhall Input.
#TaskInputConfig: close({
	// Name
	//
	// Identifier of the artifact (must match the name of a get step or
	// the output of a previous task).
	name!: string

	// Path
	//
	// Mount path inside the container; defaults to the input name.
	path?: string

	// Optional
	//
	// When true the task proceeds even if this input is absent.
	optional?: bool
})

// Task Output Config
//
// Declares an artifact the task will produce.
// Mirrors Task.dhall Output.
#TaskOutputConfig: close({
	// Name
	//
	// Identifier of the artifact for downstream steps.
	name!: string

	// Path
	//
	// Source path inside the container; defaults to the output name.
	path?: string
})

// Task Cache Config
//
// Declares a directory to be persisted across builds on the same worker.
// Mirrors Task.dhall Cache.
#TaskCacheConfig: close({
	// Path
	//
	// The absolute or relative path within the container to cache.
	path!: string
})

// Task Config
//
// Inline task definition. Requires both platform and run. Either
// image_resource or rootfs_uri must be provided unless the task step
// overrides the image via the image field.
// Mirrors Task.dhall Config.
#TaskConfig: close({
	// Platform
	//
	// Target OS platform (e.g., "linux", "windows", "darwin").
	platform!: string

	// Image Resource
	//
	// Container image definition used to run the task.
	image_resource?: #ImageResource

	// Rootfs URI
	//
	// Alternative URI to a root filesystem archive.
	rootfs_uri?: string

	// Container Limits
	//
	// CPU and memory constraints for this task.
	container_limits?: #ContainerLimits

	// Params
	//
	// Environment variables injected into the task container.
	params?: {
		{[=~".*"]: string}
		...
	}

	// Run
	//
	// Entry point command configuration.
	run!: #TaskRunConfig

	// Inputs
	//
	// Artifacts the task requires.
	inputs?: [...#TaskInputConfig]

	// Outputs
	//
	// Artifacts the task produces.
	outputs?: [...#TaskOutputConfig]

	// Caches
	//
	// Directories persisted across builds.
	caches?: [...#TaskCacheConfig]
})

// In Parallel Config
//
// Advanced configuration for parallel step execution.
// When in_parallel is an object rather than a list, this type is used.
#InParallelConfig: close({
	// Steps
	//
	// The steps to execute in parallel.
	steps!: [...#Step]

	// Limit
	//
	// Maximum number of steps running simultaneously. Unbounded if absent.
	limit?: int & >=1

	// Fail Fast
	//
	// When true, in-flight steps are interrupted as soon as any step fails.
	fail_fast?: bool
})

// Resource Config
//
// Declares an external resource the pipeline interacts with.
// Mirrors Resource.dhall.
#ResourceConfig: close({
	// Name
	//
	// Unique identifier for this resource within the pipeline.
	name!: string

	// Old Name
	//
	// Previous name of the resource. Concourse copies build history across
	// the rename before discarding the old identifier.
	old_name?: string

	// Type
	//
	// The resource type that implements check/get/put for this resource.
	type!: string

	// Source
	//
	// Resource-type-specific connection configuration.
	source!: {
		{[=~".*"]: null | bool | number | string | [...] | {
			...
		}}
		...
	}

	// Icon
	//
	// Material Design icon name rendered in the web UI.
	icon?: string

	// Version
	//
	// When set, Concourse pins the resource to this version and ignores newer ones.
	version?: {
		{[=~".*"]: string}
		...
	}

	// Check Every
	//
	// How frequently Concourse polls for new versions.
	check_every?: #CheckEvery

	// Check Timeout
	//
	// Maximum duration allowed for a single check operation.
	check_timeout?: #Duration

	// Tags
	//
	// Restricts check operations to workers with matching tags.
	tags?: [...string]

	// Public
	//
	// When true, resource metadata is visible to unauthenticated users.
	public?: bool

	// Webhook Token
	//
	// Secret appended to the webhook URL. POST requests to the webhook
	// endpoint trigger an immediate check.
	webhook_token?: string

	// Expose Build Created By
	//
	// When true, the username of whoever triggered the build is passed
	// to the resource's check/get/put operations.
	expose_build_created_by?: bool
})

// Resource Type
//
// Installs a custom resource type into the pipeline.
// Mirrors ResourceType.dhall.
#ResourceType: close({
	// Name
	//
	// Identifier used in resource and step type fields.
	name!: string

	// Type
	//
	// The base resource type used to fetch this resource type's container.
	type!: string

	// Source
	//
	// Configuration for fetching the resource type container image.
	source!: {
		{[=~".*"]: null | bool | number | string | [...] | {
			...
		}}
		...
	}

	// Defaults
	//
	// Default source configuration merged into every resource that uses
	// this type.
	defaults?: {
		{[=~".*"]: null | bool | number | string | [...] | {
			...
		}}
		...
	}

	// Privileged
	//
	// When true, the resource type container runs as root.
	privileged?: bool

	// Check Every
	//
	// Default check interval applied to resources of this type.
	check_every?: #CheckEvery

	// Tags
	//
	// Restricts check operations to workers with matching tags.
	tags?: [...string]

	// Params
	//
	// Additional parameters forwarded to the resource type container.
	params?: {
		{[=~".*"]: null | bool | number | string | [...] | {
			...
		}}
		...
	}
})

// Prototype
//
// Experimental primitive for defining reusable resource-like constructs.
// Structurally identical to ResourceType but kept distinct per the Concourse
// spec so that tooling can distinguish them.
#Prototype: close({
	// Name
	//
	// Unique identifier for this prototype.
	name!: string

	// Type
	//
	// The base resource type used to fetch the prototype container.
	type!: string

	// Source
	//
	// Configuration for fetching the prototype container image.
	source!: {
		{[=~".*"]: null | bool | number | string | [...] | {
			...
		}}
		...
	}

	// Defaults
	//
	// Default configuration merged into usages of this prototype.
	defaults?: {
		{[=~".*"]: null | bool | number | string | [...] | {
			...
		}}
		...
	}

	// Privileged
	//
	// When true, the prototype container runs as root.
	privileged?: bool

	// Check Every
	//
	// How frequently Concourse polls for new versions.
	check_every?: #CheckEvery

	// Tags
	//
	// Restricts workers that may run this prototype.
	tags?: [...string]

	// Params
	//
	// Additional parameters forwarded to the prototype container.
	params?: {
		{[=~".*"]: null | bool | number | string | [...] | {
			...
		}}
		...
	}
})

// Job Config
//
// Defines a unit of execution comprising an ordered plan of steps.
// Mirrors Job.dhall.
#JobConfig: close({
	// Name
	//
	// Unique identifier for the job within the pipeline.
	name!: string

	// Old Name
	//
	// Previous job name. Concourse migrates build history before dropping it.
	old_name?: string

	// Plan
	//
	// The ordered sequence of steps Concourse executes for each build.
	plan!: [...#Step]

	// Public
	//
	// When true, build logs are visible to unauthenticated users.
	public?: bool

	// Disable Manual Trigger
	//
	// Prevents users from manually starting builds via the UI or fly.
	disable_manual_trigger?: bool

	// Serial
	//
	// When true, only one build of this job runs at a time.
	serial?: bool

	// Interruptible
	//
	// When true, a newer build will abort in-progress older builds of
	// this job.
	interruptible?: bool

	// Serial Groups
	//
	// Jobs sharing a serial group name are serialised against each other,
	// not just against themselves.
	serial_groups?: [...string]

	// Max In Flight
	//
	// Maximum number of builds that may run concurrently for this job.
	max_in_flight?: int & >=1

	// Build Logs To Retain
	//
	// Shorthand for retaining a fixed number of build logs.
	build_logs_to_retain?: int & >=0

	// Build Log Retention
	//
	// Fine-grained log retention policy.
	build_log_retention?: #BuildLogRetention

	// On Success
	//
	// Step executed when the job succeeds.
	on_success?: #Step

	// On Failure
	//
	// Step executed when the job fails.
	on_failure?: #Step

	// On Abort
	//
	// Step executed when the job is aborted.
	on_abort?: #Step

	// On Error
	//
	// Step executed when the job errors (as opposed to failing).
	on_error?: #Step

	// Ensure
	//
	// Step executed unconditionally after the job completes.
	ensure?: #Step
})

// Config
//
// Top-level Concourse pipeline configuration.
// Mirrors Pipeline.dhall.
#Config: close({
	// Groups
	//
	// Logical groupings shown as tabs in the Concourse web UI.
	groups?: [...#GroupConfig]

	// Var Sources
	//
	// External credential managers or variable stores.
	var_sources?: [...#VarSourceConfig]

	// Resources
	//
	// External entities the pipeline interacts with.
	resources?: [...#ResourceConfig]

	// Resource Types
	//
	// Custom resource type installations.
	resource_types?: [...#ResourceType]

	// Prototypes
	//
	// Experimental prototype definitions.
	prototypes?: [...#Prototype]

	// Jobs
	//
	// The jobs that make up the pipeline.
	jobs?: [...#JobConfig]

	// Display
	//
	// Visual settings for the pipeline UI.
	display?: #DisplayConfig
})

// Step
//
// A single unit within a job plan. Exactly one discriminator key must be
// present (get, put, task, set_pipeline, load_var, in_parallel, do, try).
// All other fields are optional and specific to the active variant.
//
// This is intentionally a single closed struct rather than a disjunction of
// closed structs. CUE's closed-struct semantics make disjunctions of closed
// types hostile to valid data: every closed branch rejects fields it doesn't
// know about, causing spurious errors. Instead, close() is applied at this
// level and matchN enforces the discriminator constraint.
#Step: close({
	// --- Get step fields ---

	// Get
	//
	// Fetches a version of the named resource. The value is the local
	// name for the fetched artifact within the build plan.
	get?: string

	// --- Put step fields ---

	// Put
	//
	// Pushes to the named resource and emits a new version.
	put?: string

	// Get Params
	//
	// Parameters for the implicit get step that follows a successful put.
	get_params?: {
		{[=~".*"]: null | bool | number | string | [...] | {
			...
		}}
		...
	}

	// Inputs
	//
	// Controls which build artifacts are mounted into the put container.
	inputs?: #InputsConfig

	// --- Task step fields ---

	// Task
	//
	// Executes a command in a container. The value is a display name for
	// the task within the build output.
	task?: string

	// Config
	//
	// Inline task configuration. Mutually exclusive with file.
	config?: #TaskConfig

	// Image
	//
	// Overrides the image used by the task. Must reference a get step
	// that fetched a registry-image resource.
	image?: string

	// Privileged
	//
	// When true, the task container runs as root. Requires the worker
	// to permit privileged containers.
	privileged?: bool

	// Container Limits
	//
	// CPU and memory constraints applied to the task container.
	container_limits?: #ContainerLimits

	// Input Mapping
	//
	// Renames build artifacts before mounting them into the task.
	input_mapping?: {
		{[=~".*"]: string}
		...
	}

	// Output Mapping
	//
	// Renames task outputs before they become build artifacts.
	output_mapping?: {
		{[=~".*"]: string}
		...
	}

	// --- Set pipeline step fields ---

	// Set Pipeline
	//
	// Dynamically configures or updates a pipeline. The value ":self"
	// targets the currently running pipeline.
	set_pipeline?: string

	// Team
	//
	// The team that owns the pipeline being set. Defaults to the current team.
	team?: string

	// Var Files
	//
	// Paths to YAML files whose contents are merged into vars.
	var_files?: [...string]

	// Instance Vars
	//
	// Variables that uniquely identify an instanced pipeline.
	instance_vars?: {
		{[=~".*"]: null | bool | number | string | [...] | {
			...
		}}
		...
	}

	// --- Load var step fields ---

	// Load Var
	//
	// Reads a file and sets a local build variable with the parsed result.
	load_var?: string

	// Format
	//
	// Serialisation format of the file. Defaults to "detect".
	format?: #LoadVarFormat

	// Reveal
	//
	// When true, the variable value is printed in the build log. Use
	// only for non-sensitive values.
	reveal?: bool

	// --- In parallel step fields ---

	// In Parallel
	//
	// Executes a list of steps concurrently. Accepts either a shorthand
	// list of steps or an #InParallelConfig object for limit/fail_fast control.
	in_parallel?: #InParallelConfig | [...#Step]

	// --- Do step fields ---

	// Do
	//
	// Groups a sequence of steps. Useful for attaching hooks to multiple
	// steps as a unit.
	do?: [...#Step]

	// --- Try step fields ---

	// Try
	//
	// Wraps a step and ignores its failure, allowing the plan to continue.
	try?: #Step

	// --- Fields shared across multiple step types ---

	// Resource
	//
	// The pipeline resource to operate on when it differs from the get/put name.
	resource?: string

	// Version
	//
	// Selects which version of the resource to fetch.
	version?: #VersionConfig

	// Passed
	//
	// Constrains the fetched version to those that have passed the listed jobs.
	passed?: [...string]

	// Trigger
	//
	// When true, a new version of this resource automatically triggers the job.
	trigger?: bool

	// File
	//
	// Path to an external configuration file. Used by task (task file path),
	// set_pipeline (pipeline YAML path), and load_var (variable file path).
	file?: string

	// Params
	//
	// Step-specific parameters. For get/put steps these are resource params;
	// for task steps these become environment variables.
	params?: {
		{[=~".*"]: null | bool | number | string | [...] | {
			...
		}}
		...
	}

	// Vars
	//
	// Template variables for interpolation into task configs or pipeline files.
	vars?: {
		{[=~".*"]: null | bool | number | string | [...] | {
			...
		}}
		...
	}

	// --- Hook fields (applicable to all step types) ---

	// Timeout
	//
	// Maximum duration for the step. The step is aborted if it exceeds this.
	timeout?: #Duration

	// Attempts
	//
	// Number of times to retry the step on failure. Must be at least 1.
	attempts?: int & >=1

	// Tags
	//
	// Restricts the step to workers with matching tags.
	tags?: [...string]

	// On Success
	//
	// Step to execute if this step succeeds.
	on_success?: #Step

	// On Failure
	//
	// Step to execute if this step fails.
	on_failure?: #Step

	// On Abort
	//
	// Step to execute if this step is aborted.
	on_abort?: #Step

	// On Error
	//
	// Step to execute if this step errors (distinct from failure).
	on_error?: #Step

	// Ensure
	//
	// Step to execute unconditionally after this step completes.
	ensure?: #Step
} & matchN(1, [
	{get!: string},
	{put!: string},
	{task!: string},
	{set_pipeline!: string},
	{load_var!: string},
	{in_parallel!: _},
	{do!: _},
	{try!: _},
]))

// ===========================================================================
// Well-known resource type schemas
// ===========================================================================

// Git Source
//
// Source configuration for the concourse/git-resource.
// Mirrors Git.dhall Source.Type.
#GitSource: close({
	// URI
	//
	// The URI of the git repository (SSH or HTTPS).
	uri!: string

	// Branch
	//
	// The branch to track. Required for put; optional for get.
	branch?: string

	// Private Key
	//
	// SSH private key for authenticating to the repository.
	private_key?: string

	// Forward Agent
	//
	// When true, SSH agent forwarding is enabled.
	forward_agent?: bool

	// Username
	//
	// Username for HTTPS authentication.
	username?: string

	// Password
	//
	// Password or personal access token for HTTPS authentication.
	password?: string

	// Paths
	//
	// Restrict triggering to commits that touch these paths.
	paths?: [...string]

	// Ignore Paths
	//
	// Suppress triggering for commits that only touch these paths.
	ignore_paths?: [...string]

	// Skip SSL Verification
	//
	// Disables TLS certificate validation. Not recommended for production.
	skip_ssl_verification?: bool

	// Tag Filter
	//
	// Glob pattern restricting which tags are considered versions.
	tag_filter?: string

	// Fetch Tags
	//
	// When true, all tags are fetched along with the repository.
	fetch_tags?: bool

	// Submodule Credentials
	//
	// Per-host credentials for private submodules.
	submodule_credentials?: [...#GitSubmoduleCredential]

	// Git Config
	//
	// Additional git config entries applied before any operation.
	git_config?: [...{mapKey: string, mapValue: string}]

	// Disable CI Skip
	//
	// When true, commits containing [ci skip] are not ignored.
	disable_ci_skip?: bool

	// Commit Verification Keys
	//
	// GPG public keys that commits must be signed with.
	commit_verification_keys?: [...string]

	// Commit Verification Key IDs
	//
	// GPG key IDs that commits must be signed with.
	commit_verification_key_ids?: [...string]

	// GPG Keyserver
	//
	// Keyserver to retrieve public keys from.
	gpg_keyserver?: string

	// Git Crypt Key
	//
	// Base64-encoded git-crypt key for decrypting files in the repository.
	git_crypt_key?: string

	// HTTPS Tunnel
	//
	// Proxy configuration for tunnelling git over HTTPS.
	https_tunnel?: #GitHttpsTunnel

	// Commit Filter
	//
	// Include/exclude filters applied to commit messages.
	commit_filter?: #GitCommitFilter
})

// Git Submodule Credential
//
// Per-host credential for accessing private submodules.
// Mirrors Git.dhall Source.SubmoduleCredential.
#GitSubmoduleCredential: close({
	// Host
	//
	// Hostname of the server hosting the submodule.
	host!: string

	// Username
	//
	// Username for authentication.
	username!: string

	// Password
	//
	// Password or token for authentication.
	password!: string
})

// Git HTTPS Tunnel
//
// Proxy settings for tunnelling git operations over an HTTPS proxy.
// Mirrors Git.dhall Source.HttpsTunnel.
#GitHttpsTunnel: close({
	// Proxy Host
	//
	// Hostname of the HTTPS proxy.
	proxy_host!: string

	// Proxy Port
	//
	// Port of the HTTPS proxy.
	proxy_port!: string

	// Proxy User
	//
	// Username for proxy authentication.
	proxy_user?: string

	// Proxy Password
	//
	// Password for proxy authentication.
	proxy_password?: string
})

// Git Commit Filter
//
// Glob patterns that include or exclude commits from consideration.
// Mirrors Git.dhall Source.CommitFilter.
#GitCommitFilter: close({
	// Include
	//
	// Only consider commits whose message matches one of these patterns.
	include?: [...string]

	// Exclude
	//
	// Ignore commits whose message matches one of these patterns.
	exclude?: [...string]
})

// Git Version
//
// A specific git commit version.
// Mirrors Git.dhall Version.Type.
#GitVersion: close({
	// Ref
	//
	// The full git commit SHA.
	ref!: string
})

// Git Submodules
//
// Controls which submodules are initialised during a get.
// Either a single string ("all" or "none") or a list of paths.
// Mirrors Git.dhall Params.Get.Submodules.
#GitSubmodules: string | [...string]

// Git Get Params
//
// Parameters for the get operation of a git resource.
// Mirrors Git.dhall Params.Get.Type.
#GitGetParams: close({
	// Depth
	//
	// Perform a shallow clone with the given depth.
	depth?: int & >=1

	// Fetch Tags
	//
	// Override the source-level fetch_tags setting for this get.
	fetch_tags?: bool

	// Submodules
	//
	// Which submodules to initialise.
	submodules?: #GitSubmodules

	// Submodule Recursive
	//
	// When true, submodules are initialised recursively.
	submodule_recursive?: bool

	// Submodule Remote
	//
	// When true, submodules track their remote branch rather than the SHA
	// recorded in the superproject.
	submodule_remote?: bool

	// Disable Git LFS
	//
	// Skips pulling LFS objects.
	disable_git_lfs?: bool

	// Clean Tags
	//
	// Removes local tags that do not exist on the remote.
	clean_tags?: bool

	// Short Ref Format
	//
	// Printf format string for the short ref metadata file.
	short_ref_format?: string

	// Describe Ref Options
	//
	// Options forwarded to git describe when generating the describe_ref file.
	describe_ref_options?: string
})

// Git Put Params
//
// Parameters for the put operation of a git resource.
// Mirrors Git.dhall Params.Put.Type.
#GitPutParams: close({
	// Repository
	//
	// Path to the artifact containing the repository to push.
	repository!: string

	// Rebase
	//
	// When true, rebases the local commit onto the remote before pushing.
	rebase?: bool

	// Merge
	//
	// When true, merges the remote into the local branch before pushing.
	merge?: bool

	// Returning
	//
	// Controls what version is emitted: "merged-commit" or "unmerged-commit".
	returning?: "merged-commit" | "unmerged-commit"

	// Tag
	//
	// Path to a file whose contents are used as the tag name.
	tag?: string

	// Only Tag
	//
	// When true, only pushes the tag without pushing the commit.
	only_tag?: bool

	// Tag Prefix
	//
	// String prepended to the tag name.
	tag_prefix?: string

	// Force
	//
	// When true, performs a force push.
	force?: bool

	// Annotate
	//
	// Path to a file whose contents become the annotation message for the tag.
	annotate?: string

	// Notes
	//
	// Path to a file whose contents are stored as a git note on the commit.
	notes?: string
})

// Time Source
//
// Source configuration for the concourse/time-resource.
// Mirrors Time.dhall Source.Type.
#TimeSource: close({
	// Interval
	//
	// How frequently the resource should emit new versions (e.g., "1h").
	interval?: string

	// Location
	//
	// IANA timezone location for start/stop interpretation (e.g., "US/Eastern").
	location?: string

	// Start
	//
	// Start of the time range during which versions are emitted (e.g., "8:00 AM").
	start?: string

	// Stop
	//
	// End of the time range during which versions are emitted (e.g., "5:00 PM").
	stop?: string

	// Days
	//
	// Days of the week on which versions may be emitted (e.g., ["Monday", "Friday"]).
	days?: [...string]
})

// Time Version
//
// A version emitted by the time resource.
// Mirrors Time.dhall Version.Type.
#TimeVersion: close({
	// Time
	//
	// ISO 8601 timestamp of the emitted version.
	time!: string
})

// Registry Image Source
//
// Source configuration for the concourse/registry-image-resource.
#RegistryImageSource: close({
	// Repository
	//
	// The image repository (e.g., "ubuntu" or "myorg/myimage").
	repository!: string

	// Tag
	//
	// The image tag to track. Defaults to "latest".
	tag?: string

	// Username
	//
	// Username for authenticating to the registry.
	username?: string

	// Password
	//
	// Password or token for authenticating to the registry.
	password?: string

	// AWS Access Key ID
	//
	// AWS access key for ECR authentication.
	aws_access_key_id?: string

	// AWS Secret Access Key
	//
	// AWS secret key for ECR authentication.
	aws_secret_access_key?: string

	// AWS Session Token
	//
	// AWS session token for temporary ECR credentials.
	aws_session_token?: string

	// AWS Region
	//
	// AWS region for ECR (e.g., "us-east-1").
	aws_region?: string

	// AWS Role ARN
	//
	// IAM role to assume for ECR authentication.
	aws_role_arn?: string

	// Insecure
	//
	// When true, allows connecting to registries over plain HTTP.
	insecure?: bool

	// Debug
	//
	// When true, enables verbose debug logging.
	debug?: bool

	// Registry Mirror
	//
	// A pull-through cache mirror to use.
	registry_mirror?: #RegistryImageMirror

	// CA Certs
	//
	// Additional CA certificates to trust when connecting to the registry.
	ca_certs?: [...#RegistryImageCaCert]

	// Client Certs
	//
	// Client certificates for mTLS registry authentication.
	client_certs?: [...#RegistryImageClientCert]
})

// Registry Image Mirror
//
// A pull-through cache mirror configuration.
#RegistryImageMirror: close({
	// Host
	//
	// Hostname (and optional port) of the mirror.
	host!: string

	// Username
	//
	// Username for authenticating to the mirror.
	username?: string

	// Password
	//
	// Password for authenticating to the mirror.
	password?: string
})

// Registry Image CA Cert
//
// A trusted CA certificate for a specific registry domain.
#RegistryImageCaCert: close({
	// Domain
	//
	// The registry domain this CA cert applies to.
	domain!: string

	// Cert
	//
	// PEM-encoded CA certificate.
	cert!: string
})

// Registry Image Client Cert
//
// A client certificate for mTLS authentication to a registry domain.
#RegistryImageClientCert: close({
	// Domain
	//
	// The registry domain this client cert applies to.
	domain!: string

	// Cert
	//
	// PEM-encoded client certificate.
	cert!: string

	// Key
	//
	// PEM-encoded private key for the client certificate.
	key!: string
})

// Registry Image Get Params
//
// Parameters for fetching a registry-image resource.
#RegistryImageGetParams: close({
	// Format
	//
	// Output format of the fetched image: "rootfs" (default) or "oci".
	format?: "rootfs" | "oci"

	// Skip Download
	//
	// When true, skips downloading the image layers. Useful for triggering only.
	skip_download?: bool

	// Image Platform
	//
	// Target platform for multi-arch images (e.g., "linux/arm64").
	image_platform?: string
})

// Registry Image Put Params
//
// Parameters for pushing a registry-image resource.
#RegistryImagePutParams: close({
	// Image
	//
	// Path to an OCI image tarball to push.
	image!: string

	// Version
	//
	// Additional version tag to apply.
	version?: string

	// Bump Aliases
	//
	// When true, updates floating tags (e.g., "latest") after the push.
	bump_aliases?: bool

	// Additional Tags
	//
	// Path to a file containing newline-separated additional tags to push.
	additional_tags?: string
})

// Registry Image Version
//
// A specific version of a registry-image resource.
#RegistryImageVersion: close({
	// Digest
	//
	// The image digest (e.g., "sha256:abc123...").
	digest!: string
})

// S3 Source
//
// Source configuration for the concourse/s3-resource.
#S3Source: close({
	// Bucket
	//
	// The S3 bucket name.
	bucket!: string

	// Access Key ID
	//
	// AWS access key ID for authentication.
	access_key_id?: string

	// Secret Access Key
	//
	// AWS secret access key for authentication.
	secret_access_key?: string

	// Session Token
	//
	// AWS session token for temporary credentials.
	session_token?: string

	// AWS Role ARN
	//
	// IAM role to assume for S3 access.
	aws_role_arn?: string

	// Region Name
	//
	// AWS region the bucket is in (e.g., "us-east-1").
	region_name?: string

	// Endpoint
	//
	// Custom S3-compatible endpoint URL.
	endpoint?: string

	// Disable SSL
	//
	// When true, uses HTTP instead of HTTPS.
	disable_ssl?: bool

	// Skip SSL Verification
	//
	// Disables TLS certificate validation. Not recommended for production.
	skip_ssl_verification?: bool

	// Server Side Encryption
	//
	// Encryption algorithm for uploaded objects (e.g., "AES256").
	server_side_encryption?: string

	// SSE KMS Key ID
	//
	// KMS key ID for server-side encryption.
	sse_kms_key_id?: string

	// Use V2 Signing
	//
	// When true, uses Signature Version 2 instead of Version 4.
	use_v2_signing?: bool

	// Regexp
	//
	// A regular expression matching object keys. The latest matching
	// object becomes the current version. Mutually exclusive with versioned_file.
	regexp?: string

	// Versioned File
	//
	// An object key with S3 versioning enabled. Mutually exclusive with regexp.
	versioned_file?: string

	// CloudFront Distribution ID
	//
	// When set, the distribution's cache is invalidated after a put.
	cloudfront_distribution_id?: string
})

// S3 Get Params
//
// Parameters for fetching an S3 resource.
#S3GetParams: close({
	// Skip Download
	//
	// When true, skips downloading the object body.
	skip_download?: bool

	// Unpack
	//
	// When true, unpacks the downloaded archive.
	unpack?: bool

	// Version Filename
	//
	// Overrides the filename used to determine the version from a regexp match.
	version_filename?: string
})

// S3 Put Params
//
// Parameters for pushing to an S3 resource.
#S3PutParams: close({
	// File
	//
	// Glob pattern matching the local file to upload.
	file!: string

	// ACL
	//
	// Canned ACL applied to the uploaded object (e.g., "public-read").
	acl?: string

	// Content Type
	//
	// MIME type set on the uploaded object.
	content_type?: string

	// Cache Control
	//
	// Cache-Control header value for the uploaded object.
	cache_control?: string

	// Path
	//
	// Overrides the destination key in S3.
	path?: string
})

// S3 Version
//
// A specific version of an S3 resource.
#S3Version: close({
	// Path
	//
	// The full S3 object key identifying this version.
	path!: string
})
