package release

import "cue.dev/x/goreleaser"

goreleaser.#Project & {
	version:      2
	project_name: "nudox"
	builds: [{
		id:      "nudox"
		builder: "rust"
		binary:  "nudox"
	}]
	archives: [{
		formats: ["tar.gz"]
		name_template: "{{ .ProjectName }}_{{ .Os }}_{{ .Arch }}"
		files: ["README.md"]
	}]
	checksum: {
		name_template: "SHA256.sum"
		algorithm:     "sha256" // TODO: PR support for Blake3 and migrate
	}
	snapshot: name_template: "{{ .Tag }}-next"
	release: disable:        true
}
