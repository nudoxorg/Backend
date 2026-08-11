// Serialization of go/types values into the oracle's JSON schema.
//
// The one structural rule: NAMED types (and aliases) are serialized as
// *references* — {kind:"named", pkg, name, typeArgs} — never expanded
// inline. Their full definitions live in their owning package's decl
// list. Anonymous composite types (structs, interfaces, funcs, maps, …)
// are expanded structurally. Because Go cannot express a cyclic type
// without a named intermediary, this guarantees termination; a depth cap
// guards against pathological inputs anyway.
package main

import (
	"fmt"
	"go/token"
	"go/types"

	"golang.org/x/tools/go/packages"
)

// maxDepth bounds structural type recursion. Anonymous Go types cannot
// cycle (cycles require a named type, which we emit by reference), so
// this is purely a defensive limit.
const maxDepth = 64

// Decl is one package-level declaration.
type Decl struct {
	// Kind discriminates the payload: "type" (defined type), "alias",
	// "func", "const", or "var".
	Kind string `json:"kind"`
	// Name is the declared identifier.
	Name string `json:"name"`
	// Exported reports whether the identifier is exported.
	Exported bool `json:"exported"`
	// Doc is the declaration's doc comment, markers stripped and
	// //go: directives removed (ast.CommentGroup.Text semantics).
	Doc string `json:"doc,omitempty"`
	// Pos is the declaring source position.
	Pos *Pos `json:"pos,omitempty"`

	// TypeParams lists generic type parameters with their constraints
	// (kind == "type" or "func").
	TypeParams []*TypeParamDecl `json:"typeParams,omitempty"`

	// Span is the byte range of this declaration's full source text — for
	// a single (non-grouped) declaration, from its leading keyword
	// (`func`/`type`/`const`/`var`) to its end; for one member of a
	// grouped declaration (`const ( A; B )`), just that spec, since the
	// keyword documents the group rather than the member. See
	// docCatalog.declSpan in docs.go for how this is computed, and
	// serializer.span for how it is resolved to byte offsets. Nil when
	// the oracle could not find an enclosing AST node (should not happen
	// for anything with a non-nil Pos).
	Span *Span `json:"span,omitempty"`
	// Underlying is the structural underlying type (kind == "type").
	Underlying *Type `json:"underlying,omitempty"`
	// Methods are the methods DECLARED on this named type.
	Methods []*Method `json:"methods,omitempty"`
	// PromotedMethods are methods reachable through embedded fields —
	// the rest of the full method set of *T — each with its origin.
	PromotedMethods []*Method `json:"promotedMethods,omitempty"`
	// FieldDocs maps struct field name -> doc comment (kind == "type"
	// whose underlying is a struct).
	FieldDocs map[string]string `json:"fieldDocs,omitempty"`
	// MethodDocs maps interface method name -> doc comment (kind ==
	// "type" whose underlying is an interface).
	MethodDocs map[string]string `json:"methodDocs,omitempty"`

	// Target is the aliased type (kind == "alias").
	Target *Type `json:"target,omitempty"`

	// Signature is the function type (kind == "func").
	Signature *Type `json:"signature,omitempty"`

	// Type is the declared/inferred type (kind == "const" or "var").
	Type *Type `json:"type,omitempty"`
	// Value is the exact constant value (constant.Value.ExactString),
	// e.g. "42", `"hello"`, "1/3" (kind == "const").
	Value string `json:"value,omitempty"`
	// ConstGroup identifies the const declaration block this constant
	// belongs to (unique per package). Constants of the same defined
	// type sharing a group form Go's enum convention.
	ConstGroup int `json:"constGroup,omitempty"`
	// GroupHasIota reports whether the const group uses iota.
	GroupHasIota bool `json:"groupHasIota,omitempty"`

	// Implements lists in-package interfaces this named type satisfies
	// (method-set inclusion via types.Implements). Only populated for
	// kind == "type" declarations that are not themselves interfaces.
	// Each entry is a named/alias type reference.
	Implements []*Type `json:"implements,omitempty"`
}

// Method is a method attached to a named type (declared or promoted).
type Method struct {
	// Name is the method identifier.
	Name string `json:"name"`
	// Exported reports whether the method is exported.
	Exported bool `json:"exported"`
	// Doc is the method's doc comment.
	Doc string `json:"doc,omitempty"`
	// Pos is the declaring position.
	Pos *Pos `json:"pos,omitempty"`
	// Span is the byte range of the method's full declaration — the
	// `func (recv T) Name(...) ... { ... }` text, start to end (or, for a
	// promoted method, the same range on the *embedded* type's method,
	// since that is where the text actually lives). Nil when the
	// declaring `*ast.FuncDecl` could not be found (should not happen for
	// anything the oracle discovered via source, as opposed to purely
	// synthetic promotion bookkeeping).
	Span *Span `json:"span,omitempty"`
	// RecvName is the receiver binding name (e.g. "s" in `(s *Server)`).
	RecvName string `json:"recvName,omitempty"`
	// PointerRecv distinguishes `func (t *T)` from `func (t T)`.
	PointerRecv bool `json:"pointerRecv"`
	// RecvTypeParams names the receiver's type parameters (e.g. "T" in
	// `func (l *List[T]) Push(v T)`).
	RecvTypeParams []string `json:"recvTypeParams,omitempty"`
	// Signature is the method's function type (receiver excluded).
	Signature *Type `json:"signature"`
	// Origin, for promoted methods, is the qualified embedded type the
	// method was promoted from (e.g. "sync.Mutex").
	Origin string `json:"origin,omitempty"`
}

// TypeParamDecl is one generic type parameter and its constraint.
type TypeParamDecl struct {
	// Name is the type parameter identifier (e.g. "T").
	Name string `json:"name"`
	// Constraint is the constraint type (an interface; possibly a
	// named one like constraints.Ordered, possibly an inline union).
	Constraint *Type `json:"constraint,omitempty"`
}

// Pos is a file:line:column source position, plus the byte offset the
// FileSet resolves it to. Line/Col remain 1-based and human-facing; Offset
// is the 0-based byte index into File that `Span` is expressed in terms of.
type Pos struct {
	File string `json:"file"`
	Line int    `json:"line"`
	Col  int    `json:"col"`
	// Offset is the 0-based byte offset of this position within File.
	Offset int `json:"offset"`
}

// Span is a byte range `[Start, End)` into the file named by the
// declaration's Pos. Unlike Pos (which marks a single point — conventionally
// the declared identifier), Span covers the declaration's full source text,
// so that slicing File[Start:End] reproduces it.
type Span struct {
	Start int `json:"start"`
	End   int `json:"end"`
}

// Type is the recursive structural type tree, discriminated by Kind.
type Type struct {
	// Kind is one of: "basic", "named", "alias", "typeParam",
	// "pointer", "slice", "array", "map", "chan", "func", "struct",
	// "interface", "union", "tuple", "invalid".
	Kind string `json:"kind"`

	// Name is the basic-type name ("int", "rune", "byte", …), the
	// named/alias type name, or the type-parameter name.
	Name string `json:"name,omitempty"`
	// Pkg is the defining package's import path for named/alias types
	// (empty for universe types like "error").
	Pkg string `json:"pkg,omitempty"`
	// TypeArgs are the instantiation arguments of a generic named type
	// (e.g. the "int" in "List[int]").
	TypeArgs []*Type `json:"typeArgs,omitempty"`

	// Elem is the element type of a pointer, slice, array, or chan.
	Elem *Type `json:"elem,omitempty"`
	// Len is the fixed length of an array.
	Len int64 `json:"len,omitempty"`
	// Key and Value carry a map's key/value types.
	Key   *Type `json:"key,omitempty"`
	Value *Type `json:"value,omitempty"`
	// Dir is the channel direction: "send" (chan<-), "recv" (<-chan),
	// or "both".
	Dir string `json:"dir,omitempty"`

	// Params/Results/Variadic describe a func type. A variadic func's
	// final param has a slice type (`...T` arrives as `[]T`).
	Params   []*Param `json:"params,omitempty"`
	Results  []*Param `json:"results,omitempty"`
	Variadic bool     `json:"variadic,omitempty"`

	// Fields are a struct's fields, in declaration order.
	Fields []*StructField `json:"fields,omitempty"`

	// ExplicitMethods are an interface's directly-declared methods.
	ExplicitMethods []*MethodSig `json:"explicitMethods,omitempty"`
	// Embeddeds are an interface's embedded types (interfaces, and in
	// constraint position, unions / concrete terms).
	Embeddeds []*Type `json:"embeddeds,omitempty"`
	// AllMethods is the COMPLETE method set after embedding expansion.
	AllMethods []*MethodSig `json:"allMethods,omitempty"`
	// IsComparable reports whether the interface requires comparability.
	IsComparable bool `json:"isComparable,omitempty"`

	// Terms are a constraint union's terms (e.g. `~int | string`).
	Terms []*Term `json:"terms,omitempty"`

	// Types are a tuple's component types (rare at the surface).
	Types []*Type `json:"types,omitempty"`
}

// Param is a func parameter or result.
type Param struct {
	// Name is the binding name; empty for unnamed params/results.
	Name string `json:"name,omitempty"`
	// Type is the param/result type.
	Type *Type `json:"type"`
}

// StructField is one struct field.
type StructField struct {
	// Name is the field name (for embedded fields, the implicit name).
	Name string `json:"name"`
	// Type is the field type.
	Type *Type `json:"type"`
	// Tag is the raw struct tag, if any (backquotes not included).
	Tag string `json:"tag,omitempty"`
	// Embedded marks anonymous/embedded fields.
	Embedded bool `json:"embedded,omitempty"`
	// Exported reports whether the field name is exported.
	Exported bool `json:"exported"`
}

// MethodSig is an interface method: name + func signature + provenance.
type MethodSig struct {
	// Name is the method name.
	Name string `json:"name"`
	// Exported reports whether the method name is exported.
	Exported bool `json:"exported"`
	// Signature is the method's func type.
	Signature *Type `json:"signature"`
	// Pos is the declaring position (points into the embedded
	// interface's source for inherited methods).
	Pos *Pos `json:"pos,omitempty"`
	// Pkg is the import path of the package that declared the method.
	Pkg string `json:"pkg,omitempty"`
}

// Term is one term of a constraint union.
type Term struct {
	// Tilde marks approximation terms (`~int`: any type whose
	// underlying type is int).
	Tilde bool `json:"tilde,omitempty"`
	// Type is the term's type.
	Type *Type `json:"type"`
}

// serializer carries the file set needed to resolve positions.
type serializer struct {
	fset *token.FileSet
}

func newSerializer(pkg *packages.Package) *serializer {
	return &serializer{fset: pkg.Fset}
}

func (s *serializer) position(pos token.Pos) *Pos {
	if !pos.IsValid() || s.fset == nil {
		return nil
	}
	p := s.fset.Position(pos)
	return &Pos{File: p.Filename, Line: p.Line, Col: p.Column, Offset: p.Offset}
}

// span resolves a [start, end) pair of token.Pos values (as recorded by
// docCatalog.declSpan / methodSpan) into byte offsets via the FileSet.
// Returns nil if either bound is invalid, so a caller with no recorded
// range simply omits Span rather than emitting a false 0..0.
func (s *serializer) span(start, end token.Pos) *Span {
	if !start.IsValid() || !end.IsValid() || s.fset == nil {
		return nil
	}
	return &Span{
		Start: s.fset.Position(start).Offset,
		End:   s.fset.Position(end).Offset,
	}
}

// typ serializes any go/types.Type into the JSON tree.
func (s *serializer) typ(t types.Type) *Type {
	return s.typDepth(t, 0)
}

func (s *serializer) typDepth(t types.Type, depth int) *Type {
	if t == nil || depth > maxDepth {
		return &Type{Kind: "invalid"}
	}

	switch t := t.(type) {
	case *types.Basic:
		if t.Kind() == types.UnsafePointer {
			return &Type{Kind: "basic", Name: "unsafe.Pointer"}
		}
		return &Type{Kind: "basic", Name: t.Name()}

	case *types.Named:
		out := &Type{Kind: "named", Name: t.Obj().Name()}
		if p := t.Obj().Pkg(); p != nil {
			out.Pkg = p.Path()
		}
		if args := t.TypeArgs(); args != nil {
			for i := 0; i < args.Len(); i++ {
				out.TypeArgs = append(out.TypeArgs, s.typDepth(args.At(i), depth+1))
			}
		}
		return out

	case *types.Alias:
		out := &Type{Kind: "alias", Name: t.Obj().Name()}
		if p := t.Obj().Pkg(); p != nil {
			out.Pkg = p.Path()
		}
		if args := t.TypeArgs(); args != nil {
			for i := 0; i < args.Len(); i++ {
				out.TypeArgs = append(out.TypeArgs, s.typDepth(args.At(i), depth+1))
			}
		}
		return out

	case *types.TypeParam:
		return &Type{Kind: "typeParam", Name: t.Obj().Name()}

	case *types.Pointer:
		return &Type{Kind: "pointer", Elem: s.typDepth(t.Elem(), depth+1)}

	case *types.Slice:
		return &Type{Kind: "slice", Elem: s.typDepth(t.Elem(), depth+1)}

	case *types.Array:
		return &Type{Kind: "array", Len: t.Len(), Elem: s.typDepth(t.Elem(), depth+1)}

	case *types.Map:
		return &Type{
			Kind:  "map",
			Key:   s.typDepth(t.Key(), depth+1),
			Value: s.typDepth(t.Elem(), depth+1),
		}

	case *types.Chan:
		dir := "both"
		switch t.Dir() {
		case types.SendOnly:
			dir = "send"
		case types.RecvOnly:
			dir = "recv"
		}
		return &Type{Kind: "chan", Dir: dir, Elem: s.typDepth(t.Elem(), depth+1)}

	case *types.Signature:
		return s.signatureDepth(t, depth)

	case *types.Struct:
		out := &Type{Kind: "struct"}
		for i := 0; i < t.NumFields(); i++ {
			f := t.Field(i)
			out.Fields = append(out.Fields, &StructField{
				Name:     f.Name(),
				Type:     s.typDepth(f.Type(), depth+1),
				Tag:      t.Tag(i),
				Embedded: f.Embedded(),
				Exported: f.Exported(),
			})
		}
		return out

	case *types.Interface:
		out := &Type{Kind: "interface", IsComparable: t.IsComparable()}
		for i := 0; i < t.NumExplicitMethods(); i++ {
			out.ExplicitMethods = append(out.ExplicitMethods, s.methodSig(t.ExplicitMethod(i), depth))
		}
		for i := 0; i < t.NumEmbeddeds(); i++ {
			out.Embeddeds = append(out.Embeddeds, s.typDepth(t.EmbeddedType(i), depth+1))
		}
		for i := 0; i < t.NumMethods(); i++ {
			out.AllMethods = append(out.AllMethods, s.methodSig(t.Method(i), depth))
		}
		return out

	case *types.Union:
		out := &Type{Kind: "union"}
		for i := 0; i < t.Len(); i++ {
			term := t.Term(i)
			out.Terms = append(out.Terms, &Term{
				Tilde: term.Tilde(),
				Type:  s.typDepth(term.Type(), depth+1),
			})
		}
		return out

	case *types.Tuple:
		out := &Type{Kind: "tuple"}
		for i := 0; i < t.Len(); i++ {
			out.Types = append(out.Types, s.typDepth(t.At(i).Type(), depth+1))
		}
		return out

	default:
		return &Type{Kind: "invalid", Name: fmt.Sprintf("%T", t)}
	}
}

// signature serializes a func type (the receiver, if any, is NOT part of
// the emitted params — it is carried on Method instead).
func (s *serializer) signature(sig *types.Signature) *Type {
	return s.signatureDepth(sig, 0)
}

func (s *serializer) signatureDepth(sig *types.Signature, depth int) *Type {
	out := &Type{Kind: "func", Variadic: sig.Variadic()}
	params := sig.Params()
	for i := 0; i < params.Len(); i++ {
		p := params.At(i)
		out.Params = append(out.Params, &Param{Name: p.Name(), Type: s.typDepth(p.Type(), depth+1)})
	}
	results := sig.Results()
	for i := 0; i < results.Len(); i++ {
		r := results.At(i)
		out.Results = append(out.Results, &Param{Name: r.Name(), Type: s.typDepth(r.Type(), depth+1)})
	}
	return out
}

// methodSig serializes an interface method.
func (s *serializer) methodSig(f *types.Func, depth int) *MethodSig {
	ms := &MethodSig{
		Name:     f.Name(),
		Exported: f.Exported(),
		Pos:      s.position(f.Pos()),
	}
	if p := f.Pkg(); p != nil {
		ms.Pkg = p.Path()
	}
	if sig, ok := f.Type().(*types.Signature); ok {
		ms.Signature = s.signatureDepth(sig, depth+1)
	}
	return ms
}

// typeParams serializes a generic type-parameter list with constraints.
func (s *serializer) typeParams(tps *types.TypeParamList) []*TypeParamDecl {
	if tps == nil || tps.Len() == 0 {
		return nil
	}
	out := make([]*TypeParamDecl, 0, tps.Len())
	for i := 0; i < tps.Len(); i++ {
		tp := tps.At(i)
		out = append(out, &TypeParamDecl{
			Name:       tp.Obj().Name(),
			Constraint: s.typ(tp.Constraint()),
		})
	}
	return out
}

// declaredMethods serializes the methods declared directly on a named
// type (value AND pointer receivers), attaching harvested doc comments.
func (s *serializer) declaredMethods(named *types.Named, docs *docCatalog) []*Method {
	typeName := named.Obj().Name()
	var out []*Method
	for i := 0; i < named.NumMethods(); i++ {
		f := named.Method(i)
		sig, ok := f.Type().(*types.Signature)
		if !ok {
			continue
		}
		m := &Method{
			Name:      f.Name(),
			Exported:  f.Exported(),
			Doc:       docs.methodDoc[typeName+"."+f.Name()],
			Pos:       s.position(f.Pos()),
			Span:      s.methodSpan(docs, typeName, f.Name()),
			Signature: s.signatureDepth(sig, 0),
		}
		if recv := sig.Recv(); recv != nil {
			m.RecvName = recv.Name()
			_, m.PointerRecv = recv.Type().(*types.Pointer)
		}
		if rtp := sig.RecvTypeParams(); rtp != nil {
			for j := 0; j < rtp.Len(); j++ {
				m.RecvTypeParams = append(m.RecvTypeParams, rtp.At(j).Obj().Name())
			}
		}
		out = append(out, m)
	}
	return out
}

// declSpan resolves the recorded AST range for a package-level declaration
// named name (see docCatalog.declSpan in docs.go) into a byte-offset Span.
// Returns nil when no range was recorded — struct fields and other
// declarations with no top-level ast.Decl of their own never populate
// declSpan, and this is how that absence survives into the JSON as "no
// span" rather than a fabricated one.
func (s *serializer) declSpan(docs *docCatalog, name string) *Span {
	r, ok := docs.declSpan[name]
	if !ok {
		return nil
	}
	return s.span(r.start, r.end)
}

// methodSpan resolves the recorded AST range for a method declared on
// typeName (see docCatalog.methodSpan in docs.go) into a byte-offset Span.
// Returns nil when typeName's package was never harvested — true of every
// method promoted from a type outside the current package (e.g. an embedded
// stdlib type), whose declaring source this oracle invocation never parsed
// a docCatalog for.
func (s *serializer) methodSpan(docs *docCatalog, typeName, methodName string) *Span {
	r, ok := docs.methodSpan[typeName+"."+methodName]
	if !ok {
		return nil
	}
	return s.span(r.start, r.end)
}

// promotedMethods serializes methods reachable on *T through embedded
// fields but not declared on T itself, recording the embedded origin.
func (s *serializer) promotedMethods(named *types.Named, docs *docCatalog) []*Method {
	declared := map[string]bool{}
	for i := 0; i < named.NumMethods(); i++ {
		declared[named.Method(i).Name()] = true
	}
	// Interfaces expose their full method set via the interface type
	// itself; promotion only applies to concrete (struct-backed) types.
	if types.IsInterface(named) {
		return nil
	}

	mset := types.NewMethodSet(types.NewPointer(named))
	var out []*Method
	for i := 0; i < mset.Len(); i++ {
		sel := mset.At(i)
		f, ok := sel.Obj().(*types.Func)
		if !ok || declared[f.Name()] {
			continue
		}
		sig, ok := f.Type().(*types.Signature)
		if !ok {
			continue
		}
		m := &Method{
			Name:      f.Name(),
			Exported:  f.Exported(),
			Pos:       s.position(f.Pos()),
			Span:      s.methodSpan(docs, originTypeName(sig), f.Name()),
			Signature: s.signatureDepth(sig, 0),
			Origin:    originOf(sig),
		}
		if recv := sig.Recv(); recv != nil {
			m.RecvName = recv.Name()
			_, m.PointerRecv = recv.Type().(*types.Pointer)
		}
		out = append(out, m)
	}
	return out
}

// implementsInterfaces returns in-package non-empty interfaces that
// named (or *named) satisfies, via types.Implements. Empty interfaces
// (any / interface{}) are skipped — every type implements them.
func (s *serializer) implementsInterfaces(named *types.Named, pkg *types.Package) []*Type {
	if named == nil || pkg == nil {
		return nil
	}
	scope := pkg.Scope()
	var out []*Type
	ptr := types.NewPointer(named)
	for _, name := range scope.Names() {
		obj, ok := scope.Lookup(name).(*types.TypeName)
		if !ok || obj == named.Obj() {
			continue
		}
		iface, ok := obj.Type().Underlying().(*types.Interface)
		if !ok {
			continue
		}
		iface = iface.Complete()
		if iface.Empty() {
			continue
		}
		if types.Implements(named, iface) || types.Implements(ptr, iface) {
			out = append(out, s.typ(obj.Type()))
		}
	}
	return out
}

// originTypeName returns the bare (unqualified) name of a promoted method's
// declaring receiver type, for keying into docCatalog.methodSpan — the same
// key harvestFuncDecl uses when it recorded that method's AST range.
func originTypeName(sig *types.Signature) string {
	recv := sig.Recv()
	if recv == nil {
		return ""
	}
	t := recv.Type()
	if p, ok := t.(*types.Pointer); ok {
		t = p.Elem()
	}
	if n, ok := t.(*types.Named); ok {
		return n.Obj().Name()
	}
	return ""
}

// originOf renders the qualified defining type of a promoted method's
// receiver (e.g. "sync.Mutex" for a method promoted from an embedded
// sync.Mutex).
func originOf(sig *types.Signature) string {
	recv := sig.Recv()
	if recv == nil {
		return ""
	}
	t := recv.Type()
	if p, ok := t.(*types.Pointer); ok {
		t = p.Elem()
	}
	switch t := t.(type) {
	case *types.Named:
		if p := t.Obj().Pkg(); p != nil {
			return p.Path() + "." + t.Obj().Name()
		}
		return t.Obj().Name()
	case *types.Interface:
		return "interface"
	default:
		return types.TypeString(t, nil)
	}
}
