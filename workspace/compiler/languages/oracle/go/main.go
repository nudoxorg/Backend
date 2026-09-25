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
	"go/ast"
	"go/build/constraint"
	"go/parser"
	"go/token"
	"go/types"
	"os"
	"path/filepath"
	"sort"
	"strings"

	"golang.org/x/tools/go/packages"
)

// SchemaVersion is the payload schema this oracle emits.
//
// It exists because every field on the Rust mirror is `#[serde(default)]` —
// correct for forward compatibility, but it makes a binary that predates a
// field indistinguishable from a package that genuinely lacks the data. A
// shipped application does not bundle this binary, so users point at whatever
// copy they find; one that predated `references` made every `refs` query for
// every Go symbol answer "not recorded" while the source produced references
// correctly.
//
// Bump this in lockstep with `Output::REQUIRED_SCHEMA_VERSION` in
// `src/go/oracle.rs` whenever the Rust side starts reading a newly emitted
// field. `tests/go/oracle_staleness.rs` pins the handshake.
//
// v2: `Decl.Implements` now records interface satisfaction against every
// non-empty interface in the loaded module, not just interfaces declared in
// the same package as the satisfying type (see `collectInterfaceCandidates`
// and `serializer.implementsInterfaces` in serialize.go). A pre-v2 binary
// under-reports `subtypes` for every cross-package case — the field is
// present either way, but its *scope* changed, which is exactly what this
// handshake exists to catch (an older binary's narrower answer looks
// identical to "no cross-package implementers exist").
//
// v3: `Package.UnresolvedCgo` names incomplete cgo types (`C.*`, `_Ctype_*`)
// the oracle could not expand. A pre-v3 binary either panics on those
// packages or omits the list, and both look like "no unresolved cgo".
const SchemaVersion = 3

// Output is the root of the emitted JSON document.
type Output struct {
	// SchemaVersion lets the Rust reader tell an out-of-date binary from a
	// package with no data. Never omitempty: absence is the signal for
	// "older than the handshake", so emitting nothing would be a lie once
	// this field exists.
	SchemaVersion int `json:"schemaVersion"`
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
	// BuildConstraints records source files excluded from the active build
	// (for example, a windows-only file on darwin) and the exported
	// declarations they contain. go/types cannot expose these declarations
	// because they are intentionally absent from the loaded package scope.
	BuildConstraints []*BuildConstraint `json:"buildConstraints,omitempty"`
	// References is the resolved same-package function call graph.
	References []*Reference `json:"references,omitempty"`
	// UnresolvedCgo names incomplete cgo (or otherwise unexpandable) types
	// this package touched. Qualified as `import/path.Name`. The package
	// still extracts; these names are gaps, not a panic.
	UnresolvedCgo []string `json:"unresolvedCgo,omitempty"`
}

// BuildConstraint describes one excluded Go source file.
type BuildConstraint struct {
	File          string       `json:"file"`
	Constraints   []string     `json:"constraints,omitempty"`
	ExportedDecls []*BuildDecl `json:"exportedDecls,omitempty"`
}

// BuildDecl identifies an exported declaration found in an excluded file.
type BuildDecl struct {
	Name string `json:"name"`
	Kind string `json:"kind"`
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
		Dir: dir,
		// Load augmented package variants so declarations from _test.go files
		// participate in the same semantic package model as production files.
		Tests: true,
	}

	pkgs, err := packages.Load(cfg, "./...")
	if err != nil {
		return nil, fmt.Errorf("loading packages under %s: %w", dir, err)
	}
	if len(pkgs) == 0 {
		return nil, fmt.Errorf("no packages found under %s", dir)
	}

	out := &Output{SchemaVersion: SchemaVersion}
	selected := make(map[string]*packages.Package)
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
		if pkg.Types == nil || (pkg.Name == "main" && strings.HasSuffix(pkg.PkgPath, ".test")) {
			continue
		}
		// Tests=true returns the ordinary package and an augmented variant with
		// its internal tests. Keep exactly one semantic package per import path:
		// the richest variant. External foo_test packages have a distinct import
		// path and remain independently indexed.
		current := selected[pkg.PkgPath]
		if current == nil || richerPackageVariant(pkg, current) {
			selected[pkg.PkgPath] = pkg
		}
	}
	// Gathered once across every selected package, so `implementsInterfaces`
	// can check a concrete type against interfaces declared anywhere in the
	// module, not only its own package. See collectInterfaceCandidates.
	candidates := collectInterfaceCandidates(selected)
	for _, pkg := range selected {
		out.Packages = append(out.Packages, extractPackage(pkg, candidates))
	}

	sort.Slice(out.Packages, func(i, j int) bool {
		return out.Packages[i].ImportPath < out.Packages[j].ImportPath
	})
	return out, nil
}

// interfaceCandidate is one non-empty interface eligible for cross-package
// satisfaction checks: the TypeName that declares it (so a result can be
// serialized as a reference, and so a type can be excluded from testing
// against itself) paired with its completed method set.
type interfaceCandidate struct {
	obj   *types.TypeName
	iface *types.Interface
}

// collectInterfaceCandidates gathers every non-empty interface declared in
// any package this oracle invocation loaded — not just the package under
// extraction. Go's interface satisfaction is structural and module-wide: a
// type in package A can satisfy an interface in package B with no import
// between them at all (no `implements` keyword exists to state it), so
// scoping the candidate list to one package at a time — the previous
// behavior — silently missed the entire cross-package case. That gap is
// exactly what field-reported `main.AuthDeps` satisfied by `auth.Client`
// hit: both are real packages in the same module, and the edge was empty.
//
// Sorted by (package path, name) for deterministic output regardless of Go
// map iteration order over `selected`.
func collectInterfaceCandidates(selected map[string]*packages.Package) []interfaceCandidate {
	var out []interfaceCandidate
	for _, pkg := range selected {
		if pkg.Types == nil {
			continue
		}
		scope := pkg.Types.Scope()
		for _, name := range scope.Names() {
			obj, ok := scope.Lookup(name).(*types.TypeName)
			if !ok {
				continue
			}
			iface, ok := underlyingInterface(obj)
			if !ok {
				continue
			}
			iface = iface.Complete()
			// Every type implements the empty interface; recording that
			// would be noise on every single concrete type, not information.
			if iface.Empty() {
				continue
			}
			out = append(out, interfaceCandidate{obj: obj, iface: iface})
		}
	}
	sort.Slice(out, func(i, j int) bool {
		pi, pj := "", ""
		if p := out[i].obj.Pkg(); p != nil {
			pi = p.Path()
		}
		if p := out[j].obj.Pkg(); p != nil {
			pj = p.Path()
		}
		if pi != pj {
			return pi < pj
		}
		return out[i].obj.Name() < out[j].obj.Name()
	})
	return out
}

// underlyingInterface reports whether obj's underlying type is an interface.
// Underlying panics on an incomplete cgo named type; that type is not an
// interface candidate, and the panic must not abort the whole module.
func underlyingInterface(obj *types.TypeName) (iface *types.Interface, ok bool) {
	defer func() { _ = recover() }()
	iface, ok = obj.Type().Underlying().(*types.Interface)
	return iface, ok
}

// richerPackageVariant imposes a total, deterministic order on the ordinary
// and test-augmented variants returned by packages.Load. File count is the
// semantic signal: the augmented package contains every production file plus
// its internal tests. The remaining comparisons are deterministic tie-breaks,
// not assumptions about packages.Load result order.
func richerPackageVariant(candidate, current *packages.Package) bool {
	if len(candidate.CompiledGoFiles) != len(current.CompiledGoFiles) {
		return len(candidate.CompiledGoFiles) > len(current.CompiledGoFiles)
	}
	if candidateTests, currentTests := testFileCount(candidate), testFileCount(current); candidateTests != currentTests {
		return candidateTests > currentTests
	}
	return packageVariantKey(candidate) < packageVariantKey(current)
}

func testFileCount(pkg *packages.Package) int {
	count := 0
	for _, file := range pkg.CompiledGoFiles {
		if strings.HasSuffix(file, "_test.go") {
			count++
		}
	}
	return count
}

func packageVariantKey(pkg *packages.Package) string {
	files := append([]string(nil), pkg.CompiledGoFiles...)
	sort.Strings(files)
	return pkg.ID + "\x00" + strings.Join(files, "\x00")
}

// extractPackage serializes a single loaded package: its docs (harvested
// from the AST) plus every object in the package scope (which includes
// unexported names — reflection could never see those). candidates is the
// module-wide interface list from collectInterfaceCandidates, threaded down
// to extractObject so a concrete type declared in this package can be
// checked against interfaces declared in ANY package the module loaded.
func extractPackage(pkg *packages.Package, candidates []interfaceCandidate) *Package {
	docs := harvestDocs(pkg)

	p := &Package{
		ImportPath: pkg.PkgPath,
		Name:       pkg.Name,
		Doc:        docs.packageDoc,
		Files:      pkg.GoFiles,
	}
	p.BuildConstraints = scanBuildConstraints(pkg)
	p.References = extractReferences(pkg)

	scope := pkg.Types.Scope()
	names := scope.Names() // already sorted
	var unresolved []string
	for _, name := range names {
		obj := scope.Lookup(name)
		if obj == nil {
			continue
		}
		decl, extra := extractObject(pkg, obj, docs, candidates)
		if decl != nil {
			p.Decls = append(p.Decls, decl)
		}
		unresolved = append(unresolved, extra...)
	}
	p.UnresolvedCgo = dedupeSorted(unresolved)
	return p
}

func dedupeSorted(names []string) []string {
	if len(names) == 0 {
		return nil
	}
	sort.Strings(names)
	out := []string{names[0]}
	for _, name := range names[1:] {
		if name != out[len(out)-1] {
			out = append(out, name)
		}
	}
	return out
}

// extractReferences walks function bodies with go/types' Uses table. This is
// deliberately not a spelling scan: Uses excludes strings, comments, and
// shadowed locals, and only package-level functions in this package are
// emitted as graph targets.
func extractReferences(pkg *packages.Package) []*Reference {
	var refs []*Reference
	for _, file := range pkg.Syntax {
		ast.Inspect(file, func(node ast.Node) bool {
			fn, ok := node.(*ast.FuncDecl)
			if !ok || fn.Body == nil || fn.Name == nil {
				return true
			}
			if fn.Recv != nil {
				return false
			}
			owner, ok := pkg.TypesInfo.Defs[fn.Name].(*types.Func)
			if !ok || owner.Pkg() != pkg.Types {
				return true
			}
			ast.Inspect(fn.Body, func(child ast.Node) bool {
				ident, ok := child.(*ast.Ident)
				if !ok {
					return true
				}
				target, ok := pkg.TypesInfo.Uses[ident].(*types.Func)
				if !ok || target.Pkg() != pkg.Types || target == owner {
					return true
				}
				start := pkg.Fset.PositionFor(ident.Pos(), false).Offset
				end := pkg.Fset.PositionFor(ident.End(), false).Offset
				file := pkg.Fset.PositionFor(ident.Pos(), false).Filename
				refs = append(refs, &Reference{
					Owner: owner.Name(), Target: target.Name(),
					File: file, Start: start, End: end,
				})
				return true
			})
			return false
		})
	}
	sort.Slice(refs, func(i, j int) bool {
		if refs[i].File != refs[j].File {
			return refs[i].File < refs[j].File
		}
		if refs[i].Start != refs[j].Start {
			return refs[i].Start < refs[j].Start
		}
		if refs[i].Owner != refs[j].Owner {
			return refs[i].Owner < refs[j].Owner
		}
		return refs[i].Target < refs[j].Target
	})
	return refs
}

// scanBuildConstraints is the source-level fallback for declarations that
// packages.Load deliberately omits from the active package. It scans the
// package directory independently of go/types, so platform/build-tag-only
// exported declarations remain visible as typed availability diagnostics.
func scanBuildConstraints(pkg *packages.Package) []*BuildConstraint {
	files := pkg.GoFiles
	if len(files) == 0 {
		files = pkg.CompiledGoFiles
	}
	if len(files) == 0 {
		return nil
	}
	dir := filepath.Dir(files[0])
	active := make(map[string]bool, len(pkg.CompiledGoFiles)+len(pkg.GoFiles))
	for _, file := range append(append([]string{}, pkg.CompiledGoFiles...), pkg.GoFiles...) {
		abs, err := filepath.Abs(file)
		if err == nil {
			active[filepath.Clean(abs)] = true
		}
	}
	entries, err := os.ReadDir(dir)
	if err != nil {
		return nil
	}
	var out []*BuildConstraint
	for _, entry := range entries {
		if entry.IsDir() || !strings.HasSuffix(entry.Name(), ".go") {
			continue
		}
		path := filepath.Join(dir, entry.Name())
		abs, err := filepath.Abs(path)
		if err != nil || active[filepath.Clean(abs)] {
			continue
		}
		source, err := os.ReadFile(path)
		if err != nil {
			continue
		}
		expr, err := parseConstraint(source)
		if err != nil || expr == nil {
			continue
		}
		decls := exportedDecls(path)
		if len(decls) == 0 {
			continue
		}
		out = append(out, &BuildConstraint{
			File:          path,
			Constraints:   []string{expr.String()},
			ExportedDecls: decls,
		})
	}
	sort.Slice(out, func(i, j int) bool { return out[i].File < out[j].File })
	return out
}

// parseConstraint parses one build-constraint line from a source file.
// go/build/constraint exposes Parse for a line, not ParseFile.
func parseConstraint(source []byte) (constraint.Expr, error) {
	for _, line := range strings.Split(string(source), "\n") {
		line = strings.TrimSpace(line)
		if constraint.IsGoBuild(line) || constraint.IsPlusBuild(line) {
			return constraint.Parse(line)
		}
		if line != "" && !strings.HasPrefix(line, "//") {
			break
		}
	}
	return nil, nil
}

func exportedDecls(path string) []*BuildDecl {
	file, err := parser.ParseFile(token.NewFileSet(), path, nil, parser.ParseComments)
	if err != nil {
		return nil
	}
	var out []*BuildDecl
	for _, decl := range file.Decls {
		switch decl := decl.(type) {
		case *ast.FuncDecl:
			if decl.Recv == nil && ast.IsExported(decl.Name.Name) {
				out = append(out, &BuildDecl{Name: decl.Name.Name, Kind: "func"})
			}
		case *ast.GenDecl:
			kind := strings.ToLower(decl.Tok.String())
			for _, spec := range decl.Specs {
				switch spec := spec.(type) {
				case *ast.TypeSpec:
					if ast.IsExported(spec.Name.Name) {
						out = append(out, &BuildDecl{Name: spec.Name.Name, Kind: kind})
					}
				case *ast.ValueSpec:
					for _, name := range spec.Names {
						if ast.IsExported(name.Name) {
							out = append(out, &BuildDecl{Name: name.Name, Kind: kind})
						}
					}
				}
			}
		}
	}
	return out
}

// extractObject serializes one package-scope object into a Decl. candidates
// is the module-wide interface list (see collectInterfaceCandidates), used
// only for kind == "type" declarations that are not themselves interfaces.
func extractObject(pkg *packages.Package, obj types.Object, docs *docCatalog, candidates []interfaceCandidate) (*Decl, []string) {
	s := newSerializer(pkg)

	base := &Decl{
		Name:     obj.Name(),
		Exported: obj.Exported(),
		Doc:      docs.declDoc[obj.Name()],
		Pos:      s.position(obj.Pos()),
		Span:     s.declSpan(docs, obj.Name()),
	}

	switch obj := obj.(type) {
	case *types.TypeName:
		if obj.IsAlias() {
			base.Kind = "alias"
			base.Target = s.typ(aliasRHS(obj))
			// Generic type aliases (Go 1.23+): surface type parameters
			// when the toolchain materializes them on *types.Alias.
			if a, ok := obj.Type().(*types.Alias); ok {
				base.TypeParams = s.typeParams(a.TypeParams())
			}
			return base, s.unresolvedList()
		}
		named, ok := obj.Type().(*types.Named)
		if !ok {
			// Defensive: a non-alias TypeName should always carry a Named.
			base.Kind = "type"
			s.catchUnresolved(nil, func() {
				base.Underlying = s.typ(obj.Type().Underlying())
			})
			if base.Underlying == nil {
				base.Underlying = &Type{Kind: "invalid", Name: obj.Name()}
				s.noteUnresolved(obj.Name())
			}
			return base, s.unresolvedList()
		}
		base.Kind = "type"
		s.catchUnresolved(named, func() {
			base.TypeParams = s.typeParams(named.TypeParams())
		})
		var underlying types.Type
		s.catchUnresolved(named, func() {
			underlying = named.Underlying()
		})
		if underlying == nil {
			base.Underlying = &Type{Kind: "invalid", Name: qualifiedNamed(named)}
			s.noteUnresolved(qualifiedNamed(named))
		} else {
			s.collectCgo(underlying, 0)
			base.Underlying = s.typ(underlying)
		}
		base.Methods = s.declaredMethods(named, docs)
		base.PromotedMethods = s.promotedMethods(named, docs)
		base.FieldDocs = docs.fieldDocs[obj.Name()]
		base.MethodDocs = docs.ifaceMethodDocs[obj.Name()]
		// Interfaces this concrete type satisfies, anywhere in the module.
		// IsInterface and Implements both walk under(), which panics on an
		// incomplete cgo named type; catchUnresolved records the name.
		s.catchUnresolved(named, func() {
			if !types.IsInterface(named) {
				base.Implements = s.implementsInterfaces(named, candidates)
			}
		})
		return base, s.unresolvedList()

	case *types.Func:
		sig, ok := obj.Type().(*types.Signature)
		if !ok {
			return nil, s.unresolvedList()
		}
		base.Kind = "func"
		base.TypeParams = s.typeParams(sig.TypeParams())
		base.Signature = s.signature(sig)
		return base, s.unresolvedList()

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
		return base, s.unresolvedList()

	case *types.Var:
		base.Kind = "var"
		base.Type = s.typ(obj.Type())
		return base, s.unresolvedList()

	default:
		// Builtins / labels / imported package names never sit in a
		// package scope; skip anything unexpected.
		return nil, nil
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
