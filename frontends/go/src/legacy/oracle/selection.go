package main

import (
	"bytes"
	"crypto/sha256"
	"fmt"
	"go/ast"
	"go/build"
	"go/parser"
	"go/token"
	"io"
	"os"
	"path/filepath"
	"runtime"
	"sort"
	"strconv"
	"strings"

	"golang.org/x/tools/go/packages"
)

// packageSelection is request-owned. The same target and tags feed go/packages
// and go/build; neither a Rust platform table nor filename parsing selects files.
type packageSelection struct {
	context     build.Context
	environment []string
}

func nativeSelection(environment []string) (*packageSelection, error) {
	context := build.Default
	if context.GOOS != runtime.GOOS || context.GOARCH != runtime.GOARCH {
		return nil, fmt.Errorf("Go helper target contradicts its native runtime")
	}
	context.CgoEnabled = false
	context.BuildTags = nil
	// The parent admits only native Go with cgo disabled. Make the implicit
	// compiler defaults explicit in the nested loader environment as well.
	for _, target := range [][2]string{{"GOOS", context.GOOS}, {"GOARCH", context.GOARCH}} {
		present := false
		for _, entry := range environment {
			if strings.HasPrefix(entry, target[0]+"=") {
				present = true
			}
		}
		if !present {
			environment = append(environment, target[0]+"="+target[1])
		}
	}
	return newPackageSelection(context, environment)
}

func newPackageSelection(context build.Context, environment []string) (*packageSelection, error) {
	if context.GOOS == "" || context.GOARCH == "" || context.Compiler == "" || context.UseAllFiles {
		return nil, fmt.Errorf("missing or unrestricted Go build selection")
	}
	values := make(map[string]string, len(environment))
	for _, entry := range environment {
		key, value, ok := strings.Cut(entry, "=")
		if !ok || key == "" {
			return nil, fmt.Errorf("malformed Go selection environment")
		}
		if _, exists := values[key]; exists {
			return nil, fmt.Errorf("duplicate Go selection environment key %s", key)
		}
		values[key] = value
	}
	cgo := "0"
	if context.CgoEnabled {
		cgo = "1"
	}
	for _, target := range [][2]string{{"GOOS", context.GOOS}, {"GOARCH", context.GOARCH}, {"CGO_ENABLED", cgo}} {
		if values[target[0]] != target[1] {
			return nil, fmt.Errorf("missing or contradictory Go selection %s", target[0])
		}
	}
	if values["GOFLAGS"] != "" {
		return nil, fmt.Errorf("unadmitted Go build flags")
	}
	context.BuildTags = append([]string(nil), context.BuildTags...)
	context.ToolTags = append([]string(nil), context.ToolTags...)
	context.ReleaseTags = append([]string(nil), context.ReleaseTags...)
	return &packageSelection{context: context, environment: append([]string(nil), environment...)}, nil
}

func (selection *packageSelection) buildFlags() []string {
	if len(selection.context.BuildTags) == 0 {
		return nil
	}
	return []string{"-tags=" + strings.Join(selection.context.BuildTags, ",")}
}

// scanBuildConstraints consumes the compiler's ignored-file set. MatchFile
// checks its agreement with that exact target, including implicit GOOS/GOARCH
// suffixes, source build expressions, compiler/release tags, and custom tags.
// Import-C selection is the separate, explicit cgo policy Go applies after
// MatchFile. A dormant declaration parse failure is recorded as unavailable;
// it cannot turn source that the compiler ignores into an active load failure.
func scanBuildConstraints(pkg *packages.Package, context build.Context) ([]*BuildConstraint, error) {
	if !filepath.IsAbs(pkg.Dir) || filepath.Clean(pkg.Dir) != pkg.Dir {
		return nil, fmt.Errorf("Go package has no admitted absolute directory")
	}
	active := make(map[string]bool, len(pkg.GoFiles))
	match := func(path string) (bool, error) {
		if !filepath.IsAbs(path) || filepath.Clean(path) != path || filepath.Dir(path) != pkg.Dir {
			return false, fmt.Errorf("Go selected file escapes its package directory: %s", path)
		}
		return context.MatchFile(pkg.Dir, filepath.Base(path))
	}
	for _, path := range pkg.GoFiles {
		matches, err := match(path)
		if err != nil {
			return nil, err
		}
		importsCgo, err := fileImportsCgo(path)
		if err != nil {
			return nil, err
		}
		if !matches || (!context.CgoEnabled && importsCgo) || active[path] {
			return nil, fmt.Errorf("Go active file contradicts build selection: %s", path)
		}
		active[path] = true
	}
	ignored := make(map[string]bool, len(pkg.IgnoredFiles))
	var out []*BuildConstraint
	for _, path := range pkg.IgnoredFiles {
		if !strings.HasSuffix(path, ".go") {
			continue
		}
		if active[path] || ignored[path] {
			return nil, fmt.Errorf("Go ignored file contradicts build selection: %s", path)
		}
		ignored[path] = true
		constraint, _, _, err := readCompilerExcludedSource(path, pkg.Dir, pkg.PkgPath, pkg.Name, context)
		if err != nil {
			return nil, err
		}
		if constraint != nil {
			out = append(out, constraint)
		}
	}
	sort.Slice(out, func(i, j int) bool { return out[i].File < out[j].File })
	return out, nil
}

// readCompilerExcludedSource applies the native Go build law to one actual
// compiler-returned ignored operand. expectedNamespace is an actual selected
// package name, or empty for a source-only witness with no semantic package.
func readCompilerExcludedSource(path, directory, directoryImportPath, expectedNamespace string, context build.Context) (*BuildConstraint, [32]byte, string, error) {
	var digest [32]byte
	if directoryImportPath == "" || !filepath.IsAbs(directory) || filepath.Clean(directory) != directory || !filepath.IsAbs(path) || filepath.Clean(path) != path || filepath.Dir(path) != directory {
		return nil, digest, "", fmt.Errorf("Go ignored source has no admitted compiler directory witness: %s", path)
	}
	source, err := os.ReadFile(path)
	if err != nil {
		return nil, digest, "", err
	}
	digest = sha256.Sum256(source)
	// MatchFile and both Go parsers consume the same captured bytes. The
	// caller's context remains unchanged, and this cannot read another file.
	context.OpenFile = func(name string) (io.ReadCloser, error) {
		if name != path {
			return nil, fmt.Errorf("Go source selection requested an unadmitted path: %s", name)
		}
		return io.NopCloser(bytes.NewReader(source)), nil
	}
	matches, err := context.MatchFile(directory, filepath.Base(path))
	if err != nil {
		return nil, digest, "", err
	}
	header, headerError := parser.ParseFile(token.NewFileSet(), path, source, parser.ImportsOnly)
	namespace := ""
	importsCgo := false
	if headerError == nil {
		namespace = header.Name.Name
		importsCgo, headerError = headerImportsCgo(header)
	}
	if expectedNamespace != "" && (headerError != nil || namespace != expectedNamespace) {
		// Unknown clauses belong to source availability, not every semantic
		// namespace that happens to share this directory.
		return nil, digest, namespace, nil
	}
	if matches && (headerError != nil || context.CgoEnabled || !importsCgo) {
		return nil, digest, namespace, fmt.Errorf("Go ignored file contradicts build selection: %s", path)
	}
	expr, constraintError := parseConstraint(source)
	constraints := []string(nil)
	if constraintError == nil && expr != nil {
		constraints = append(constraints, expr.String())
	}
	tags := append([]string(nil), context.BuildTags...)
	sort.Strings(tags)
	cgo := "0"
	if context.CgoEnabled {
		cgo = "1"
	}
	// directoryImportPath comes from a compiler package already returned for
	// this directory. It anchors the file witness and is never an invented
	// import path for its absent external-test semantic namespace.
	reason := fmt.Sprintf("go-build-selection GOOS=%s GOARCH=%s CGO_ENABLED=%s tags=%q file=%q", context.GOOS, context.GOARCH, cgo, tags, directoryImportPath+"/"+filepath.Base(path))
	if expectedNamespace == "" {
		reason += fmt.Sprintf(" source-only namespace=%q source-sha256=%x", namespace, digest)
	}
	if constraintError != nil {
		reason += " constraint-text-unavailable:go-parser"
	}
	var declarations []*BuildDecl
	if headerError != nil {
		reason += " package-unavailable:go-parser declarations-unavailable:go-parser"
	} else {
		declarations, err = exportedDecls(path, source)
		if err != nil {
			declarations = nil
			reason += " declarations-unavailable:go-parser"
		}
	}
	if importsCgo && !context.CgoEnabled {
		reason += " cgo-disabled-import-C"
	}
	return &BuildConstraint{File: path, Constraints: constraints, ExcludedReason: reason, ExportedDecls: declarations, packageUnavailable: headerError != nil}, digest, namespace, nil
}

func captureUnownedExclusions(output *Output, compilerPackages []*packages.Package, ignoredByDirectory map[string]map[string]struct{}, context build.Context) error {
	covered := make(map[string]bool)
	for _, pkg := range output.Packages {
		for _, path := range pkg.Files {
			covered[path] = true
		}
		for _, constraint := range pkg.BuildConstraints {
			covered[constraint.File] = true
		}
	}
	anchors := make(map[string]string)
	for _, pkg := range compilerPackages {
		if pkg.PkgPath != "" && (anchors[pkg.Dir] == "" || pkg.PkgPath < anchors[pkg.Dir]) {
			anchors[pkg.Dir] = pkg.PkgPath
		}
	}
	for directory, paths := range ignoredByDirectory {
		for path := range paths {
			if covered[path] || !strings.HasSuffix(path, ".go") {
				continue
			}
			constraint, digest, namespace, err := readCompilerExcludedSource(path, directory, anchors[directory], "", context)
			if err != nil {
				return err
			}
			output.sourceExclusions = append(output.sourceExclusions, &sourceExclusion{
				constraint: constraint, sourceDigest: digest, declaredNamespace: namespace,
				directory: compilerDirectoryWitness{path: directory, importPath: anchors[directory]},
			})
		}
	}
	sort.Slice(output.sourceExclusions, func(i, j int) bool {
		return output.sourceExclusions[i].constraint.File < output.sourceExclusions[j].constraint.File
	})
	return nil
}

func headerImportsCgo(file *ast.File) (bool, error) {
	for _, imported := range file.Imports {
		path, err := strconv.Unquote(imported.Path.Value)
		if err != nil {
			return false, err
		}
		if path == "C" {
			return true, nil
		}
	}
	return false, nil
}
