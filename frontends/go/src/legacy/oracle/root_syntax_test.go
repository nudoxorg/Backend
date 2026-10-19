package main

import (
	"bytes"
	"go/ast"
	"go/parser"
	"go/token"
	"os"
	"path/filepath"
	"sync"
	"testing"

	"golang.org/x/tools/go/packages"
)

func writeRootSyntaxFixture(t *testing.T) string {
	t.Helper()
	root := t.TempDir()
	files := map[string]string{
		"go.mod": "module example.com/rootonly\n\ngo 1.23\n",
		"dep/dep.go": `package dep
// Box preserves imported generic fields and methods, including private fields.
type Box[T any] struct { Value T; hidden int }
func (b *Box[T]) Get() T { return b.Value }
type Reader[T any] interface { Get() T }
func Identity[T any](value T) T { return value }
`,
		"root.go": `package rootonly
import "example.com/rootonly/dep"
type Local struct { dep.Box[string] }
var Current = dep.Box[int]{Value: dep.Identity(7)}
func Read(box *dep.Box[int]) int { return box.Get() }
`,
		"root_test.go": `package rootonly
import ("testing"; "example.com/rootonly/dep")
func TestInternal(t *testing.T) { if Read(&dep.Box[int]{Value: 7}) != 7 { t.Fatal("wrong value") } }
`,
		"external_test.go": `package rootonly_test
import ("testing"; root "example.com/rootonly"; "example.com/rootonly/dep")
func TestExternal(t *testing.T) { if root.Read(&dep.Box[int]{Value: 9}) != 9 { t.Fatal("wrong value") } }
`,
	}
	for name, source := range files {
		path := filepath.Join(root, name)
		if err := os.MkdirAll(filepath.Dir(path), 0700); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path, []byte(source), 0600); err != nil {
			t.Fatal(err)
		}
	}
	return root
}

// Run the production config and its previous NeedDeps mode on the same physical
// fixture. ParseFile observes actual go/packages work, not a load-mode mirror.
func loadRootSyntaxFixture(t *testing.T, root string, allSyntax bool) (*Output, int) {
	t.Helper()
	selection := testSelection(t, "linux", "amd64", false)
	cfg := selectedPackageConfig(root, selection)
	if allSyntax {
		cfg.Mode |= packages.NeedDeps
	}
	var mu sync.Mutex
	dependencyParses := 0
	cfg.ParseFile = func(fset *token.FileSet, filename string, source []byte) (*ast.File, error) {
		if filepath.Dir(filename) == filepath.Join(root, "dep") {
			mu.Lock()
			dependencyParses++
			mu.Unlock()
		}
		return parser.ParseFile(fset, filename, source, parser.AllErrors|parser.ParseComments)
	}
	output, err := extractWithConfig(cfg, ".", selection)
	if err != nil {
		t.Fatal(err)
	}
	return output, dependencyParses
}

func TestRootSyntaxKeepsAuthorityImagesWithoutDependencyParsing(t *testing.T) {
	root := writeRootSyntaxFixture(t)
	before, beforeParses := loadRootSyntaxFixture(t, root, true)
	after, afterParses := loadRootSyntaxFixture(t, root, false)
	if len(before.Errors) != 0 || len(after.Errors) != 0 {
		t.Fatalf("genuine package errors: before=%v after=%v", before.Errors, after.Errors)
	}
	if beforeParses == 0 || afterParses != 0 {
		t.Fatalf("dependency source parses: prior=%d root-only=%d", beforeParses, afterParses)
	}
	for _, file := range []string{"root.go", "root_test.go", "external_test.go"} {
		var oldImage, newImage bytes.Buffer
		path := filepath.Join(root, file)
		if err := writeAuthorityImage(&oldImage, path, before); err != nil {
			t.Fatal(err)
		}
		if err := writeAuthorityImage(&newImage, path, after); err != nil {
			t.Fatal(err)
		}
		if !bytes.Equal(oldImage.Bytes(), newImage.Bytes()) {
			t.Fatalf("complete authority image changed for %s (old=%d new=%d)", file, oldImage.Len(), newImage.Len())
		}
	}
}

func TestRootSyntaxStillRefusesMissingDependency(t *testing.T) {
	root := writeRootSyntaxFixture(t)
	path := filepath.Join(root, "dep", "dep.go")
	if err := os.Remove(path); err != nil {
		t.Fatal(err)
	}
	for _, allSyntax := range []bool{true, false} {
		output, _ := loadRootSyntaxFixture(t, root, allSyntax)
		var image bytes.Buffer
		if err := writeAuthorityImage(&image, filepath.Join(root, "root.go"), output); err == nil {
			t.Fatalf("mode NeedDeps=%t admitted source with a missing imported generic dependency", allSyntax)
		}
	}
}
