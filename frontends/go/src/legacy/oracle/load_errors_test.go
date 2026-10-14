package main

import (
	"os"
	"path/filepath"
	"testing"

	"golang.org/x/tools/go/packages"
)

func loadErrorFixture(t *testing.T) (string, map[string]struct{}) {
	t.Helper()
	dir := t.TempDir()
	for _, name := range []string{"lib.go", "lib_test.go", "other_test.go", "ignored.go"} {
		if err := os.WriteFile(filepath.Join(dir, name), []byte("package lib\n"), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	return dir, map[string]struct{}{filepath.Join(dir, "ignored.go"): {}}
}

func outputWith(t *testing.T, positions ...string) (*Output, string) {
	t.Helper()
	dir, ignored := loadErrorFixture(t)
	out := &Output{}
	for _, position := range positions {
		e := packages.Error{Pos: position, Msg: "boom", Kind: packages.TypeError}
		out.Errors = append(out.Errors, e.Error())
		out.loadErrors = append(out.loadErrors, classifyLoadError(dir, e, ignored))
	}
	return out, dir
}

func TestLoadErrorInATestFileDoesNotRefuseLibraryImages(t *testing.T) {
	out, dir := outputWith(t, "lib_test.go:7:2")
	if got := out.loadRefusalFor(filepath.Join(dir, "lib.go")); got != "" {
		t.Fatalf("library image refused by a test file diagnostic: %q", got)
	}
	for _, test := range []string{"lib_test.go", "other_test.go"} {
		if out.loadRefusalFor(filepath.Join(dir, test)) == "" {
			t.Fatalf("%s must be refused: its package variant has a load error", test)
		}
	}
}

func TestLoadErrorInAnIgnoredFileRefusesOnlyThatFile(t *testing.T) {
	// go/types spells positions absolutely; go list spells them relative.
	out, dir := outputWith(t, "ignored.go:3:9")
	for _, name := range []string{"lib.go", "lib_test.go"} {
		if got := out.loadRefusalFor(filepath.Join(dir, name)); got != "" {
			t.Fatalf("%s refused by an ignored file's diagnostic: %q", name, got)
		}
	}
	if out.loadRefusalFor(filepath.Join(dir, "ignored.go")) == "" {
		t.Fatal("the ignored file's own image must be refused")
	}
}

func TestLoadErrorInLibrarySourceOrWithoutAFileRefusesEverything(t *testing.T) {
	for _, position := range []string{"lib.go:1:1", "-", "", "missing.go:1:1", "/elsewhere/lib.go:1:1"} {
		out, dir := outputWith(t, position)
		for _, name := range []string{"lib.go", "lib_test.go", "ignored.go"} {
			if out.loadRefusalFor(filepath.Join(dir, name)) == "" {
				t.Fatalf("position %q must refuse %s", position, name)
			}
		}
	}
}

func TestAbsoluteTypeCheckerPositionNamesTheDirectorysFile(t *testing.T) {
	dir, _ := loadErrorFixture(t)
	for _, position := range []string{filepath.Join(dir, "lib_test.go") + ":4:5", filepath.Join(dir, "lib_test.go") + ":4", "./lib_test.go:4:5"} {
		if got := diagnosedFile(dir, position); got != filepath.Join(dir, "lib_test.go") {
			t.Fatalf("diagnosedFile(%q) = %q", position, got)
		}
	}
}

func TestNoDiagnosticsRefuseNothing(t *testing.T) {
	out, dir := outputWith(t)
	if got := out.loadRefusalFor(filepath.Join(dir, "lib.go")); got != "" {
		t.Fatalf("unexpected refusal %q", got)
	}
}
