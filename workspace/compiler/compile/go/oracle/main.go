// The Go extraction oracle for the nudox compiler.
//
// This is a deliberately thin, dumb extractor: it loads a Go module's
// packages with full static type information (go/types via
// golang.org/x/tools/go/packages LoadAllSyntax), walks every package
// scope, and serializes everything the type system knows into ONE
// exhaustive JSON document on stdout.
//
// No lowering, shaping, or classification happens here — that all lives
// on the Rust side (workspace/compiler/languages/go). The oracle's only
// job is to get information OUT of the Go type system: unexported items,
// doc comments, generics (type params, constraints, type-set unions),
// interfaces (explicit + embedded + fully-expanded method sets), structs
// (fields, tags, embedded fields), value/pointer receivers, const groups
// (with iota detection and exact values), aliases vs defined types, and
// source positions.
//
// Usage:
//
//	oracle [dir]
//
// where dir is the root of the target module (defaults to "."). The
// module's packages are loaded with the pattern "./..." relative to dir.
// Diagnostics go to stderr; the JSON document goes to stdout.
package main

import (
	"encoding/json"
	"fmt"
	"go/types"
	"os"
	"sort"

	"golang.org/x/tools/go/packages"
)

// Output is the root of the emitted JSON document.
type Output struct {
	// Module describes the loaded module (path, dir, Go version).
	Module *Module `json:"module"`
	// Packages holds one entry per package in the module, sorted by
	// import path for deterministic output. omitempty keeps a nil slice
	// out of the document (Go would otherwise emit `null`, which the
	// Rust mirror's defaulted Vec rejects).
	Packages []*Package `json:"packages,omitempty"`
	// Errors carries package-load diagnostics; extraction proceeds
	// best-effort even when some packages fail to type-check.
	Errors []string `json:"errors,omitempty"`
}

// Module mirrors the go.mod-derived module metadata.
type Module struct {
	// Path is the module path (the `module` directive in go.mod).
	Path string `json:"path"`
	// Dir is the on-disk root of the module.
	Dir string `json:"dir,omitempty"`
	// GoVersion is the `go` directive (e.g. "1.22").
	GoVersion string `json:"goVersion,omitempty"`
	// Version is the resolved module version, when known (empty for
	// the main module being analysed from a working tree).
	Version string `json:"version,omitempty"`
}

// Package is one Go package with all of its top-level declarations.
type Package struct {
	// ImportPath is the fully-qualified import path.
	ImportPath string `json:"importPath"`
	// Name is the package identifier (the `package` clause).
	Name string `json:"name"`
	// Doc is the package doc comment (markers stripped, directives
	// removed), joined across files when several carry one.
	Doc string `json:"doc,omitempty"`
	// Files lists the Go source files that make up the package.
	Files []string `json:"files,omitempty"`
	// Decls holds every package-level declaration — exported AND
	// unexported — sorted by name. omitempty: see Output.Packages.
	Decls []*Decl `json:"decls,omitempty"`
}

func main() {
	dir := "."
	if len(os.Args) > 1 {
		dir = os.Args[1]
	}

	out, err := extract(dir)
	if err != nil {
		fmt.Fprintf(os.Stderr, "oracle: %v\n", err)
		os.Exit(1)
	}

	enc := json.NewEncoder(os.Stdout)
	if err := enc.Encode(out); err != nil {
		fmt.Fprintf(os.Stderr, "oracle: encoding JSON: %v\n", err)
		os.Exit(1)
	}
}

// extract loads every package under dir and serializes it.
func extract(dir string) (*Output, error) {
	cfg := &packages.Config{
		Mode: packages.NeedName | packages.NeedFiles | packages.NeedCompiledGoFiles |
			packages.NeedImports | packages.NeedDeps | packages.NeedTypes |
			packages.NeedSyntax | packages.NeedTypesInfo | packages.NeedTypesSizes |
			packages.NeedModule,
		Dir:   dir,
		Tests: false,
	}

	pkgs, err := packages.Load(cfg, "./...")
	if err != nil {
		return nil, fmt.Errorf("loading packages under %s: %w", dir, err)
	}
	if len(pkgs) == 0 {
		return nil, fmt.Errorf("no packages found under %s", dir)
	}

	out := &Output{}
	for _, pkg := range pkgs {
		for _, e := range pkg.Errors {
			out.Errors = append(out.Errors, e.Error())
		}
		if out.Module == nil && pkg.Module != nil {
			out.Module = &Module{
				Path:      pkg.Module.Path,
				Dir:       pkg.Module.Dir,
				GoVersion: pkg.Module.GoVersion,
				Version:   pkg.Module.Version,
			}
		}
		if pkg.Types == nil {
			continue
		}
		out.Packages = append(out.Packages, extractPackage(pkg))
	}

	sort.Slice(out.Packages, func(i, j int) bool {
		return out.Packages[i].ImportPath < out.Packages[j].ImportPath
	})
	return out, nil
}

// extractPackage serializes a single loaded package: its docs (harvested
// from the AST) plus every object in the package scope (which includes
// unexported names — reflection could never see those).
func extractPackage(pkg *packages.Package) *Package {
	docs := harvestDocs(pkg)

	p := &Package{
		ImportPath: pkg.PkgPath,
		Name:       pkg.Name,
		Doc:        docs.packageDoc,
		Files:      pkg.GoFiles,
	}

	scope := pkg.Types.Scope()
	names := scope.Names() // already sorted
	for _, name := range names {
		obj := scope.Lookup(name)
		if obj == nil {
			continue
		}
		if decl := extractObject(pkg, obj, docs); decl != nil {
			p.Decls = append(p.Decls, decl)
		}
	}
	return p
}

// extractObject serializes one package-scope object into a Decl.
func extractObject(pkg *packages.Package, obj types.Object, docs *docCatalog) *Decl {
	s := newSerializer(pkg)

	base := &Decl{
		Name:     obj.Name(),
		Exported: obj.Exported(),
		Doc:      docs.declDoc[obj.Name()],
		Pos:      s.position(obj.Pos()),
	}

	switch obj := obj.(type) {
	case *types.TypeName:
		if obj.IsAlias() {
			base.Kind = "alias"
			base.Target = s.typ(aliasRHS(obj))
			return base
		}
		named, ok := obj.Type().(*types.Named)
		if !ok {
			// Defensive: a non-alias TypeName should always carry a Named.
			base.Kind = "type"
			base.Underlying = s.typ(obj.Type().Underlying())
			return base
		}
		base.Kind = "type"
		base.TypeParams = s.typeParams(named.TypeParams())
		base.Underlying = s.typ(named.Underlying())
		base.Methods = s.declaredMethods(named, docs)
		base.PromotedMethods = s.promotedMethods(named)
		base.FieldDocs = docs.fieldDocs[obj.Name()]
		base.MethodDocs = docs.ifaceMethodDocs[obj.Name()]
		return base

	case *types.Func:
		sig, ok := obj.Type().(*types.Signature)
		if !ok {
			return nil
		}
		base.Kind = "func"
		base.TypeParams = s.typeParams(sig.TypeParams())
		base.Signature = s.signature(sig)
		return base

	case *types.Const:
		base.Kind = "const"
		base.Type = s.typ(obj.Type())
		if obj.Val() != nil {
			base.Value = obj.Val().ExactString()
		}
		if g, ok := docs.constGroup[obj.Name()]; ok {
			base.ConstGroup = g.id
			base.GroupHasIota = g.hasIota
		}
		return base

	case *types.Var:
		base.Kind = "var"
		base.Type = s.typ(obj.Type())
		return base

	default:
		// Builtins / labels / imported package names never sit in a
		// package scope; skip anything unexpected.
		return nil
	}
}

// aliasRHS resolves the aliased (right-hand-side) type of an alias
// declaration, whether or not materialized *types.Alias nodes are enabled
// for this toolchain.
func aliasRHS(obj *types.TypeName) types.Type {
	if a, ok := obj.Type().(*types.Alias); ok {
		return a.Rhs()
	}
	return obj.Type()
}
