package main

import (
	"crypto/sha256"
	"fmt"
	"go/ast"
	"go/parser"
	"go/token"
	"go/types"
	"io"
	"os"
	"path/filepath"
	"strings"

	"golang.org/x/tools/go/packages"
)

// Export data preserves declaration lines but can lose columns and byte offsets.
// Only an emitted imported promoted method needs this extra source work. Every
// file comes from the compiler's CompiledGoFiles, and every parse uses the same
// Config.ParseFile hook as packages.Load. Unsupported mappings fall back to the
// full compiler source load; they never emit approximate positions.
type dependencyPositions struct {
	cfg     *packages.Config
	allowed map[string]map[string]bool
	syntax  map[string]map[string]bool
	files   map[string]*dependencyPositionFile
	parsed  []string
	err     error
}

type dependencyPositionFile struct {
	file   *os.File
	info   os.FileInfo
	digest [32]byte
	fset   *token.FileSet
	ast    *ast.File
}

func newDependencyPositions(roots []*packages.Package, cfg *packages.Config) *dependencyPositions {
	if cfg.Mode&packages.NeedDeps != 0 {
		return nil
	}
	r := &dependencyPositions{cfg: cfg, allowed: map[string]map[string]bool{}, syntax: map[string]map[string]bool{}, files: map[string]*dependencyPositionFile{}}
	seen := map[*packages.Package]bool{}
	var visit func(*packages.Package)
	visit = func(pkg *packages.Package) {
		if pkg == nil || seen[pkg] {
			return
		}
		seen[pkg] = true
		if r.allowed[pkg.PkgPath] == nil {
			r.allowed[pkg.PkgPath] = map[string]bool{}
			r.syntax[pkg.PkgPath] = map[string]bool{}
		}
		for _, path := range pkg.CompiledGoFiles {
			r.allowed[pkg.PkgPath][path] = true
		}
		for _, file := range pkg.Syntax {
			if pkg.Fset != nil {
				r.syntax[pkg.PkgPath][pkg.Fset.PositionFor(file.Pos(), false).Filename] = true
			}
		}
		for _, imported := range pkg.Imports {
			visit(imported)
		}
	}
	for _, pkg := range roots {
		visit(pkg)
	}
	return r
}

func (r *dependencyPositions) recoverMethod(fset *token.FileSet, function *types.Func, method *Method) {
	if r.err != nil {
		return
	}
	origin := function.Origin()
	position := fset.PositionFor(origin.Pos(), false)
	owner := origin.Pkg().Path()
	if r.syntax[owner][position.Filename] {
		return // go/packages already supplied genuine source positions.
	}
	r.err = r.recoverMethodSource(origin, position, method)
}

func (r *dependencyPositions) recoverMethodSource(origin *types.Func, position token.Position, method *Method) error {
	if r.cfg.Context != nil && r.cfg.Context.Err() != nil {
		return r.cfg.Context.Err()
	}
	path := position.Filename
	if !position.IsValid() || !filepath.IsAbs(path) || filepath.Clean(path) != path || !r.allowed[origin.Pkg().Path()][path] {
		return fmt.Errorf("imported method %s has no exact compiler-listed source file", origin.FullName())
	}
	if len(r.cfg.Overlay) != 0 || nestedSignaturePositions(method.Signature) {
		return fmt.Errorf("imported method %s needs the compiler's full source mapping", origin.FullName())
	}
	file, err := r.sourceFile(path)
	if err != nil {
		return err
	}
	if file.ast.Name.Name != origin.Pkg().Name() {
		return fmt.Errorf("imported method package differs from compiler source: %s", path)
	}
	signature, ok := origin.Type().(*types.Signature)
	if !ok {
		return fmt.Errorf("imported method lacks a compiler signature: %s", origin.FullName())
	}
	var selected *ast.FuncDecl
	for _, decl := range file.ast.Decls {
		function, ok := decl.(*ast.FuncDecl)
		if !ok || function.Recv == nil || len(function.Recv.List) != 1 || function.Name.Name != origin.Name() || receiverTypeName(function.Recv.List[0].Type) != originTypeName(signature) {
			continue
		}
		if selected != nil {
			return fmt.Errorf("ambiguous imported method declaration: %s", origin.FullName())
		}
		selected = function
	}
	if selected == nil || file.fset.PositionFor(selected.Name.Pos(), false).Line != position.Line {
		return fmt.Errorf("imported method declaration differs from compiler line: %s", origin.FullName())
	}
	params, err := sourceTuplePositions(file.fset, selected.Type.Params, signature.Params())
	if err != nil {
		return err
	}
	results, err := sourceTuplePositions(file.fset, selected.Type.Results, signature.Results())
	if err != nil {
		return err
	}
	if len(params) != len(method.Signature.Params) || len(results) != len(method.Signature.Results) {
		return fmt.Errorf("imported instantiated method tuple differs from its source declaration")
	}
	method.Pos = sourcePosition(file.fset, selected.Name.Pos())
	for i, pos := range params {
		method.Signature.Params[i].Pos = pos
	}
	for i, pos := range results {
		method.Signature.Results[i].Pos = pos
	}
	return nil
}

func (r *dependencyPositions) sourceFile(path string) (*dependencyPositionFile, error) {
	if cached := r.files[path]; cached != nil {
		return cached, nil
	}
	file, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	info, err := file.Stat()
	if err != nil || !info.Mode().IsRegular() {
		file.Close()
		return nil, fmt.Errorf("imported compiler source is not a readable regular file: %s", path)
	}
	raw, err := io.ReadAll(file)
	if err != nil {
		file.Close()
		return nil, err
	}
	parse := r.cfg.ParseFile
	if parse == nil {
		parse = func(fset *token.FileSet, filename string, source []byte) (*ast.File, error) {
			return parser.ParseFile(fset, filename, source, parser.AllErrors|parser.ParseComments)
		}
	}
	fset := token.NewFileSet()
	r.parsed = append(r.parsed, path)
	syntax, err := parse(fset, path, raw)
	if err != nil || syntax == nil || fset.File(syntax.Pos()) == nil {
		file.Close()
		return nil, fmt.Errorf("cannot recover imported compiler source positions: %s: %v", path, err)
	}
	for _, group := range syntax.Comments {
		for _, comment := range group.List {
			if strings.HasPrefix(comment.Text, "//line ") || strings.HasPrefix(comment.Text, "/*line ") {
				file.Close()
				return nil, fmt.Errorf("imported line directives need compiler source mapping: %s", path)
			}
		}
	}
	cached := &dependencyPositionFile{file: file, info: info, digest: sha256.Sum256(raw), fset: fset, ast: syntax}
	r.files[path] = cached
	return cached, nil
}

// Held descriptors and a second complete content hash authenticate every cached
// AST before an Output can escape. Source changes trigger a fresh compiler load.
func (r *dependencyPositions) finish() error {
	if r == nil {
		return nil
	}
	if r.err != nil {
		return r.err
	}
	for path, cached := range r.files {
		if r.cfg.Context != nil && r.cfg.Context.Err() != nil {
			return r.cfg.Context.Err()
		}
		named, err := os.Stat(path)
		if err != nil || !os.SameFile(cached.info, named) || cached.info.Size() != named.Size() || !cached.info.ModTime().Equal(named.ModTime()) {
			return fmt.Errorf("imported compiler source changed: %s", path)
		}
		if _, err := cached.file.Seek(0, io.SeekStart); err != nil {
			return err
		}
		hash := sha256.New()
		if _, err := io.Copy(hash, cached.file); err != nil {
			return err
		}
		var digest [32]byte
		copy(digest[:], hash.Sum(nil))
		after, err := cached.file.Stat()
		namedAfter, nameErr := os.Stat(path)
		if err != nil || nameErr != nil || !os.SameFile(cached.info, namedAfter) || !os.SameFile(cached.info, after) || cached.info.Size() != after.Size() || !cached.info.ModTime().Equal(after.ModTime()) || digest != cached.digest {
			return fmt.Errorf("imported compiler source bytes changed: %s", path)
		}
	}
	if r.cfg.Context != nil {
		return r.cfg.Context.Err()
	}
	return nil
}

func (r *dependencyPositions) close() {
	if r != nil {
		for _, file := range r.files {
			file.file.Close()
		}
	}
}

func sourcePosition(fset *token.FileSet, pos token.Pos) *Pos {
	p := fset.Position(pos)
	return &Pos{File: p.Filename, Line: p.Line, Col: p.Column, Offset: p.Offset}
}

func sourceTuplePositions(fset *token.FileSet, fields *ast.FieldList, tuple *types.Tuple) ([]*Pos, error) {
	var positions []*Pos
	var names []string
	if fields != nil {
		for _, field := range fields.List {
			if len(field.Names) == 0 {
				positions = append(positions, sourcePosition(fset, field.Type.Pos()))
				names = append(names, "")
			} else {
				for _, name := range field.Names {
					positions = append(positions, sourcePosition(fset, name.Pos()))
					names = append(names, name.Name)
				}
			}
		}
	}
	if len(positions) != tuple.Len() {
		return nil, fmt.Errorf("imported method source/compiler tuple arity differs")
	}
	for i, name := range names {
		if tuple.At(i).Name() != name {
			return nil, fmt.Errorf("imported method source/compiler parameter name differs")
		}
	}
	return positions, nil
}

// Nested function/interface positions need more than an outer tuple mapping.
// Delegate those shapes to the compiler instead of suppressing any coordinates.
func nestedSignaturePositions(signature *Type) bool {
	var visit func(*Type) bool
	visit = func(t *Type) bool {
		if t == nil {
			return false
		}
		if t.Kind == "func" || t.Kind == "interface" {
			return true
		}
		if visit(t.Elem) || visit(t.Key) || visit(t.Value) {
			return true
		}
		for _, child := range append(append(append([]*Type(nil), t.TypeArgs...), t.Types...), typeFieldChildren(t)...) {
			if visit(child) {
				return true
			}
		}
		return false
	}
	for _, param := range append(append([]*Param(nil), signature.Params...), signature.Results...) {
		if visit(param.Type) {
			return true
		}
	}
	return false
}

func typeFieldChildren(t *Type) []*Type {
	var children []*Type
	for _, field := range t.Fields {
		children = append(children, field.Type)
	}
	for _, term := range t.Terms {
		children = append(children, term.Type)
	}
	return children
}
