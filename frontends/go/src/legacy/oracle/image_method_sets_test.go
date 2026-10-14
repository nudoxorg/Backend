package main

import (
	"bytes"
	"crypto/sha256"
	"go/build"
	"os"
	"path/filepath"
	"reflect"
	"testing"
)

// Exercise the production package loader and image producer with legal
// nested interfaces, including two outer methods whose signatures recurse.
func TestNestedInterfaceMethodSetsRemainCompleteAndCanonical(t *testing.T) {
	dir := t.TempDir()
	source := []byte("package nested\ntype Outer interface {\n A(interface { Inner() })\n Z() interface { Result(interface { Deep() }) }\n}\n")
	for name, content := range map[string][]byte{
		"go.mod":    []byte("module example.com/nested\n\ngo 1.23\n"),
		"nested.go": source,
	} {
		if err := os.WriteFile(filepath.Join(dir, name), content, 0600); err != nil {
			t.Fatal(err)
		}
	}
	output, err := extractWithSelection(dir, ".", testSelection(t, build.Default.GOOS, build.Default.GOARCH, false))
	if err != nil || len(output.Errors) != 0 {
		t.Fatalf("genuine package load: %v %#v", err, output)
	}
	plan, err := buildAuthorityPlan(output, filepath.Join(dir, "nested.go"))
	if err != nil {
		t.Fatal(err)
	}
	image, err := plan.marshal(sha256.Sum256(source))
	if err != nil {
		t.Fatal(err)
	}
	if len(plan.methodSets) < 5 {
		t.Fatalf("nested method-set evidence missing: %d rows", len(plan.methodSets))
	}
	name := func(cell atomCell) string { return string(plan.atoms[cell.offset : cell.offset+cell.length]) }
	for index, row := range plan.methodSets {
		if index > 0 {
			previous := plan.methodSets[index-1]
			if previous.owner > row.owner || previous.owner == row.owner && name(previous.name) >= name(row.name) {
				t.Fatalf("noncanonical method sets at %d: %#v after %#v", index, row, previous)
			}
		}
	}
	for owner, row := range plan.types {
		if row.kind != 11 {
			continue
		}
		var expected, actual []string
		for _, request := range plan.memberRequests[owner] {
			expected = append(expected, name(request.plan.name))
		}
		for _, method := range plan.methodSets {
			if method.owner == uint32(owner) {
				actual = append(actual, name(method.name))
			}
		}
		if !reflect.DeepEqual(expected, actual) {
			t.Fatalf("owner %d lost method set: got %v, want %v", owner, actual, expected)
		}
	}
	repeated, err := buildAuthorityPlan(output, filepath.Join(dir, "nested.go"))
	if err != nil {
		t.Fatal(err)
	}
	second, err := repeated.marshal(sha256.Sum256(source))
	if err != nil || !bytes.Equal(image, second) {
		t.Fatalf("image is nondeterministic: %v", err)
	}
	// Sorting is not deduplication: hostile duplicate owner/name rows remain
	// present for the Rust reader's strict rejection boundary.
	before := len(repeated.methodSets)
	repeated.methodSets = append(repeated.methodSets, repeated.methodSets[0])
	if err := repeated.finalize(); err != nil {
		t.Fatal(err)
	}
	if len(repeated.methodSets) != before+1 {
		t.Fatal("duplicate method-set row was silently removed")
	}
}
