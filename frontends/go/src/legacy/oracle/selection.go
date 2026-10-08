package main

import (
	"fmt"
	"go/build"
	"os"
	"path/filepath"
	"runtime"
	"sort"
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
// MatchFile. Every read/parse/selection contradiction refuses the image.
func scanBuildConstraints(pkg *packages.Package, context build.Context) ([]*BuildConstraint, error) {
	if !filepath.IsAbs(pkg.Dir) || filepath.Clean(pkg.Dir) != pkg.Dir {
		return nil, fmt.Errorf("Go package has no admitted absolute directory")
	}
	active := make(map[string]bool, len(pkg.GoFiles))
	selected := func(path string) (bool, bool, error) {
		if !filepath.IsAbs(path) || filepath.Clean(path) != path || filepath.Dir(path) != pkg.Dir {
			return false, false, fmt.Errorf("Go selected file escapes its package directory: %s", path)
		}
		matches, err := context.MatchFile(pkg.Dir, filepath.Base(path))
		if err != nil {
			return false, false, err
		}
		importsCgo, err := fileImportsCgo(path)
		if err != nil {
			return false, false, err
		}
		return matches && (context.CgoEnabled || !importsCgo), importsCgo, nil
	}
	for _, path := range pkg.GoFiles {
		matches, _, err := selected(path)
		if err != nil {
			return nil, err
		}
		if !matches || active[path] {
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
		matches, importsCgo, err := selected(path)
		if err != nil {
			return nil, err
		}
		if matches || active[path] || ignored[path] {
			return nil, fmt.Errorf("Go ignored file contradicts build selection: %s", path)
		}
		ignored[path] = true
		source, err := os.ReadFile(path)
		if err != nil {
			return nil, err
		}
		expr, err := parseConstraint(source)
		if err != nil {
			return nil, err
		}
		constraints := []string(nil)
		if expr != nil {
			constraints = append(constraints, expr.String())
		}
		// This is identity/availability metadata from a compiler-returned
		// operand, never a filename-derived selection predicate. Include the
		// package and file so distinct implicit exclusions cannot alias owners.
		tags := append([]string(nil), context.BuildTags...)
		sort.Strings(tags)
		cgo := "0"
		if context.CgoEnabled {
			cgo = "1"
		}
		reason := fmt.Sprintf("go-build-selection GOOS=%s GOARCH=%s CGO_ENABLED=%s tags=%q file=%q", context.GOOS, context.GOARCH, cgo, tags, pkg.PkgPath+"/"+filepath.Base(path))
		if importsCgo && !context.CgoEnabled {
			reason += " cgo-disabled-import-C"
		}
		declarations, err := exportedDecls(path)
		if err != nil {
			return nil, err
		}
		out = append(out, &BuildConstraint{File: path, Constraints: constraints, ExcludedReason: reason, ExportedDecls: declarations})
	}
	sort.Slice(out, func(i, j int) bool { return out[i].File < out[j].File })
	return out, nil
}
