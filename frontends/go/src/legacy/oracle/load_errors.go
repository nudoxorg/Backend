package main

import (
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"

	"golang.org/x/tools/go/packages"
)

// loadErrorScope says whose authority images a package-load diagnostic
// invalidates. A diagnostic about one test or ignored file must not stop the
// library files of its package from being indexed, but a diagnostic that could
// have changed what a library file's declarations mean must.
type loadErrorScope int

const (
	// loadErrorLibrary covers diagnostics in active non-test source and any
	// diagnostic that names no file of its package. Every image is refused.
	loadErrorLibrary loadErrorScope = iota
	// loadErrorTest covers diagnostics in an active _test.go file. The
	// package's test files are refused; its library files are unaffected.
	loadErrorTest
	// loadErrorIgnored covers diagnostics in a file the build selection ignores.
	// No package contains that file, so only its own image is refused.
	loadErrorIgnored
)

// loadError is one package-load diagnostic with the file it names.
type loadError struct {
	message string
	// file is the absolute path of the named file of the diagnosed package;
	// empty when the diagnostic names none.
	file  string
	scope loadErrorScope
}

// classifyLoadError attributes one go/packages diagnostic to a file of the
// diagnosed package's directory. `ignored` holds that directory's ignored files.
func classifyLoadError(directory string, e packages.Error, ignored map[string]struct{}) loadError {
	result := loadError{message: e.Error(), scope: loadErrorLibrary}
	file := diagnosedFile(directory, e.Pos)
	if file == "" {
		return result
	}
	result.file = file
	switch _, isIgnored := ignored[file]; {
	case isIgnored:
		result.scope = loadErrorIgnored
	case strings.HasSuffix(file, "_test.go"):
		result.scope = loadErrorTest
	}
	return result
}

// diagnosedFile returns the absolute path of the regular file in `directory`
// that a `file:line:col` position names, or "" when it names no such file. Only
// the file's base name is trusted: go list spells positions relative to the
// loader's working directory, go/types spells them absolutely.
func diagnosedFile(directory, position string) string {
	if directory == "" || position == "" || position == "-" {
		return ""
	}
	name := position
	for i := 0; i < 2; i++ {
		cut := strings.LastIndex(name, ":")
		if cut < 0 {
			break
		}
		if _, err := strconv.Atoi(name[cut+1:]); err != nil {
			break
		}
		name = name[:cut]
	}
	base := filepath.Base(filepath.FromSlash(name))
	if base == "." || base == string(filepath.Separator) || !strings.HasSuffix(base, ".go") {
		return ""
	}
	candidate := filepath.Join(directory, base)
	info, err := os.Stat(candidate)
	if err != nil || !info.Mode().IsRegular() {
		return ""
	}
	return candidate
}

// loadRefusalFor returns the diagnostic that forbids an authority image for
// sourcePath, or "" when every load diagnostic is about other files whose
// failure cannot change this one's meaning.
func (o *Output) loadRefusalFor(sourcePath string) string {
	if len(o.loadErrors) == 0 && len(o.Errors) > 0 {
		// Diagnostics without attribution (an Output built by hand) refuse everything.
		return o.Errors[0]
	}
	sourceIsTest := strings.HasSuffix(sourcePath, "_test.go")
	for _, diagnostic := range o.loadErrors {
		switch diagnostic.scope {
		case loadErrorLibrary:
			return diagnostic.message
		case loadErrorTest:
			if sourceIsTest {
				return diagnostic.message
			}
		case loadErrorIgnored:
			if sameFile(diagnostic.file, sourcePath) {
				return diagnostic.message
			}
		default:
			return fmt.Sprintf("unclassified load diagnostic: %s", diagnostic.message)
		}
	}
	return ""
}
