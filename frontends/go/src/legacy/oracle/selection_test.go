package main

import (
	"bytes"
	"crypto/sha256"
	"fmt"
	"go/build"
	"os"
	"path/filepath"
	"reflect"
	"sort"
	"strings"
	"testing"

	"golang.org/x/tools/go/packages"
)

func testSelection(t *testing.T, goos, goarch string, cgo bool, tags ...string) *packageSelection {
	t.Helper()
	context := build.Default
	context.GOOS, context.GOARCH, context.CgoEnabled = goos, goarch, cgo
	context.BuildTags = tags
	environment := []string{}
	// Keep the actual installed compiler's PATH/GOROOT, but isolate caches,
	// network, per-user Go configuration and ambient cross-compilation flags.
	for _, key := range []string{"PATH", "GOROOT", "SystemRoot"} {
		if value, ok := os.LookupEnv(key); ok {
			environment = append(environment, key+"="+value)
		}
	}
	cgoValue := "0"
	if cgo {
		cgoValue = "1"
	}
	environment = append(environment, "GOOS="+goos, "GOARCH="+goarch, "CGO_ENABLED="+cgoValue,
		"GOCACHE="+filepath.Join(t.TempDir(), "build"), "GOMODCACHE="+t.TempDir(),
		"GOENV=off", "GOTOOLCHAIN=local", "GOPROXY=off", "GOSUMDB=off", "GOPACKAGESDRIVER=off", "GOWORK=off", "GOMAXPROCS=2")
	selection, err := newPackageSelection(context, environment)
	if err != nil {
		t.Fatal(err)
	}
	return selection
}

func writeSelectionFixture(t *testing.T) string {
	t.Helper()
	dir := t.TempDir()
	files := map[string]string{
		"go.mod":                   "module example.com/selection\n\ngo 1.23\n",
		"common.go":                "package selection\n// Inferred is compiler-derived from the selected platform.\nvar Inferred = Platform()\nfunc Caller() int { return Platform() + Architecture() + Combined() + Feature() }\n",
		"listener_darwin.go":       "package selection\n// Platform is the Darwin implementation.\nfunc Platform() int { return 1 }\n",
		"listener_windows.go":      "package selection\n// Platform is the Windows implementation.\nfunc Platform() int { return 2 }\n",
		"arch_arm64.go":            "package selection\nfunc Architecture() int { return 64 }\n",
		"arch_amd64.go":            "package selection\nfunc Architecture() int { return 86 }\n",
		"combined_darwin_arm64.go": "package selection\nfunc Combined() int { return 1 }\n",
		"combined_other.go":        "//go:build !darwin || !arm64\n\npackage selection\nfunc Combined() int { return 2 }\n",
		"tag_feature.go":           "//go:build feature\n\npackage selection\nfunc Feature() int { return 1 }\n",
		"tag_default.go":           "//go:build !feature\n\npackage selection\nfunc Feature() int { return 2 }\n",
	}
	for name, source := range files {
		if err := os.WriteFile(filepath.Join(dir, name), []byte(source), 0600); err != nil {
			t.Fatal(err)
		}
	}
	return dir
}

func TestCompilerSelectionTargetsTagsAndInactiveImage(t *testing.T) {
	for _, test := range []struct {
		name, goos, goarch, platform, architecture, combined, feature string
		tags                                                          []string
	}{
		{"darwin-arm64", "darwin", "arm64", "listener_darwin.go", "arch_arm64.go", "combined_darwin_arm64.go", "tag_default.go", nil},
		{"darwin-amd64", "darwin", "amd64", "listener_darwin.go", "arch_amd64.go", "combined_other.go", "tag_default.go", nil},
		{"windows-amd64-feature", "windows", "amd64", "listener_windows.go", "arch_amd64.go", "combined_other.go", "tag_feature.go", []string{"feature"}},
		{"windows-arm64-feature", "windows", "arm64", "listener_windows.go", "arch_arm64.go", "combined_other.go", "tag_feature.go", []string{"feature"}},
	} {
		t.Run(test.name, func(t *testing.T) {
			dir := writeSelectionFixture(t)
			output, err := extractWithSelection(dir, ".", testSelection(t, test.goos, test.goarch, false, test.tags...))
			if err != nil {
				t.Fatal(err)
			}
			if len(output.Errors) != 0 || len(output.Packages) != 1 {
				t.Fatalf("actual package load: %#v", output)
			}
			pkg := output.Packages[0]
			var files []string
			for _, path := range pkg.Files {
				files = append(files, filepath.Base(path))
			}
			wantFiles := []string{test.architecture, test.combined, "common.go", test.platform, test.feature}
			// Compare compiler-returned operands, not an expected empty set.
			sort.Strings(files)
			sort.Strings(wantFiles)
			if !reflect.DeepEqual(files, wantFiles) {
				t.Fatalf("active files = %v, want %v", files, wantFiles)
			}
			var platform *Decl
			var caller *Reference
			for _, decl := range pkg.Decls {
				if decl.Name == "Platform" {
					platform = decl
				}
			}
			for _, reference := range pkg.References {
				if reference.Owner == "Caller" && reference.Target == "Platform" {
					caller = reference
				}
			}
			if platform == nil || platform.Pos == nil || filepath.Base(platform.Pos.File) != test.platform || !strings.Contains(platform.Doc, "implementation") {
				t.Fatalf("wrong selected Platform declaration/docs: %#v", platform)
			}
			if caller == nil || filepath.Base(caller.File) != "common.go" {
				t.Fatalf("missing genuine crossfile call: %#v", caller)
			}
			activePlan, err := buildAuthorityPlan(output, filepath.Join(dir, "common.go"))
			if err != nil || len(activePlan.refs) == 0 {
				t.Fatalf("active image lost compiler calls: %v", err)
			}
			var identities = map[string]bool{}
			for _, excluded := range pkg.BuildConstraints {
				if excluded.ExcludedReason == "" || identities[excluded.ExcludedReason] {
					t.Fatalf("missing/aliased excluded identity: %#v", excluded)
				}
				identities[excluded.ExcludedReason] = true
				selected, err := authorityOutputForSource(output, excluded.File)
				if err != nil {
					t.Fatal(err)
				}
				plan, err := buildAuthorityPlan(selected, excluded.File)
				if err != nil {
					t.Fatal(err)
				}
				if len(plan.decls) != 0 || len(plan.refs) != 0 || len(plan.docs) != 0 || len(plan.cons) != 1 {
					t.Fatalf("inactive source received active semantics: decl=%d refs=%d docs=%d constraints=%d", len(plan.decls), len(plan.refs), len(plan.docs), len(plan.cons))
				}
				var image bytes.Buffer
				if err := writeAuthorityImage(&image, excluded.File, output); err != nil {
					t.Fatal(err)
				}
				source, err := os.ReadFile(excluded.File)
				if err != nil {
					t.Fatal(err)
				}
				digest := sha256.Sum256(source)
				if !bytes.Equal(image.Bytes()[20:52], digest[:]) {
					t.Fatal("inactive image lost its exact source binding")
				}
			}
			if len(identities) != 4 {
				t.Fatalf("excluded files = %d, want four actual compiler exclusions", len(identities))
			}
		})
	}
}

func TestCompilerCgoSelectionBothPolicies(t *testing.T) {
	dir := t.TempDir()
	for name, source := range map[string]string{
		"go.mod":    "module example.com/cgo-selection\n\ngo 1.23\n",
		"active.go": "package selection\nfunc Active() int { return 1 }\n",
		"cgo.go":    "package selection\nimport \"C\"\nfunc CgoOnly() int { return 2 }\n",
	} {
		if err := os.WriteFile(filepath.Join(dir, name), []byte(source), 0600); err != nil {
			t.Fatal(err)
		}
	}
	for _, cgo := range []bool{false, true} {
		// NeedFiles queries actual go list selection without requiring a C
		// compiler or pretending that cgo type-checking ran.
		selection := testSelection(t, build.Default.GOOS, build.Default.GOARCH, cgo)
		loaded, err := packages.Load(&packages.Config{Mode: packages.NeedName | packages.NeedFiles, Dir: dir, Env: selection.environment}, ".")
		if err != nil || len(loaded) != 1 || len(loaded[0].Errors) != 0 {
			t.Fatalf("actual cgo file selection: %v %#v", err, loaded)
		}
		constraints, err := scanBuildConstraints(loaded[0], selection.context)
		if err != nil {
			t.Fatal(err)
		}
		activeCgo := false
		for _, path := range loaded[0].GoFiles {
			activeCgo = activeCgo || filepath.Base(path) == "cgo.go"
		}
		if activeCgo != cgo {
			t.Fatalf("CGO_ENABLED=%v active=%v", cgo, activeCgo)
		}
		if cgo && len(constraints) != 0 {
			t.Fatal("enabled cgo file was marked excluded")
		}
		if !cgo && (len(constraints) != 1 || !strings.HasSuffix(constraints[0].ExcludedReason, " cgo-disabled-import-C")) {
			t.Fatalf("missing disabled-cgo witness: %#v", constraints)
		}
	}
}

func TestActualRestServerPairUsesCompilerFilenameSelection(t *testing.T) {
	dir := filepath.Join("..", "..", "..", "tests", "fixtures", "rest-server-listener")
	for _, file := range []struct{ name, digest string }{
		{"listener_windows.go", "8ce2009bb5082aa6b80572b8978799fe9aa5aee73bf2f7df0c8997abc2aea713"},
		{"listener_unix.go", "fc3955e328899f592866ac710068fe02995f526234ac79e45dd381c09cdb66c8"},
	} {
		source, err := os.ReadFile(filepath.Join(dir, file.name))
		if err != nil {
			t.Fatal(err)
		}
		if fmt.Sprintf("%x", sha256.Sum256(source)) != file.digest {
			t.Fatalf("changed authentic rest-server source %s", file.name)
		}
		for _, goos := range []string{"darwin", "windows"} {
			for _, goarch := range []string{"arm64", "amd64"} {
				context := build.Default
				context.GOOS, context.GOARCH, context.CgoEnabled = goos, goarch, false
				selected, err := context.MatchFile(dir, file.name)
				if err != nil {
					t.Fatal(err)
				}
				want := (goos == "windows") == (file.name == "listener_windows.go")
				if selected != want {
					t.Fatalf("%s/%s selected %s=%v, want %v", goos, goarch, file.name, selected, want)
				}
			}
		}
	}
}

func TestCompilerSelectionContradictionsRefuse(t *testing.T) {
	selection := testSelection(t, "darwin", "arm64", false)
	for _, key := range []string{"GOOS", "GOARCH", "CGO_ENABLED"} {
		for _, value := range []string{"", "contradictory"} {
			env := append([]string(nil), selection.environment...)
			for i, entry := range env {
				if strings.HasPrefix(entry, key+"=") {
					env[i] = key + "=" + value
				}
			}
			if _, err := newPackageSelection(selection.context, env); err == nil {
				t.Fatalf("accepted %s=%q", key, value)
			}
		}
	}
	context := selection.context
	context.GOOS = ""
	if _, err := newPackageSelection(context, selection.environment); err == nil {
		t.Fatal("accepted absent context target")
	}
	if _, err := newPackageSelection(selection.context, append(selection.environment, "GOFLAGS=-tags=unadmitted")); err == nil {
		t.Fatal("accepted ambient tags")
	}
	if _, err := newPackageSelection(selection.context, append(selection.environment, "GOOS=darwin")); err == nil {
		t.Fatal("accepted duplicate target")
	}
	if _, err := extractWithSelection(t.TempDir(), ".", nil); err == nil {
		t.Fatal("accepted no selection")
	}
	dir := writeSelectionFixture(t)
	for _, pkg := range []*packages.Package{
		{Name: "selection", Dir: dir, GoFiles: []string{filepath.Join(dir, "listener_windows.go")}},
		{Name: "selection", Dir: dir, IgnoredFiles: []string{filepath.Join(dir, "listener_darwin.go")}},
		{Name: "selection", Dir: dir, IgnoredFiles: []string{filepath.Join(filepath.Dir(dir), "outside.go")}},
	} {
		if _, err := scanBuildConstraints(pkg, selection.context); err == nil {
			t.Fatalf("accepted contradictory package operands: %#v", pkg)
		}
	}
}

func TestNativeSelectionRetainsTheClosedParentPolicy(t *testing.T) {
	selection, err := nativeSelection([]string{"CGO_ENABLED=0"})
	if err != nil {
		t.Fatal(err)
	}
	if selection.context.GOOS != build.Default.GOOS || selection.context.GOARCH != build.Default.GOARCH || selection.context.CgoEnabled || len(selection.context.BuildTags) != 0 {
		t.Fatal("native selection changed the closed parent profile")
	}
	for _, environment := range [][]string{
		nil,
		{"CGO_ENABLED=1"},
		{"CGO_ENABLED=0", "GOOS="},
		{"CGO_ENABLED=0", "GOARCH=contradictory"},
		{"CGO_ENABLED=0", "GOFLAGS=-tags=ambient"},
	} {
		if _, err := nativeSelection(environment); err == nil {
			t.Fatalf("accepted unadmitted native policy %v", environment)
		}
	}
}

func TestAuthoritySourceSelectionMustHaveOneOwner(t *testing.T) {
	source := "/selected/source.go"
	for _, output := range []*Output{
		{Packages: []*Package{{Files: []string{"/selected/other.go"}}}},
		{Packages: []*Package{{Files: []string{source}, BuildConstraints: []*BuildConstraint{{File: source}}}}},
		{Packages: []*Package{{Files: []string{source, source}}}},
		{Packages: []*Package{{BuildConstraints: []*BuildConstraint{{File: source}, {File: source}}}}},
	} {
		if _, err := authorityOutputForSource(output, source); err == nil {
			t.Fatalf("accepted unowned/contradictory source: %#v", output)
		}
	}
}

func TestExternalTestSourceRetainsCompilerPackageOwner(t *testing.T) {
	dir := t.TempDir()
	for name, source := range map[string]string{
		"go.mod":                   "module example.com/test-owner\n\ngo 1.23\n",
		"foo.go":                   "package foo\nfunc Same() int { return 42 }\n",
		"internal_test.go":         "package foo\nfunc Internal() int { return Same() }\n",
		"external_test.go":         "package foo_test\nimport foo \"example.com/test-owner\"\nfunc Same() string { return \"external\" }\nfunc Caller() int { return foo.Same() }\nvar Inferred = foo.Same()\n",
		"external_windows_test.go": "package foo_test\nfunc Dormant() int { return 7 }\n",
	} {
		if err := os.WriteFile(filepath.Join(dir, name), []byte(source), 0600); err != nil {
			t.Fatal(err)
		}
	}
	output, err := extractWithSelection(dir, ".", testSelection(t, "darwin", "arm64", false))
	if err != nil {
		t.Fatal(err)
	}
	if len(output.Errors) != 0 || len(output.Packages) != 2 {
		t.Fatalf("actual test package load: errors=%v packages=%d", output.Errors, len(output.Packages))
	}
	for _, test := range []struct{ file, owner, result string }{
		{"foo.go", "example.com/test-owner", "int"},
		{"internal_test.go", "example.com/test-owner", "int"},
		{"external_test.go", "example.com/test-owner_test", "string"},
		{"external_windows_test.go", "example.com/test-owner_test", ""},
	} {
		selected, err := authorityOutputForSource(output, filepath.Join(dir, test.file))
		if err != nil {
			t.Fatal(err)
		}
		if len(selected.Packages) != 1 || selected.Packages[0].ImportPath != test.owner {
			t.Fatalf("%s got wrong owner: %#v", test.file, selected.Packages)
		}
		pkg := selected.Packages[0]
		plan, err := buildAuthorityPlan(selected, filepath.Join(dir, test.file))
		if err != nil {
			t.Fatal(err)
		}
		if test.result == "" {
			if len(plan.cons) != 1 || len(plan.decls) != 0 || len(plan.refs) != 0 {
				t.Fatal("dormant external test image contains active sibling semantics")
			}
			continue
		}
		var same []*Decl
		for _, decl := range pkg.Decls {
			if decl.Name == "Same" {
				same = append(same, decl)
			}
		}
		if len(same) != 1 || same[0].Signature == nil || len(same[0].Signature.Results) != 1 || same[0].Signature.Results[0].Type.Name != test.result {
			t.Fatalf("%s conflated Same result: %#v", test.file, same)
		}
		if test.file == "external_test.go" {
			known := false
			for _, reference := range pkg.References {
				// Empty kind/class are the protocol's closed free-function call
				// spelling; the image maps them to Call/Func discriminants.
				known = known || (reference.Owner == "Caller" && reference.Target == "Same" && reference.TargetPkg == "example.com/test-owner" && reference.Kind == "" && reference.Class == "")
			}
			if !known {
				t.Fatalf("missing compiler-resolved external test -> ordinary package call: %#v", pkg.References)
			}
			var inferred *Decl
			for _, decl := range pkg.Decls {
				if decl.Name == "Inferred" {
					inferred = decl
				}
			}
			if inferred == nil || inferred.Type == nil || inferred.Type.Kind != "basic" || inferred.Type.Name != "int" {
				t.Fatalf("external inferred result: %#v", inferred)
			}
		}
		var image bytes.Buffer
		if err := writeAuthorityImage(&image, filepath.Join(dir, test.file), output); err != nil {
			t.Fatal(err)
		}
	}
}

func TestIgnoredMalformedBodyDoesNotPoisonActiveCompilerPackage(t *testing.T) {
	dir := t.TempDir()
	for name, source := range map[string]string{
		"go.mod":                   "module example.com/dormant\n\ngo 1.23\n",
		"active.go":                "package dormant\n// Active remains compiler-typed.\nfunc Active() int { return 1 }\nvar Inferred = Active()\n",
		"broken.go":                "//go:build ignore\n\npackage dormant\nfunc Broken( {\n",
		"broken_header_windows.go": "package ???\n",
	} {
		if err := os.WriteFile(filepath.Join(dir, name), []byte(source), 0600); err != nil {
			t.Fatal(err)
		}
	}
	output, err := extractWithSelection(dir, ".", testSelection(t, "darwin", "arm64", false))
	if err != nil {
		t.Fatal(err)
	}
	if len(output.Errors) != 0 || len(output.Packages) != 1 {
		t.Fatalf("native Go must ignore malformed dormant code: %v", output.Errors)
	}
	active, err := authorityOutputForSource(output, filepath.Join(dir, "active.go"))
	if err != nil {
		t.Fatal(err)
	}
	plan, err := buildAuthorityPlan(active, filepath.Join(dir, "active.go"))
	if err != nil {
		t.Fatal(err)
	}
	if len(plan.decls) == 0 || len(plan.refs) == 0 || len(plan.docs) == 0 {
		t.Fatal("unrelated active compiler facts were lost")
	}
	for _, decl := range active.Packages[0].Decls {
		if decl.Name == "Broken" {
			t.Fatal("malformed dormant declaration was fabricated")
		}
	}
	var image bytes.Buffer
	if err := writeAuthorityImage(&image, filepath.Join(dir, "active.go"), output); err != nil {
		t.Fatal(err)
	}
	dormant, err := authorityOutputForSource(output, filepath.Join(dir, "broken.go"))
	if err != nil {
		t.Fatal(err)
	}
	constraint := dormant.Packages[0].BuildConstraints[0]
	if !strings.Contains(constraint.ExcludedReason, " declarations-unavailable:go-parser") || len(constraint.ExportedDecls) != 0 {
		t.Fatalf("untruthful dormant parse status: %#v", constraint)
	}
	plan, err = buildAuthorityPlan(dormant, filepath.Join(dir, "broken.go"))
	if err != nil {
		t.Fatal(err)
	}
	if len(plan.decls) != 0 || len(plan.refs) != 0 || len(plan.cons) != 1 {
		t.Fatal("malformed ignored source received active facts")
	}
	if _, err := authorityOutputForSource(output, filepath.Join(dir, "broken_header_windows.go")); err == nil {
		t.Fatal("unknown dormant package header must not invent a source owner")
	}
}
