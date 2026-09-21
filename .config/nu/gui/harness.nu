# Runs the GUI against a real GPUI driver with pinned display, fonts, time,
# viewport, services, and artifact policy. The driver is intentionally external:
# this layer never fabricates a window, package, index result, or screenshot.

def gui-control []: nothing -> record {
    let source = $env.BACKEND_GUI_CONFIG? | default ""
    if ($source | is-empty) or not ($source | path exists) {
        tooling-fail "missing-gui-control" "the Nix GUI control plane is not available" "enter through nix shell .#gui-harness .#gui-tools"
    }
    open $source
}

def gui-root []: nothing -> string {
    let root = repository-root | path join ".local" "gui-artifacts"
    mkdir $root
    $root
}

def gui-lane-root [lane: string]: nothing -> string {
    let root = repository-root | path join ".local" "gui-lanes" $lane
    mkdir $root
    $root
}

def gui-lock-root []: nothing -> string {
    let root = repository-root | path join ".local" "gui-locks"
    mkdir $root
    $root
}

def gui-lane [requested: string]: nothing -> string {
    let lane = if ($requested | is-empty) {
        $env.NUDOX_GUI_LANE? | default "default"
    } else { $requested }
    if $lane !~ '^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}$' {
        tooling-fail "invalid-gui-lane" $"GUI lane names must match [A-Za-z0-9][A-Za-z0-9_.-]{0,63}: ($lane)"
    }
    $lane
}

def gui-manifest [requested: string]: nothing -> string {
    let value = if ($requested | is-empty) {
        let configured = gui-control | get journeys.manifest
        configuration-root | path join $configured
    } else if ($requested | path type) == "absolute" {
        $requested | path expand
    } else {
        owned-path $requested
    }
    if not ($value | path exists) {
        tooling-fail "missing-gui-manifest" $"GUI journey manifest does not exist: ($value)"
    }
    $value
}

def gui-run-id [lane: string]: nothing -> string {
    let stamp = date now | format date "%Y%m%dT%H%M%S%.fZ"
    $"($lane)-($stamp)-(random uuid)"
}

def gui-slot [id: string, count: int]: nothing -> int {
    if $count < 1 { tooling-fail "invalid-gui-shards" "shard count must be at least one" }
    let digest = $id | hash sha256 | str substring 0..7 | into int --radix 16
    $digest mod $count
}

def gui-journeys [manifest: string]: nothing -> list<record> {
    let document = open $manifest
    let config = gui-control
    if ($document.schema? | default 0) != 1 {
        tooling-fail "invalid-gui-manifest" "GUI journey manifest schema must be 1"
    }
    let journeys = $document.journeys? | default []
    if ($journeys | is-empty) { tooling-fail "invalid-gui-manifest" "GUI journey manifest contains no journeys" }
    let ids = $journeys | each {|journey| $journey.id? | default "" }
    if ($ids | length) != ($ids | uniq | length) {
        tooling-fail "invalid-gui-manifest" "GUI journey identifiers must be unique"
    }
    for journey in $journeys {
        for field in ["id" "route" "tags" "modes" "actions"] {
            if $field not-in ($journey | columns) { tooling-fail "invalid-gui-manifest" $"journey ($journey.id?) lacks ($field)" }
        }
        if ($journey.id | str trim | is-empty) or ($journey.id !~ '^[a-z][a-z0-9-]{1,63}$') {
            tooling-fail "invalid-gui-manifest" $"invalid journey id: ($journey.id)"
        }
        if ($journey.actions | is-empty) { tooling-fail "invalid-gui-manifest" $"journey ($journey.id) has no actions" }
    }
    let requiredTags = $config.journeys.requiredTags
    let observedTags = $journeys | get tags | flatten | uniq
    let missingTags = $requiredTags | where {|tag| $tag not-in $observedTags }
    if not ($missingTags | is-empty) {
        tooling-fail "incomplete-gui-manifest" $"journey manifest is missing required state tags: ($missingTags | str join ', ')"
    }
    let requiredFlows = $config.journeys.requiredFlows
    let missingFlows = $requiredFlows | where {|flow| $flow not-in $ids }
    if not ($missingFlows | is-empty) {
        tooling-fail "incomplete-gui-manifest" $"journey manifest is missing required flows: ($missingFlows | str join ', ')"
    }
    $journeys
}

def gui-driver [requested: string]: nothing -> string {
    let candidate = if ($requested | is-empty) {
        $env.NUDOX_GUI_DRIVER? | default ""
    } else { $requested }
    if ($candidate | is-empty) {
        tooling-fail "missing-gui-driver" "NUDOX_GUI_DRIVER must name the real GPUI journey driver" "build or enter the GUI driver through the same nix shell closure"
    }
    let path = if (($candidate | path type) == "absolute") or ($candidate | path exists) {
        $candidate | path expand
    } else {
        let found = which $candidate
        if ($found | is-empty) { tooling-fail "missing-gui-driver" $"GUI driver is not on the pinned PATH: ($candidate)" }
        $found | first | get path
    }
    if not ($path | path exists) { tooling-fail "missing-gui-driver" $"GUI driver does not exist: ($path)" }
    let executable = process-result "test" ["-x" $path]
    if $executable.status != 0 { tooling-fail "invalid-gui-driver" $"GUI driver is not executable: ($path)" }
    $path
}

def gui-service-endpoint []: nothing -> string {
    let endpoint = $env.NUDOX_GUI_LOCALD_ENDPOINT? | default ""
    if ($endpoint | is-empty) {
        tooling-fail "missing-live-index" "NUDOX_GUI_LOCALD_ENDPOINT must point at the real locald framed endpoint" "start locald through the pinned service command before running journeys"
    }
    let path = if ($endpoint | str starts-with "unix:") {
        $endpoint | str substring 5..
    } else { $endpoint }
    if not ($path | path exists) {
        tooling-fail "missing-live-index" $"locald endpoint does not exist: ($path)"
    }
    $endpoint
}

def gui-service-readiness [endpoint: string]: nothing -> record {
    let command = $env.NUDOX_GUI_READINESS_COMMAND? | default ""
    if ($command | is-empty) {
        tooling-fail "missing-live-readiness" "NUDOX_GUI_READINESS_COMMAND must probe the real locald/index protocol before a journey"
    }
    let executable = if (($command | path type) == "absolute") or ($command | path exists) {
        $command | path expand
    } else {
        let found = which $command
        if ($found | is-empty) { tooling-fail "missing-live-readiness" $"readiness command is not on the pinned PATH: ($command)" }
        $found | first | get path
    }
    process-require $executable ["--endpoint" $endpoint]
}

def gui-detected-gpu []: nothing -> record {
    let probe = $env.NUDOX_GUI_GPU_PROBE? | default ""
    if ($probe | is-empty) { tooling-fail "missing-gui-gpu-probe" "the pinned GPU probe is not available in the GUI closure" }
    let result = process-require $probe []
    let report = try {
        $result.stdout | from json
    } catch { tooling-fail "invalid-gui-gpu-probe" "the pinned GPU probe did not return JSON" }
    for field in ["backend" "device"] {
        if $field not-in ($report | columns) or (($report | get $field | into string | str trim) | is-empty) {
            tooling-fail "incomplete-gui-gpu-probe" $"the pinned GPU probe lacks ($field)"
        }
    }
    $report
}

def gui-runtime [
    lane: string
    run: string
    artifacts: string
    mode: string
]: nothing -> record {
    let config = gui-control
    let viewport = $config.viewport
    let display = $config.display
    let backend = $env.NUDOX_GUI_DISPLAY_BACKEND? | default $display.default
    if $backend not-in $display.backends { tooling-fail "invalid-gui-display" $"unsupported GUI display backend: ($backend)" }
    let laneRoot = gui-lane-root $lane
    let cache = $laneRoot | path join "cache"
    let configRoot = $laneRoot | path join "config"
    let data = $laneRoot | path join "data"
    mkdir $cache $configRoot $data $artifacts
    let x11 = $display.x11
    let wayland = $display.wayland
    {
        BACKEND_CONFIG_MODE: "immutable"
        NUDOX_GUI_NIX_SHELL: "1"
        NUDOX_GUI_RUN: $run
        NUDOX_GUI_LANE: $lane
        NUDOX_GUI_MODE: $mode
        NUDOX_GUI_ARTIFACTS: $artifacts
        NUDOX_GUI_VIEWPORT_WIDTH: ($viewport.defaultWidth | into string)
        NUDOX_GUI_VIEWPORT_HEIGHT: ($viewport.defaultHeight | into string)
        NUDOX_GUI_SCALE: ($viewport.defaultScale | into string)
        NUDOX_GUI_COLOR_DEPTH: ($viewport.colorDepth | into string)
        NUDOX_GUI_COLOR_PROFILE: $viewport.colorProfile
        NUDOX_GUI_ANIMATION_CLOCK: $config.animation.clock
        NUDOX_GUI_ANIMATION_FPS: ($config.animation.fps | into string)
        NUDOX_GUI_ANIMATION_SETTLE_MS: ($config.animation.settleMs | into string)
        NUDOX_GUI_VIRTUAL_TIME: "1"
        NUDOX_GUI_REDUCED_MOTION: "0"
        NUDOX_GUI_GPUI_SOURCE_DIGEST: ($env.NUDOX_GUI_GPUI_SOURCE_DIGEST? | default "")
        NUDOX_GUI_GPUI_COMPONENT_SOURCE_DIGEST: ($env.NUDOX_GUI_GPUI_COMPONENT_SOURCE_DIGEST? | default "")
        NUDOX_GUI_GPUI_SOURCE_MANIFEST: ($env.NUDOX_GUI_GPUI_SOURCE_MANIFEST? | default "")
        NUDOX_GUI_DEPENDENCY_GRAPH_SHA256: ($env.NUDOX_GUI_DEPENDENCY_GRAPH_SHA256? | default "")
        NUDOX_GUI_GPU_BACKEND: ($env.NUDOX_GUI_GPU_BACKEND? | default $config.gpu.defaultGpuBackend)
        NUDOX_GUI_GPU_DEVICE: ($env.NUDOX_GUI_GPU_DEVICE? | default $config.gpu.expectedGpuDevice)
        NUDOX_GUI_GPU_PROBE: ($env.NUDOX_GUI_GPU_PROBE? | default "")
        WGPU_BACKEND: ($env.WGPU_BACKEND? | default $config.gpu.forceEnvironment.WGPU_BACKEND)
        LIBGL_ALWAYS_SOFTWARE: (
            $env.LIBGL_ALWAYS_SOFTWARE?
            | default $config.gpu.forceEnvironment.LIBGL_ALWAYS_SOFTWARE
        )
        MESA_LOADER_DRIVER_OVERRIDE: (
            $env.MESA_LOADER_DRIVER_OVERRIDE?
            | default $config.gpu.forceEnvironment.MESA_LOADER_DRIVER_OVERRIDE
        )
        NUDOX_GUI_TOOLCHAIN: ($env.NUDOX_GUI_TOOLCHAIN? | default "")
        NUDOX_GUI_ENCODER_VERSION: ($env.NUDOX_GUI_ENCODER_VERSION? | default "")
        NUDOX_GUI_DISPLAY_BACKEND: $backend
        DISPLAY: $x11.display
        WAYLAND_DISPLAY: $wayland.socket
        XDG_SESSION_TYPE: (if $backend == "wayland" { "wayland" } else { "x11" })
        XDG_CACHE_HOME: $cache
        XDG_CONFIG_HOME: $configRoot
        XDG_DATA_HOME: $data
        LANG: $config.fonts.locale
        LC_ALL: $config.fonts.locale
        TZ: "UTC"
        RUST_BACKTRACE: "1"
        CARGO_TARGET_DIR: ($laneRoot | path join "target")
        NUDOX_GUI_WORKSPACE: ($laneRoot | path join "live-workspace")
        NUDOX_GUI_LOCALD_ENDPOINT: (gui-service-endpoint)
        NUDOX_GUI_CAPTURE_DIR: $artifacts
        FONTCONFIG_FILE: ($env.BACKEND_GUI_FONTCONFIG? | default "")
        NUDOX_GUI_FONT_MANIFEST: ($env.BACKEND_GUI_FONT_MANIFEST? | default "")
        NUDOX_GUI_FONT_FAMILIES: ($config.fonts.families | str join ",")
    }
}

def gui-lock [
    resource: string
    run: string
    lane: string
    shard: int
]: nothing -> string {
    let config = gui-control
    if $resource not-in $config.locks.resources { tooling-fail "invalid-gui-lock" $"resource is not declared by the GUI control plane: ($resource)" }
    let root = gui-lock-root
    let path = $root | path join $resource
    if ($path | path exists) {
        let ownerPath = $path | path join "owner.json"
        let owner = if ($ownerPath | path exists) { open $ownerPath } else { {resource: $resource, owner: "unknown"} }
        tooling-fail "gui-resource-busy" $"GUI resource lock is held: ($resource)" ($owner | to json)
    }
    try {
        mkdir $path
    } catch {
        # mkdir is the atomic lock acquisition. A concurrent creator wins and
        # this process must never proceed on a check-then-create race.
        tooling-fail "gui-resource-busy" $"GUI resource lock was acquired concurrently: ($resource)"
    }
    {
        schema: 1
        resource: $resource
        run: $run
        lane: $lane
        shard: $shard
        pid: $nu.pid
        startedAt: (date now)
        revision: ((process-result "git" ["rev-parse" "HEAD"]).stdout | str trim)
    } | to json --indent 2 | save --raw ($path | path join "owner.json")
    ^chmod "0600" ($path | path join "owner.json")
    $path
}

def gui-unlock [resource: string, run: string]: nothing -> nothing {
    let path = gui-lock-root | path join $resource
    if not ($path | path exists) { return }
    let ownerPath = $path | path join "owner.json"
    if ($ownerPath | path exists) {
        let owner = open $ownerPath
        if ($owner.run? | default "") != $run {
            tooling-fail "gui-lock-owner" $"refusing to remove lock owned by another run: ($resource)"
        }
    }
    rm --recursive $path
}

def gui-with-lock [
    resource: string
    run: string
    lane: string
    shard: int
    action: closure
]: nothing -> any {
    gui-lock $resource $run $lane $shard | ignore
    try { do $action } catch {|error| error make $error } finally { gui-unlock $resource $run }
}

def gui-capture-one [path: string]: nothing -> record {
    let custom = $env.NUDOX_GUI_CAPTURE_COMMAND? | default ""
    let backend = $env.NUDOX_GUI_DISPLAY_BACKEND? | default "x11"
    if not ($custom | is-empty) {
        process-require $custom [$path] | ignore
    } else if $backend == "wayland" {
        process-require "grim" [$path] | ignore
    } else if $backend == "quartz" {
        tooling-fail "quartz-capture-driver" "quartz capture must be supplied by the real GPUI driver"
    } else {
        process-require "import" ["-window" "root" $path] | ignore
    }
    if not ($path | path exists) { tooling-fail "missing-gui-frame" $"capture command produced no frame: ($path)" }
    process-require "pngcheck" ["-q" $path] | ignore
    let size = ls $path | get size | first | into int
    if $size < 8 { tooling-fail "empty-gui-frame" $"capture command produced an empty frame: ($path)" }
    {
        path: $path
        bytes: $size
        sha256: (open --raw $path | hash sha256)
    }
}

def gui-artifact-manifest [
    run: string
    lane: string
    mode: string
    manifest: string
    frames: list<record>
    root: string
    fps: int
]: nothing -> record {
    {
        schema: 1
        protocol: "nudox-gui-artifact-v1"
        run: $run
        lane: $lane
        mode: $mode
        revision: ((process-result "git" ["rev-parse" "HEAD"]).stdout | str trim)
        scenarioSha256: (open --raw $manifest | hash sha256)
        controlSha256: (open --raw ($env.BACKEND_GUI_CONFIG) | hash sha256)
        viewport: {
            width: ($env.NUDOX_GUI_VIEWPORT_WIDTH | into int)
            height: ($env.NUDOX_GUI_VIEWPORT_HEIGHT | into int)
            scale: ($env.NUDOX_GUI_SCALE | into int)
        }
        animation: {
            fps: $fps
            frameCount: ($frames | length)
            virtualClock: true
        }
        fonts: {
            bundledOnly: true
            fileManifestSha256: (open --raw $env.NUDOX_GUI_FONT_MANIFEST | hash sha256)
            fileManifest: $env.NUDOX_GUI_FONT_MANIFEST
            locale: $env.LC_ALL
            families: ($env.NUDOX_GUI_FONT_FAMILIES | split row ",")
        }
        gpu: {
            detected: (gui-detected-gpu)
            framework: "gpui-ce"
            componentFramework: "gpui-ce-component"
            sourceDigest: $env.NUDOX_GUI_GPUI_SOURCE_DIGEST
            componentSourceDigest: $env.NUDOX_GUI_GPUI_COMPONENT_SOURCE_DIGEST
            dependencyGraphSha256: $env.NUDOX_GUI_DEPENDENCY_GRAPH_SHA256
            runtimeProvenance: ($env.NUDOX_GUI_GPUI_RUNTIME_PROVENANCE? | default "")
            sourceManifestSha256: (open --raw $env.NUDOX_GUI_GPUI_SOURCE_MANIFEST | hash sha256)
            expectedBackend: $env.NUDOX_GUI_GPU_BACKEND
            expectedDevice: $env.NUDOX_GUI_GPU_DEVICE
            toolchain: $env.NUDOX_GUI_TOOLCHAIN
            encoderVersion: $env.NUDOX_GUI_ENCODER_VERSION
        }
        frameTrace: {
            required: true
            detects: ["allocation" "object" "timer"]
        }
        redaction: {rawTranscripts: false, privateMetadata: "redacted"}
        frames: $frames
        root: $root
    }
}

# Checks the complete manifest and emits the required journey/loop contract.
# @class inspection
def "main gui validate" []: nothing -> record {
    require-command "gui-validate"
    let manifest = gui-manifest ""
    let journeys = gui-journeys $manifest
    let config = gui-control
    {
        schema: $config.schema
        manifest: $manifest
        journeys: ($journeys | length)
        requiredTags: $config.journeys.requiredTags
        requiredFlows: $config.journeys.requiredFlows
        acceptanceLoops: ($config.acceptance.loops | get name)
        failureInjection: ($config.acceptance.failureInjection | length)
        viewports: ($config.viewport.required | length)
        scales: $config.viewport.scales
        animationPhases: $config.animation.requiredPhases
        shell: $config.shell.command
        laneLocalTarget: true
    }
}

# Starts, stops, or probes the declared real locald/index service command.
# @class verification
def "main gui service" [--action: string = "status", --endpoint: string = ""]: nothing -> record {
    require-command "gui-service"
    if $action not-in ["start" "stop" "status"] { tooling-fail "invalid-gui-service-action" "service action must be start, stop, or status" }
    let endpoint = if ($endpoint | is-empty) {
        $env.NUDOX_GUI_LOCALD_ENDPOINT? | default ""
    } else { $endpoint }
    if ($endpoint | is-empty) { tooling-fail "missing-live-index" "a real locald endpoint is required" }
    let command = $env.NUDOX_GUI_SERVICE_COMMAND? | default ""
    if ($command | is-empty) { tooling-fail "missing-gui-service-command" "NUDOX_GUI_SERVICE_COMMAND must name the real locald/index supervisor" }
    let executable = if (($command | path type) == "absolute") or ($command | path exists) {
        $command | path expand
    } else {
        let found = which $command
        if ($found | is-empty) { tooling-fail "missing-gui-service-command" $"service command is not on the pinned PATH: ($command)" }
        $found | first | get path
    }
    let result = process-require $executable [$action "--endpoint" $endpoint]
    {
        action: $action
        endpoint: $endpoint
        command: $executable
        stdout: $result.stdout
        status: $result.status
    }
}

# Verifies one GPUI/component source universe and the pinned GPU/toolchain closure.
# @class verification
def "main gui provenance" [--driver: string = ""]: nothing -> record {
    require-command "gui-provenance"
    let config = gui-control
    let driverPath = gui-driver $driver
    let result = process-require $driverPath ["provenance" "--protocol" "nudox-gui-driver-v1"]
    let report = try {
        $result.stdout | from json
    } catch { tooling-fail "invalid-gui-provenance" "the GPUI driver did not return JSON provenance" }
    let detected = gui-detected-gpu
    let required = $config.provenance.requiredFields
    for field in $required {
        if $field not-in ($report | columns) or (($report | get $field | into string | str trim) | is-empty) {
            tooling-fail "incomplete-gui-provenance" $"driver provenance lacks ($field)"
        }
    }
    if $detected.backend != ($env.NUDOX_GUI_GPU_BACKEND? | default $config.gpu.defaultGpuBackend) {
        tooling-fail "gui-gpu-drift" "the independently detected GPU backend differs from the pinned control plane"
    }
    if $report.detectedGpuBackend != $detected.backend {
        tooling-fail "gui-gpu-report-drift" "driver GPU backend differs from the independent GPU probe"
    }
    let reportedDevice = $report.gpuDevice | into string | str downcase
    let detectedDevice = $detected.device | into string | str downcase
    if not ($reportedDevice | str contains $detectedDevice) {
        tooling-fail "gui-gpu-device-drift" "driver GPU device differs from the independent GPU probe"
    }
    for pair in [
        {
            field: "gpuiSourceDigest"
            expected: ($env.NUDOX_GUI_GPUI_SOURCE_DIGEST? | default "")
        }
        {
            field: "gpuiComponentSourceDigest"
            expected: ($env.NUDOX_GUI_GPUI_COMPONENT_SOURCE_DIGEST? | default "")
        }
        {
            field: "dependencyGraphSha256"
            expected: ($env.NUDOX_GUI_DEPENDENCY_GRAPH_SHA256? | default "")
        }
    ] {
        let observed = $report | get $pair.field | into string
        if ($pair.expected | is-empty) or $observed != $pair.expected {
            tooling-fail "gui-source-drift" $"driver ($pair.field) differs from the Nix lock/source closure"
        }
    }
    let runtimeProvenance = $env.NUDOX_GUI_GPUI_RUNTIME_PROVENANCE? | default ""
    if not ($runtimeProvenance | is-empty) {
        if not ($runtimeProvenance | path exists) { tooling-fail "missing-gui-runtime-provenance" "the realized GUI runtime did not emit cargo metadata/tree evidence" }
        let graphLines = open $runtimeProvenance | lines | where {|line| $line | str starts-with "dependency-graph-runtime-sha256=" }
        if ($graphLines | length) != 1 { tooling-fail "invalid-gui-runtime-provenance" "runtime provenance lacks exactly one dependency graph digest" }
        let runtimeGraph = $graphLines | first | str replace "dependency-graph-runtime-sha256=" ""
        if $report.dependencyGraphSha256 != $runtimeGraph { tooling-fail "gui-runtime-graph-drift" "driver dependency graph differs from cargo metadata/tree evidence" }
    }
    let expectedDevice = $env.NUDOX_GUI_GPU_DEVICE? | default $config.gpu.expectedGpuDevice
    let normalizedExpectedDevice = $expectedDevice | str downcase
    if not ($detectedDevice | str contains $normalizedExpectedDevice) {
        tooling-fail "gui-gpu-device-policy-drift" "the independently detected GPU device differs from the pinned control plane"
    }
    {
        schema: 1
        protocol: "nudox-gui-provenance-v1"
        report: $report
        stdoutSha256: ($result.stdout | hash sha256)
        stderrSha256: ($result.stderr | hash sha256)
    }
}

# Prints the deterministic route/state/viewport/shard matrix without invoking a driver.
# @class inspection
def "main gui matrix" [
    --manifest: string = ""
    --shard-index: int = 0
    --shard-count: int = 1
    --lane: string = ""
]: nothing -> table {
    require-command "gui-matrix"
    if $shard_index < 0 or $shard_index >= $shard_count { tooling-fail "invalid-gui-shard" "shard index must be within shard count" }
    let lane = gui-lane $lane
    let journeys = gui-journeys (gui-manifest $manifest)
    let config = gui-control
    $journeys
    | where {|journey| (gui-slot $journey.id $shard_count) == $shard_index }
    | each {|journey|
        $config.viewport.required
        | each {|viewport|
            $config.viewport.scales
            | each {|scale|
                $journey.modes
                | each {|mode|
                    {
                        lane: $lane
                        shard: $shard_index
                        journey: $journey.id
                        route: $journey.route
                        mode: $mode
                        width: $viewport.width
                        height: $viewport.height
                        scale: $scale
                        tags: $journey.tags
                    }
                }
            }
        }
    }
    | flatten
}

# Captures one or more real compositor frames with fixed virtual time and emits hashed evidence.
# @class verification
def "main gui capture" [
    --lane: string = ""
    --mode: string = "warm"
    --frames: int = 1
    --fps: int = 0
    --reduced-motion
    --video
]: nothing -> record {
    require-command "gui-capture"
    let config = gui-control
    if $frames < 1 or $frames > $config.animation.maxFrames { tooling-fail "invalid-gui-frames" $"frames must be between 1 and ($config.animation.maxFrames)" }
    if $fps < 0 or $fps > 240 { tooling-fail "invalid-gui-fps" "fps must be between 0 and 240" }
    let lane = gui-lane $lane
    let run = gui-run-id $lane
    let root = gui-root | path join $lane $run
    let frameRoot = $root | path join "frames"
    mkdir $frameRoot
    let actualFps = if $fps == 0 { $config.animation.fps } else { $fps }
    let environment = gui-runtime $lane $run $root $mode | upsert NUDOX_GUI_REDUCED_MOTION (if $reduced_motion { "1" } else { "0" })
    let framesList = with-env $environment {
        0..($frames - 1)
        | each {|index|
            if $index > 0 {
                let delay = 1000 / $actualFps | into int
                sleep ($delay * 1ms)
            }
            $env.NUDOX_GUI_VIRTUAL_NOW_MS = $index * (1000 / $actualFps) | into int | into string
            let path = $frameRoot | path join $"frame-($index | fill -a right -c '0' -w 6).png"
            gui-capture-one $path | merge {index: $index, timestampMs: ($env.NUDOX_GUI_VIRTUAL_NOW_MS | into int)}
        }
    }
    let artifact = with-env $environment { gui-artifact-manifest $run $lane $mode (gui-manifest "") $framesList $root $actualFps }
    let artifactPath = $root | path join $config.artifacts.manifest
    $artifact | to json --indent 2 | save --raw $artifactPath
    if $video and $frames > 1 {
        process-require "ffmpeg" [
            "-hide_banner"
            "-loglevel" "error"
            "-y"
            "-framerate"
            ($actualFps | into string)
            "-i"
            ($frameRoot | path join "frame-%06d.png")
            "-c:v" "libvpx-vp9"
            "-pix_fmt" "yuv420p"
            ($root | path join "animation.webm")
        ] | ignore
    }
    $artifact | merge {manifest: $artifactPath}
}

# Executes every selected live-window journey and keeps an artifact per page, state, and mode.
# @class expanded-verification
def "main gui journey" [
    --manifest: string = ""
    --driver: string = ""
    --lane: string = ""
    --mode: string = ""
    --shard-index: int = 0
    --shard-count: int = 1
    --reduced-motion
    --dry-run
]: nothing -> record {
    require-command "gui-journey"
    let lane = gui-lane $lane
    let config = gui-control
    if $shard_count < 1 or $shard_count > ($config.sharding.maxCount | into int) { tooling-fail "invalid-gui-shards" "shard count exceeds the GUI control-plane bound" }
    if $shard_index < 0 or $shard_index >= $shard_count { tooling-fail "invalid-gui-shard" "shard index must be within shard count" }
    let manifestPath = gui-manifest $manifest
    let journeys = gui-journeys $manifestPath | where {|journey| (gui-slot $journey.id $shard_count) == $shard_index }
    let selected = $journeys | each {|journey|
        let modes = if ($mode | is-empty) { $journey.modes } else { [$mode] }
        $modes | each {|selectedMode| {id: $journey.id, route: $journey.route, mode: $selectedMode, tags: $journey.tags} }
    } | flatten
    if $dry_run {
        return {
            lane: $lane
            shard: $shard_index
            shardCount: $shard_count
            selected: $selected
            driver: $driver
        }
    }
    let driverPath = gui-driver $driver
    let endpoint = gui-service-endpoint
    gui-service-readiness $endpoint | ignore
    let run = gui-run-id $lane
    let backend = $env.NUDOX_GUI_DISPLAY_BACKEND? | default $config.display.default
    let resource = $"display-($backend)"
    let outputRoot = gui-root | path join $lane $run
    mkdir $outputRoot
    let environment = gui-runtime $lane $run $outputRoot $mode | upsert NUDOX_GUI_REDUCED_MOTION (if $reduced_motion { "1" } else { "0" })
    let outcomes = gui-with-lock $resource $run $lane $shard_index {
        $selected | each {|selection|
            let artifact = $outputRoot | path join $selection.id $selection.mode
            mkdir $artifact
            let args = [
                "journey"
                "--protocol" "nudox-gui-driver-v1"
                "--manifest" $manifestPath
                "--journey" $selection.id
                "--mode" $selection.mode
                "--locald-endpoint" $endpoint
                "--viewport" $"($environment.NUDOX_GUI_VIEWPORT_WIDTH)x($environment.NUDOX_GUI_VIEWPORT_HEIGHT)"
                "--scale" $environment.NUDOX_GUI_SCALE
                "--capture-dir" $artifact
                "--report" ($artifact | path join "driver-report.json")
            ]
            let result = with-env ($environment | upsert NUDOX_GUI_CAPTURE_DIR $artifact) { process-result $driverPath $args }
            {
                journey: $selection.id
                mode: $selection.mode
                status: $result.status
                stdoutSha256: ($result.stdout | hash sha256)
                stderrSha256: ($result.stderr | hash sha256)
                stdoutBytes: $result.stdout_bytes
                stderrBytes: $result.stderr_bytes
                artifact: $artifact
            }
        }
    }
    let failures = $outcomes | where status != 0
    {
        schema: 1
        protocol: "nudox-gui-run-v1"
        run: $run
        lane: $lane
        shard: $shard_index
        shardCount: $shard_count
        driver: $driverPath
        outcomes: $outcomes
        passed: (($outcomes | length) - ($failures | length))
        failed: ($failures | length)
    }
}

# Runs the independent property, randomized, differential, and metamorphic driver loops.
# @class expanded-verification
def "main gui acceptance" [
    --driver: string = ""
    --lane: string = ""
    --loop: string = ""
    --cases: int = 0
    --dry-run
]: nothing -> record {
    require-command "gui-acceptance"
    let config = gui-control
    let lane = gui-lane $lane
    let loops = if ($loop | is-empty) { $config.acceptance.loops } else {
        let names = $loop | split row ","
        $config.acceptance.loops | where {|item| $item.name in $names }
    }
    if ($loops | is-empty) { tooling-fail "invalid-gui-loop" "no requested acceptance loop is declared" }
    let selected = $loops | each {|item| $item | upsert cases (if $cases == 0 { $item.minCases } else { $cases }) }
    if $dry_run { return {lane: $lane, loops: $selected, driver: $driver} }
    let driverPath = gui-driver $driver
    let holdout = $env.NUDOX_GUI_HOLDOUT_MANIFEST? | default ""
    let verifier = $env.NUDOX_GUI_HOLDOUT_VERIFIER? | default ""
    if ($holdout | is-empty) or not ($holdout | path exists) {
        tooling-fail "missing-gui-holdout" "acceptance requires an independently maintained hidden holdout manifest"
    }
    if ($verifier | is-empty) { tooling-fail "missing-gui-verifier" "acceptance requires an independent holdout verifier" }
    let verifierPath = gui-driver $verifier
    let run = gui-run-id $lane
    let endpoint = gui-service-endpoint
    gui-service-readiness $endpoint | ignore
    let root = gui-root | path join $lane $run "acceptance"
    mkdir $root
    let environment = gui-runtime $lane $run $root "acceptance"
    let execution = gui-with-lock "live-server-index" $run $lane 0 {
        let outcomes = with-env $environment {
            $selected | each {|item|
                let args = ["acceptance" "--protocol" "nudox-gui-driver-v1" "--loop" $item.name "--cases" ($item.cases | into string) "--holdout-manifest" $holdout "--output" ($root | path join $"($item.name).json")]
                let result = process-result $driverPath $args
                {
                    loop: $item.name
                    cases: $item.cases
                    status: $result.status
                    stdoutSha256: ($result.stdout | hash sha256)
                    stderrSha256: ($result.stderr | hash sha256)
                }
            }
        }
        let verifierResult = process-result $verifierPath [
            "verify"
            "--protocol" "nudox-gui-driver-v1"
            "--holdout-manifest" $holdout
            "--artifacts" $root
        ]
        {outcomes: $outcomes, verifier: $verifierResult}
    }
    let outcomes = $execution.outcomes
    let verifierResult = $execution.verifier
    let allOutcomes = $outcomes | append {
        loop: "independent-holdout-verifier",
        cases: 1
        status: $verifierResult.status
        stdoutSha256: ($verifierResult.stdout | hash sha256)
        stderrSha256: ($verifierResult.stderr | hash sha256)
    }
    {
        schema: 1
        protocol: "nudox-gui-acceptance-v1"
        run: $run
        lane: $lane
        outcomes: $allOutcomes
        passed: ($allOutcomes | where status == 0 | length)
        failed: ($allOutcomes | where status != 0 | length)
    }
}

# Lists reclaimable GUI artifacts and performs deletion only after explicit confirmation.
# @class closure
def "main gui clean" [--lane: string = "", --older-than-hours: int = 168, --apply]: nothing -> record {
    require-command "gui-clean"
    if $older_than_hours < 1 { tooling-fail "invalid-cleanup-age" "cleanup age must be at least one hour" }
    let root = gui-root
    let lane = if ($lane | is-empty) { "" } else { gui-lane $lane }
    let target = if ($lane | is-empty) { $root } else {
        $root | path join $lane
    }
    let entries = if ($target | path exists) {
        ls $target | where type == "dir"
    } else { [] }
    let cutoff = (date now) - ($older_than_hours * 1hr)
    let candidates = $entries | where modified < $cutoff | get name
    if $apply {
        for path in $candidates {
            let resolved = $path | path expand
            if not ($resolved | str starts-with $"($root)/") { tooling-fail "cleanup-path-escape" $"refusing to remove outside GUI artifact root: ($resolved)" }
            rm --recursive $resolved
        }
    }
    {
        root: $root
        lane: $lane
        olderThanHours: $older_than_hours
        dryRun: (not $apply)
        candidates: $candidates
        removed: (if $apply { $candidates } else { [] })
    }
}
