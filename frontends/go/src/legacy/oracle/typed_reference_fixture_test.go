package main

import (
	"fmt"
	"go/ast"
	"go/parser"
	"go/token"
	"go/types"
	"path/filepath"
	"testing"

	"golang.org/x/tools/go/packages"
)

type referenceFixtureImporter struct{ dependency *types.Package }

func (i referenceFixtureImporter) Import(path string) (*types.Package, error) {
	if path != i.dependency.Path() {
		return nil, fmt.Errorf("unexpected fixture import %q", path)
	}
	return i.dependency, nil
}

// typedReferenceFixture checks the fixture's actual source files using go/types,
// with the same object and instantiation maps the production package loader uses.
func typedReferenceFixture(t *testing.T, fixture, importPath string) *packages.Package {
	t.Helper()
	root, err := filepath.Abs(filepath.Join("../../../tests/fixtures", fixture))
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
	dependency, err := dependencyConfig.Check(importPath+"/exports", fset, []*ast.File{parse("exports/exports.go")}, nil)
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
	config := types.Config{GoVersion: "go1.23", Importer: referenceFixtureImporter{dependency}}
	checked, err := config.Check(importPath, fset, files, info)
	if err != nil {
		t.Fatal(err)
	}
	return &packages.Package{PkgPath: checked.Path(), Name: checked.Name(), Dir: root,
		Fset: fset, Types: checked, TypesInfo: info, Syntax: files,
		GoFiles: []string{filepath.Join(root, "renamed.go"), filepath.Join(root, "dot.go")}}
}
