"""Distribution contracts for the architectures exposed by the website."""

COMMON_QA = {
    "project_index_search", "bundled_helpers", "preferences", "cold_restart",
    "clean_environment", "minimum_os", "browser_download_launch", "cli_mcp",
}
PLATFORMS = {
    "macos": {
        "target": "aarch64-apple-darwin", "asset": "nudox-macos-arm64.zip",
        "host": "Darwin", "qa_host": "macos_version", "signed": True,
        "notarized": True, "qa": COMMON_QA | {"finder_launch", "gatekeeper", "homebrew_install"},
        "ci": ["concourse/backend-fast"],
        "root": "Nudox.app", "build": "Nudox.app/Contents/Resources/build-manifest.json",
        "executables": [f"Nudox.app/Contents/MacOS/{name}" for name in
                        ("Nudox", "nudox-cli", "nudox-mcp", "nudox-locald", "backend-desktop", "backend-cli", "backend-mcp", "backend-locald")],
    },
    "linux-x64": {
        "target": "x86_64-unknown-linux-gnu", "asset": "nudox-linux-x64.tar.gz",
        "host": "Linux", "qa_host": "os_version", "signed": False,
        "notarized": False, "qa": COMMON_QA | {"desktop_launch", "install_uninstall"},
    },
    "linux-arm64": {
        "target": "aarch64-unknown-linux-gnu", "asset": "nudox-linux-arm64.tar.gz",
        "host": "Linux", "qa_host": "os_version", "signed": False,
        "notarized": False, "qa": COMMON_QA | {"desktop_launch", "install_uninstall"},
    },
    "windows": {
        "target": "x86_64-pc-windows-gnu", "asset": "nudox-windows-x64.zip",
        "host": "Windows", "qa_host": "os_version", "signed": True,
        "notarized": False, "qa": COMMON_QA | {"desktop_launch", "install_uninstall", "authenticode"},
    },
}
for key, profile in PLATFORMS.items():
    if key != "macos":
        lane = {"linux-x64": "linux", "linux-arm64": "arm64-emu", "windows": "windows-wine"}[key]
        profile["ci"] = ["concourse/backend-fast", "concourse/backend-" + lane]
        profile["root"] = "Nudox"
        profile["build"] = "Nudox/resources/build-manifest.json"
        suffix = ".exe" if key == "windows" else ""
        profile["executables"] = [f"Nudox/bin/{name}{suffix}" for name in
                                  ("backend-desktop", "backend-cli", "backend-mcp", "backend-locald")]

for profile in PLATFORMS.values():
    profile["targets"] = {profile["target"]}
PLATFORMS["windows"]["targets"].add("x86_64-pc-windows-msvc")
