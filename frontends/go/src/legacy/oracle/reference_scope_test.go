package main

import (
	"go/build"
	"os"
	"testing"
)

func TestForeignPackageScopeReferences(t *testing.T) {
	pkg := typedReferenceFixture(t, "foreign-scope", "example.com/foreignscope")
	selection := build.Default
	selection.CgoEnabled = false
	output, err := extractPackage(pkg, nil, selection)
	if err != nil {
		t.Fatal(err)
	}
	for _, want := range []struct{ owner, target, class, kind string }{
		{"Holder", "External", "type", "typeref"},
		{"Holder", "Alias", "type", "typeref"},
		{"Holder", "Generic", "type", "typeref"},
		{"Renamed", "External", "type", "typeref"},
		{"Renamed", "Limit", "const", "read"},
		{"Renamed", "Default", "var", "read"},
		{"Renamed", "Make", "", ""},
		{"Methods", "External", "type", "typeref"},
		{"Methods", "Method", "method", ""},
		{"Dot", "External", "type", "typeref"},
		{"Dot", "Limit", "const", "read"},
		{"Dot", "Default", "var", "read"},
		{"Dot", "Make", "", ""},
	} {
		found := 0
		for _, row := range output.References {
			if row.Owner != want.owner || row.Target != want.target {
				continue
			}
			found++
			if row.TargetPkg != "example.com/foreignscope/exports" || row.Class != want.class || row.Kind != want.kind {
				t.Errorf("%s -> %s lost defining package/class/kind: %#v", want.owner, want.target, row)
			}
			if want.class == "method" && row.Recv != "External" {
				t.Errorf("foreign method lost its receiver: %#v", row)
			}
			source, err := os.ReadFile(row.File)
			if err != nil {
				t.Fatal(err)
			}
			if row.Start < 0 || row.End > len(source) || row.End < row.Start || string(source[row.Start:row.End]) != row.Target {
				t.Errorf("%s -> %s lost exact source token span: %#v", want.owner, want.target, row)
			}
		}
		if found != 1 {
			t.Errorf("%s -> %s: got %d resolved references, want 1", want.owner, want.target, found)
		}
	}
	plan, err := buildAuthorityPlan(&Output{Packages: []*Package{output}}, "")
	if err != nil {
		t.Fatal(err)
	}
	for _, class := range []string{"type", "const", "var"} {
		encodedClass, err := imageReferenceClass(class)
		if err != nil {
			t.Fatal(err)
		}
		found := false
		for _, row := range plan.refs {
			cell := row.targetPkg
			if row.targetClass == encodedClass && string(plan.atoms[cell.offset:cell.offset+cell.length]) == "example.com/foreignscope/exports" {
				found = true
			}
		}
		if !found {
			t.Errorf("authority reference plane lost imported %s identity", class)
		}
	}
}

func TestPackageScopeReferencesExcludeLocalsAndUniverse(t *testing.T) {
	pkg := typedReferenceFixture(t, "foreign-scope", "example.com/foreignscope")
	refs := extractReferences(pkg, harvestDocs(pkg))
	for _, row := range refs {
		if row.Owner == "Shadowed" || row.Owner == "DotShadow" || row.Owner == "GenericLocal" || row.Owner == "LocalGeneric" || row.Owner == "UsesError" || row.Owner == "UsesIota" {
			t.Errorf("lexical local/type parameter admitted as a package target: %#v", row)
		}
		if row.Owner == "Local" && (row.Target == "Shadow" || row.Target == "ShadowLimit" || row.Target == "value" || row.Target == "len") {
			t.Errorf("local or universe object admitted as a package target: %#v", row)
		}
	}
	for _, want := range []struct{ target, class string }{{"LocalType", "type"}, {"LocalLimit", "const"}, {"LocalDefault", "var"}} {
		found := false
		for _, row := range refs {
			if row.Owner == "Local" && row.Target == want.target && row.Class == want.class && row.TargetPkg == "" {
				found = true
			}
		}
		if !found {
			t.Errorf("same-package %s reference was dropped", want.target)
		}
	}
}
