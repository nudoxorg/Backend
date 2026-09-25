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
// `references` now covers calls from METHOD bodies (previously skipped
// entirely — `fn.Recv != nil` short-circuited before the body was ever
// walked) and calls whose target lives in a DIFFERENT package (previously
// discarded via a `target.Pkg() != pkg.Types` filter). See
// `extractReferences` and `Reference.OwnerRecv`/`Reference.TargetPkg` in
// serialize.go. A pre-v3 binary silently reports an empty same-package,
// free-function-only call graph, indistinguishable from a package that
// genuinely makes no such calls.
//
// v4: `references` now covers the full go/types Uses map — every use of a
// keyable named object (package-scope var/const/func/type, method, struct
// field, imported package) with the use's closed `kind`
// (call/read/typeref/import), the used object's closed `class`, the
// receiver type name `recv` for method and field targets, and the
// NAME-TOKEN extent as `start`/`end`. Pre-v4 rows are exactly the v3 rows:
// every pre-existing call row carries an empty `kind`/`class`/`recv` and is
// byte-identical to its v3 spelling (see `Reference.Kind` in serialize.go
// for the additive protocol law). Uses that name no keyable object —
// builtins, universe names, function-local variables, parameters, labels —
// are deliberately absent: they have no package-scoped identity to key a
// reference target with. `Decl.NameSpan` (serialize.go) now also carries
// the declaration's own identifier extent. See `extractReferences` for the
// walk, and `Reference.Class`/`Reference.Kind` for the closed vocabularies.
const SchemaVersion = 4

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
	if len(os.Args) == 4 && os.Args[1] == "--authority-image" {
		out, err := extract(os.Args[3])
		if err != nil {
			fmt.Fprintf(os.Stderr, "oracle: %v\n", err)
			os.Exit(1)
		}
		if err := writeAuthorityImage(os.Stdout, os.Args[2], out); err != nil {
			fmt.Fprintf(os.Stderr, "oracle: %v\n", err)
			os.Exit(1)
		}
		return
	}
	// The package-selected authority image serializes exactly the package that
	// owns `source` while resolving that package's imports (same-module siblings
	// and replaced external modules) from the module rooted at `module`. This is
	// the real-package analogue of a single-package working directory: sibling
	// packages are import context, never serialized declarations, so two
	// packages that share a declaration spelling cannot collide in the image.
	if len(os.Args) == 4 && os.Args[1] == "--authority-image-package" {
		out, err := extractSelectedPackage(os.Args[3], os.Args[2])
		if err != nil {
			fmt.Fprintf(os.Stderr, "oracle: %v\n", err)
			os.Exit(1)
		}
		if err := writeAuthorityImage(os.Stdout, os.Args[2], out); err != nil {
			fmt.Fprintf(os.Stderr, "oracle: %v\n", err)
			os.Exit(1)
		}
		return
	}
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
	return extractWithPattern(dir, "./...")
}

// extractSelectedPackage loads exactly the package that owns sourcePath and
// serializes only that package. moduleDir roots go.mod/go.work discovery so the
// package's imports resolve, but subpackages are loaded as dependencies and are
// never serialized. A source outside moduleDir falls back to the module root.
func extractSelectedPackage(moduleDir, sourcePath string) (*Output, error) {
	pattern := "."
	if relative, err := filepath.Rel(moduleDir, filepath.Dir(sourcePath)); err == nil &&
		relative != "." && relative != "" {
		pattern = "./" + filepath.ToSlash(relative)
	}
	return extractWithPattern(moduleDir, pattern)
}

// extractWithPattern loads one go/packages pattern under dir and serializes it.
func extractWithPattern(dir, pattern string) (*Output, error) {
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

	pkgs, err := packages.Load(cfg, pattern)
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
		// An external `foo_test` package (files ending in `_test.go`
		// declaring `package foo_test`) is never compiled by a plain `go
		// build`; Go's one-package-per-directory rule makes it unreachable
		// except through this exact test-only construction, so every one of
		// its CompiledGoFiles ends in `_test.go`. Its declarations stay out
		// of `out.Packages` (they can never be the source file an authority
		// image is bound to) but the package remains in `selected` above, so
		// its interfaces still count as satisfaction candidates. Skipping it
		// here is what keeps a same-named external-test declaration (e.g.
		// `toml_test.parser`) from colliding, under the coordinate-free
		// declaration identity, with the real package's own same-named
		// declaration (e.g. `toml.parser`): the identity has no room for a
		// package discriminant, so the only correct fix is to never
		// serialize the test-only declaration in the first place.
		if isExternalTestPackage(pkg) {
			continue
		}
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

// isExternalTestPackage reports whether every file packages.Load compiled
// into pkg ends in `_test.go`. The internal test-augmented variant
// richerPackageVariant prefers always mixes production files in (that is
// what makes it "richer"), so a package left with nothing but `_test.go`
// files is, by construction, an external `foo_test` package: Go admits at
// most one non-test package name per directory, so files besides `_test.go`
// ones can never carry a second package name there.
func isExternalTestPackage(pkg *packages.Package) bool {
	if len(pkg.CompiledGoFiles) == 0 {
		return false
	}
	for _, file := range pkg.CompiledGoFiles {
		if !strings.HasSuffix(file, "_test.go") {
			return false
		}
	}
	return true
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
	p.References = extractReferences(pkg, docs)

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

// extractReferences walks every package file and records the go/types Uses
// table — one row per identifier the type checker resolved to a KEYABLE
// named object: a package-scope variable, constant, function, or named
// type; a method; a struct field; or an imported package binding. Each row
// carries the enclosing declaration (function/method for body uses, the
// declaring spec for uses inside package-level type/const/var declarations,
// including initializers and signatures), the closed use `kind`, the used
// object's closed `class`, the receiver type name for method and field
// targets, and the identifier token's exact byte extent.
//
// This is deliberately not a spelling scan: Uses excludes strings,
// comments, and shadowed identifiers, and resolution is go/types' own.
//
// Two classes of identifier are deliberately NOT recorded, because neither
// has an identity the reference-target keying can express:
//   - universe and builtin names (`len`, `error`, `nil`, `iota`, …), whose
//     package is the universe rather than any import path;
//   - function-local objects (parameters, local variables, local type and
//     const declarations, labels), which declare in no package scope.
//
// Self-edges are not recorded (an object's use inside its own declaration
// resolves to the declaration itself, which the entity plane already
// carries) — the same law the v3 call graph applied.
//
// One bounded gap remains by construction: a use inside a multi-name value
// spec (`var a, b = f(), g()`) is owned by a declaration whose recorded
// span covers only its own identifier, so the row could not prove
// owner containment and is skipped. Single-name specs, function and method
// bodies, signatures, and receivers all contain their uses fully.
func extractReferences(pkg *packages.Package, docs *docCatalog) []*Reference {
	var refs []*Reference
	for _, file := range pkg.Syntax {
		for _, decl := range file.Decls {
			switch decl := decl.(type) {
			case *ast.FuncDecl:
				if decl.Body == nil || decl.Name == nil {
					continue
				}
				if decl.Recv == nil && (decl.Name.Name == "init" || decl.Name.Name == "_") {
					// Implicit and blank functions have no declaration row;
					// their use edges are not representable as reference rows
					// and are deliberately not recorded.
					continue
				}
				owner, ownerRecv, ok := referenceOwner(pkg, decl)
				if !ok {
					continue
				}
				span, ok := declarationSpan(docs, decl)
				if !ok {
					continue
				}
				refs = append(refs, usesIn(pkg, owner, ownerRecv, span, decl)...)
			case *ast.GenDecl:
				for _, spec := range decl.Specs {
					name, ok := specOwnerName(spec)
					if !ok || name.Name == "_" {
						// Blank declarations carry no declaration row, so no
						// use can be attributed to one.
						continue
					}
					owner := pkg.TypesInfo.Defs[name]
					if owner == nil {
						continue
					}
					span, ok := docs.declSpan[name.Name]
					if !ok {
						continue
					}
					refs = append(refs, usesIn(pkg, owner, "", span, spec)...)
				}
			}
		}
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

// specOwnerName resolves the first declared identifier of a package-level
// spec: a TypeSpec's name, or a ValueSpec's first name.
func specOwnerName(spec ast.Spec) (*ast.Ident, bool) {
	switch spec := spec.(type) {
	case *ast.TypeSpec:
		return spec.Name, spec.Name != nil
	case *ast.ValueSpec:
		if len(spec.Names) == 0 || spec.Names[0] == nil {
			return nil, false
		}
		return spec.Names[0], true
	default:
		return nil, false
	}
}

// declarationSpan resolves the recorded AST range of a function or method
// declaration (see docCatalog.declSpan / docCatalog.methodSpan): the same
// range the declaration's own Span row carries, so a use the oracle records
// is always contained in its owner's span.
func declarationSpan(docs *docCatalog, decl *ast.FuncDecl) (posRange, bool) {
	if decl.Recv != nil && len(decl.Recv.List) > 0 {
		recvType := receiverTypeName(decl.Recv.List[0].Type)
		if recvType == "" {
			return posRange{}, false
		}
		span, ok := docs.methodSpan[recvType+"."+decl.Name.Name]
		return span, ok
	}
	span, ok := docs.declSpan[decl.Name.Name]
	return span, ok
}

// usesIn records one reference row per identifier under node that the type
// checker resolved to a keyable object. owner is the declaring object the
// rows are attributed to; ownerRecv is its bare receiver type name for
// methods; ownerSpan is the owner's recorded source range, which every
// emitted row must be contained in.
func usesIn(pkg *packages.Package, owner types.Object, ownerRecv string, ownerSpan posRange, node ast.Node) []*Reference {
	// One pre-pass collects the call positions (a call's Fun ident, or the
	// Sel ident of a selector Fun) and the selector parent of every Sel
	// ident, so the main walk can classify each use exactly once.
	callFuns := map[*ast.Ident]bool{}
	selectors := map[*ast.Ident]*ast.SelectorExpr{}
	ast.Inspect(node, func(n ast.Node) bool {
		switch n := n.(type) {
		case *ast.CallExpr:
			switch fun := n.Fun.(type) {
			case *ast.Ident:
				callFuns[fun] = true
			case *ast.SelectorExpr:
				callFuns[fun.Sel] = true
			}
		case *ast.SelectorExpr:
			selectors[n.Sel] = n
		}
		return true
	})

	ownerStart := pkg.Fset.PositionFor(ownerSpan.start, false).Offset
	ownerEnd := pkg.Fset.PositionFor(ownerSpan.end, false).Offset
	file := pkg.Fset.PositionFor(ownerSpan.start, false).Filename

	var refs []*Reference
	ast.Inspect(node, func(child ast.Node) bool {
		ident, ok := child.(*ast.Ident)
		if !ok || ident.Name == "_" {
			return true
		}
		row, ok := classifyUse(pkg, owner, ident, callFuns, selectors)
		if !ok {
			return true
		}
		start := pkg.Fset.PositionFor(ident.Pos(), false).Offset
		end := pkg.Fset.PositionFor(ident.End(), false).Offset
		if start < ownerStart || end > ownerEnd {
			// The use escapes its owner's recorded span (the multi-name
			// value-spec gap documented on extractReferences); the authority
			// image proves owner containment per row, so an uncontainable
			// row is never emitted.
			return true
		}
		row.Owner = owner.Name()
		row.OwnerRecv = ownerRecv
		row.File = file
		row.Start = start
		row.End = end
		refs = append(refs, row)
		return true
	})
	return refs
}

// classifyUse resolves one identifier through the Uses table and renders
// its reference row payload: the used object's identity in the
// reference-target keying (bare name plus foreign package path), the
// object's closed class, the use's closed kind, and the receiver type name
// for method and field targets. ok is false for every identifier the
// reference plane deliberately does not record (see extractReferences).
func classifyUse(pkg *packages.Package, owner types.Object, ident *ast.Ident, callFuns map[*ast.Ident]bool, selectors map[*ast.Ident]*ast.SelectorExpr) (*Reference, bool) {
	obj := pkg.TypesInfo.Uses[ident]
	if obj == nil || obj == owner {
		return nil, false
	}
	row := &Reference{}
	switch obj := obj.(type) {
	case *types.PkgName:
		row.Class = "pkg"
		row.Kind = "import"
		row.Target = obj.Name()
		if imported := obj.Imported(); imported != nil && imported != pkg.Types {
			row.TargetPkg = imported.Path()
		}
		return row, true

	case *types.TypeName:
		// A use of a named type (or alias) is a type use. Type parameters
		// and function-local types declare in no package scope and are
		// excluded by the parent check.
		if obj.Parent() != pkg.Types.Scope() {
			return nil, false
		}
		row.Class = "type"
		row.Kind = "typeref"
		row.Target = obj.Name()
		row.TargetPkg = foreignPackage(pkg, obj)
		return row, true

	case *types.Func:
		row.Target = obj.Name()
		row.TargetPkg = foreignPackage(pkg, obj)
		sig, _ := obj.Type().(*types.Signature)
		method := sig != nil && sig.Recv() != nil
		if method {
			row.Class = "method"
		}
		// A free function keeps the empty class: the v3 call-row spelling,
		// which every pre-existing row must retain byte for byte.
		row.Recv = selectionReceiver(pkg, selectors[ident])
		if method && row.Recv == "" && sig != nil {
			row.Recv = signatureReceiverName(sig)
		}
		if callFuns[ident] {
			// The empty kind is the v3 call spelling.
			row.Kind = ""
		} else {
			// A method value (`f := x.Close`) or a func value use: the
			// object is read, not called.
			row.Kind = "read"
		}
		return row, true

	case *types.Var:
		row.Target = obj.Name()
		row.TargetPkg = foreignPackage(pkg, obj)
		if selection := selectionOf(pkg, selectors[ident]); selection != nil {
			// A field selected through a value: `x.f`, `x.f = v`, `x.f()`.
			row.Class = "field"
			row.Recv = typeReceiverName(selection.Recv())
		} else if obj.Parent() == pkg.Types.Scope() {
			// A package-level variable.
			row.Class = "var"
		} else if obj.Parent() == nil {
			// A struct field with no selection: the struct-literal key
			// shape (`T{Field: v}`), the only field use that is not a
			// selector. The receiver spelling is not recoverable here, so
			// the row keys the field by package and name alone.
			row.Class = "field"
		} else {
			// Parameters and function-local variables declare in no
			// package scope.
			return nil, false
		}
		if callFuns[ident] {
			// A call through a func-typed field or variable keeps the
			// empty v3 call spelling.
			row.Kind = ""
		} else {
			// Reads and writes both use the object; go/types records no
			// lvalue distinction in the Uses table, so both carry the read
			// kind.
			row.Kind = "read"
		}
		return row, true

	case *types.Const:
		if obj.Parent() != pkg.Types.Scope() {
			// Local constants (and the universe `iota`) declare in no
			// package scope.
			return nil, false
		}
		row.Class = "const"
		row.Kind = "read"
		row.Target = obj.Name()
		row.TargetPkg = foreignPackage(pkg, obj)
		return row, true

	default:
		// Builtins, nil, and labels carry no keyable identity.
		return nil, false
	}
}

// selectionOf resolves the type-checker selection for a selector ident,
// or nil when the ident is not a selector's Sel (or the selector resolved
// to a plain qualified identifier, which carries no selection).
func selectionOf(pkg *packages.Package, sel *ast.SelectorExpr) *types.Selection {
	if sel == nil {
		return nil
	}
	return pkg.TypesInfo.Selections[sel]
}

// selectionReceiver resolves the bare named type a selector's receiver is
// written as: `x.Close` on a *Server yields "Server", on a named interface
// the interface's name. Empty when the receiver is not a named type spelling.
func selectionReceiver(pkg *packages.Package, sel *ast.SelectorExpr) string {
	selection := selectionOf(pkg, sel)
	if selection == nil {
		return ""
	}
	return typeReceiverName(selection.Recv())
}

// typeReceiverName dereferences one receiver type down to its named
// spelling (`*T`, `T[P]`, and `*T[P, Q]` all yield "T").
func typeReceiverName(t types.Type) string {
	if t == nil {
		return ""
	}
	if p, ok := t.(*types.Pointer); ok {
		t = p.Elem()
	}
	if named, ok := t.(*types.Named); ok {
		return named.Obj().Name()
	}
	return ""
}

// signatureReceiverName resolves a method's bare receiver type name from
// its signature — the fallback for method uses with no selection recorded.
func signatureReceiverName(sig *types.Signature) string {
	if sig.Recv() == nil {
		return ""
	}
	return typeReceiverName(sig.Recv().Type())
}

// foreignPackage renders the defining package's import path for a
// same-keying foreign target: present only when the object declares outside
// the package this Reference was extracted from.
func foreignPackage(pkg *packages.Package, obj types.Object) string {
	if p := obj.Pkg(); p != nil && p != pkg.Types {
		return p.Path()
	}
	return ""
}

// referenceOwner resolves the *types.Func for a FuncDecl — a package-level
// function or a method — and, for a method, its bare receiver type name (so
// the Rust side can key into GoId::Member rather than GoId::Item; see
// Reference.OwnerRecv). ok is false only when go/types could not resolve the
// declaration to a *types.Func defined in this package, which should not
// happen for any *ast.FuncDecl the type-checker accepted.
func referenceOwner(pkg *packages.Package, fn *ast.FuncDecl) (owner *types.Func, recvType string, ok bool) {
	obj, ok := pkg.TypesInfo.Defs[fn.Name].(*types.Func)
	if !ok || obj.Pkg() != pkg.Types {
		return nil, "", false
	}
	if fn.Recv == nil {
		return obj, "", true
	}
	sig, ok := obj.Type().(*types.Signature)
	if !ok || sig.Recv() == nil {
		return nil, "", false
	}
	t := sig.Recv().Type()
	if p, ok := t.(*types.Pointer); ok {
		t = p.Elem()
	}
	named, ok := t.(*types.Named)
	if !ok {
		return nil, "", false
	}
	return obj, named.Obj().Name(), true
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
	// The declared identifier token is exactly the UTF-8 encoding of the
	// object's name, so its byte extent is the position offset plus the
	// name's byte length (go/types carries no end position on objects).
	if pos := s.position(obj.Pos()); pos != nil {
		base.NameSpan = &Span{Start: pos.Offset, End: pos.Offset + len(obj.Name())}
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
