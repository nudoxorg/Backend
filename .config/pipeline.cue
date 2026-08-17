package release

import "cue.dev/x/goreleaser"

goreleaser.#Project & {
	version:      2
	project_name: "nudox"
	builds: [{
		id:      "nudox"
		builder: "rust"
		binary:  "nudox"
	}, {
		// The GUI (lindsey) is a standalone Cargo package with its own lockfile
		// (workspace/gui/Cargo.toml), so it is built from that directory, not
		// from the monorepo root. The four targets are the supported packaging
		// matrix: macOS (arm64 + x64), Linux x64 (deb/rpm), and Windows x64
		// (MSI — see packaging/windows/; goreleaser only produces the zip/tar
		// archive here, the MSI is cargo-wix). gpui/gpui-ce links platform
		// system libraries (macOS SDK, X11/Wayland, MSVC/WinSDK), so each OS
		// is built *natively* — run goreleaser once per OS (as release.nu
		// already does per single target) rather than cross-compiling.
		id:      "lindsey"
		builder: "rust"
		binary:  "lindsey"
		dir:     "workspace/gui"
		flags: ["--release", "--locked"]
		targets: [
			"aarch64-apple-darwin",
			"x86_64-apple-darwin",
			"x86_64-unknown-linux-gnu",
			"x86_64-pc-windows-msvc",
		]
	}]
	archives: [{
		// Pin this archive to the nudox build now that a second build exists;
		// without `builds` both binaries would collide on the same
		// {{ .ProjectName }}_{{ .Os }}_{{ .Arch }} name_template.
		builds: ["nudox"]
		formats: ["tar.gz"]
		name_template: "{{ .ProjectName }}_{{ .Os }}_{{ .Arch }}"
		files: ["README.md"]
	}, {
		builds: ["lindsey"]
		name_template: "lindsey_{{ .Os }}_{{ .Arch }}"
		files: ["README.md"]
	}]
	// Linux packaging: the deb/rpm pair mirrors the cargo-deb/cargo-generate-rpm
	// metadata in workspace/gui/Cargo.toml (packaging/linux/). nfpm only emits
	// packages for the linux target of the named build.
	nfpms: [{
		id: "lindsey"
		builds: ["lindsey"]
		package_name:       "lindsey"
		file_name_template: "lindsey_{{ .Version }}_{{ .Arch }}"
		formats: ["deb", "rpm"]
		maintainer:  "the nudox project"
		license:     "MIT"
		section:     "devel"
		description: "Local-first documentation and code intelligence for seven languages."
		contents: [
			{src: "workspace/gui/packaging/linux/lindsey.desktop", dst: "/usr/share/applications/lindsey.desktop"},
			{src: "workspace/gui/assets/logo.svg", dst: "/usr/share/icons/hicolor/scalable/apps/lindsey.svg"},
		]
	}]
	checksum: {
		name_template: "SHA256.sum"
		algorithm:     "sha256"
	}
	snapshot: name_template: "{{ .Tag }}-next"
	release: disable:        true
}
