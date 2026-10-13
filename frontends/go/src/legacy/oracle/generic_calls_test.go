package main

import (
	"fmt"
	"go/ast"
	"go/parser"
	"go/token"
	"go/types"
	"os"
	"path/filepath"
	"testing"

	"golang.org/x/tools/go/packages"
)

type genericCallsImporter struct{ dependency *types.Package }

func (i genericCallsImporter) Import(path string) (*types.Package, error) {
	if path != i.dependency.Path() {
		return nil, fmt.Errorf("unexpected fixture import %q", path)
	}
	return i.dependency, nil
}

func genericCallsPackage(t *testing.T) *packages.Package {
	t.Helper()
	root, err := filepath.Abs("../../../tests/fixtures/generic-calls")
	if err != nil {
		t.Fatal(err)
	}
	fset := token.NewFileSet()
	parse := func(name string) *ast.File {
		file, err := parser.ParseFile(fset, filepath.Join(root, name), nil, parser.ParseComments)
		if err != nil {
			t.Fatal(err)
		}
		return file
	}
	dependencyConfig := types.Config{GoVersion: "go1.23"}
	dependency, err := dependencyConfig.Check("example.com/genericcalls/exports", fset, []*ast.File{parse("exports/exports.go")}, nil)
	if err != nil {
		t.Fatal(err)
	}
	files := []*ast.File{parse("renamed.go"), parse("dot.go")}
	info := &types.Info{
		Defs:       make(map[*ast.Ident]types.Object),
		Uses:       make(map[*ast.Ident]types.Object),
		Selections: make(map[*ast.SelectorExpr]*types.Selection),
		Instances:  make(map[*ast.Ident]types.Instance),
	}
	config := types.Config{GoVersion: "go1.23", Importer: genericCallsImporter{dependency}}
	checked, err := config.Check("example.com/genericcalls", fset, files, info)
	if err != nil {
		t.Fatal(err)
	}
	return &packages.Package{PkgPath: checked.Path(), Name: checked.Name(), Dir: root,
		Fset: fset, Types: checked, TypesInfo: info, Syntax: files,
		GoFiles: []string{filepath.Join(root, "renamed.go"), filepath.Join(root, "dot.go")}}
}

func TestGenericCallReferencesDistinguishCallsAndValues(t *testing.T) {
	pkg := genericCallsPackage(t)
	refs := extractReferences(pkg, harvestDocs(pkg))
	for _, want := range []struct {
		owner, target, class, kind, targetPackage string
		count                                   int
	}{
		{"ExplicitSingle", "One", "", "", "example.com/genericcalls/exports", 1},
		{"ExplicitMultiple", "Two", "", "", "example.com/genericcalls/exports", 1},
		{"Inferred", "One", "", "", "example.com/genericcalls/exports", 1},
		{"Parenthesized", "One", "", "", "example.com/genericcalls/exports", 1},
		{"MultipleParentheses", "Two", "", "", "example.com/genericcalls/exports", 1},
		{"PlainParentheses", "One", "", "", "example.com/genericcalls/exports", 1},
		{"LocalExplicit", "LocalOne", "", "", "", 1},
		{"DotSingle", "One", "", "", "example.com/genericcalls/exports", 1},
		{"DotMultiple", "Two", "", "", "example.com/genericcalls/exports", 1},
		{"InstantiatedValue", "One", "", "read", "example.com/genericcalls/exports", 1},
		{"LocalValue", "LocalOne", "", "read", "", 1},
		{"Indexed", "Callbacks", "var", "read", "", 2},
		{"FieldIndexed", "Callbacks", "field", "read", "", 1},
		{"FieldIndexed", "GlobalRegistry", "var", "read", "", 1},
		{"TypeConversion", "LocalNamed", "type", "typeref", "", 1},
	} {
		found := 0
		for _, row := range refs {
			if row.Owner != want.owner || row.Target != want.target {
				continue
			}
			found++
			if row.Class != want.class || row.Kind != want.kind || row.TargetPkg != want.targetPackage {
				t.Errorf("%s -> %s lost call/value identity: %#v", want.owner, want.target, row)
			}
			source, err := os.ReadFile(row.File)
			if err != nil {
				t.Fatal(err)
			}
			if row.Start < 0 || row.End > len(source) || row.End < row.Start || string(source[row.Start:row.End]) != want.target {
				t.Errorf("%s -> %s lost exact identifier span: %#v", want.owner, want.target, row)
			}
		}
		if found != want.count {
			t.Errorf("%s -> %s: got %d references, want %d", want.owner, want.target, found, want.count)
		}
	}
}

func TestCallOperandIdentifierExcludesIndexedValuesAndTypeConversions(t *testing.T) {
	pkg := genericCallsPackage(t)
	checked := 0
	for _, file := range pkg.Syntax {
		for _, declaration := range file.Decls {
			function, ok := declaration.(*ast.FuncDecl)
			if !ok || (function.Name.Name != "Indexed" && function.Name.Name != "FieldIndexed" && function.Name.Name != "TypeConversion") {
				continue
			}
			ast.Inspect(function, func(node ast.Node) bool {
				if call, ok := node.(*ast.CallExpr); ok {
					checked++
					if ident := callOperandIdentifier(pkg, call.Fun); ident != nil {
						t.Errorf("%s incorrectly calls indexed value/type declaration %q", function.Name.Name, ident.Name)
					}
				}
				return true
			})
		}
	}
	if checked != 4 {
		t.Fatalf("checked %d negative call operands, want 4", checked)
	}
}
