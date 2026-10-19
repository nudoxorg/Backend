package main

import (
	"bytes"
	"crypto/sha256"
	"encoding/json"
	"fmt"
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
			t.Fatalf("complete authority image changed for %s (old=%d new=%d): %s", file, oldImage.Len(), newImage.Len(),
				rootSyntaxParityDiagnostic(path, before, after, oldImage.Bytes(), newImage.Bytes()))
		}
	}
}

// Diagnostics never normalize, truncate, or replace the compared images. The
// managed native case supplies an exclusive persistent directory; ordinary test
// invocations still report the full hashes and first differences in the failure.
func rootSyntaxParityDiagnostic(source string, before, after *Output, oldImage, newImage []byte) string {
	firstDifference := func(a, b []byte, skipImageDigest bool) int {
		for i := 0; i < len(a) && i < len(b); i++ {
			if skipImageDigest && i >= 52 && i < 84 {
				continue // Version 6's checksum is a consequence, not the changed record.
			}
			if a[i] != b[i] {
				return i
			}
		}
		if len(a) != len(b) {
			return min(len(a), len(b))
		}
		return -1
	}
	window := func(data []byte, offset int) []byte {
		if offset < 0 {
			return nil
		}
		return data[max(0, offset-64):min(len(data), offset+96)]
	}
	rawOffset := firstDifference(oldImage, newImage, false)
	recordOffset := firstDifference(oldImage, newImage, true)
	summary := fmt.Sprintf("old_sha256=%x new_sha256=%x first_byte=%d first_byte_outside_digest=%d old_hex=%x new_hex=%x",
		sha256.Sum256(oldImage), sha256.Sum256(newImage), rawOffset, recordOffset,
		window(oldImage, recordOffset), window(newImage, recordOffset))
	selectedBefore, err := authorityOutputForSource(before, source)
	if err != nil {
		return summary + "; diagnostic selection before: " + err.Error()
	}
	selectedAfter, err := authorityOutputForSource(after, source)
	if err != nil {
		return summary + "; diagnostic selection after: " + err.Error()
	}
	oldRecords, err := json.MarshalIndent(selectedBefore, "", "  ")
	if err != nil {
		return summary + "; diagnostic encoding before: " + err.Error()
	}
	newRecords, err := json.MarshalIndent(selectedAfter, "", "  ")
	if err != nil {
		return summary + "; diagnostic encoding after: " + err.Error()
	}
	jsonOffset := firstDifference(oldRecords, newRecords, false)
	summary += fmt.Sprintf(" records_first_byte=%d old_records=%q new_records=%q",
		jsonOffset, window(oldRecords, jsonOffset), window(newRecords, jsonOffset))
	dir := os.Getenv("NUDOX_GO_ROOT_SYNTAX_DIAGNOSTICS")
	if dir == "" {
		return summary + "; persistent diagnostic directory was not selected"
	}
	if !filepath.IsAbs(dir) || filepath.Clean(dir) != dir {
		return summary + "; diagnostic directory is not an absolute clean path"
	}
	if err := os.Mkdir(dir, 0700); err != nil {
		return summary + "; exclusive diagnostic directory: " + err.Error()
	}
	files := []struct {
		name string
		data []byte
	}{
		{"before.ngai", oldImage}, {"after.ngai", newImage},
		{"before.records.json", oldRecords}, {"after.records.json", newRecords},
		{"summary.txt", []byte(filepath.Base(source) + "\n" + summary + "\n")},
	}
	for _, file := range files {
		// This bounds failure evidence only. An oversized diagnostic still fails
		// the same complete byte-parity assertion; no authority is admitted.
		if len(file.data) > 1<<20 {
			return summary + "; diagnostic exceeds one MiB: " + file.name
		}
		f, err := os.OpenFile(filepath.Join(dir, file.name), os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0600)
		if err != nil {
			return summary + "; exclusive diagnostic file: " + err.Error()
		}
		_, writeErr := f.Write(file.data)
		syncErr := f.Sync()
		closeErr := f.Close()
		if writeErr != nil || syncErr != nil || closeErr != nil {
			return summary + fmt.Sprintf("; diagnostic write/sync/close: %v / %v / %v", writeErr, syncErr, closeErr)
		}
	}
	d, err := os.Open(dir)
	if err != nil {
		return summary + "; diagnostic directory open: " + err.Error()
	}
	syncErr, closeErr := d.Sync(), d.Close()
	if syncErr != nil || closeErr != nil {
		return summary + fmt.Sprintf("; diagnostic directory sync/close: %v / %v", syncErr, closeErr)
	}
	return summary + "; complete diagnostic files: " + dir
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
