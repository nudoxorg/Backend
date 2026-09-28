// Command goextract reads a real Go package (and one real consumer of it),
// type-checks both with go/types, and prints the facts the v5 symbol-page
// specimens need, as JSON on stdout. Nothing here is hand-written: every
// satisfier is found with types.Implements, every failure kind is read from
// the callable's own body and the bodies it reaches, every use is a resolved
// reference.
//
//	go run . <pkgdir> <consumerdir> > facts.json
package main

import (
	"encoding/json"
	"fmt"
	"go/ast"
	"go/build"
	"go/constant"
	"go/importer"
	"go/parser"
	"go/token"
	"go/types"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
)

type pkgInfo struct {
	fset  *token.FileSet
	files []*ast.File
	pkg   *types.Package
	info  *types.Info
	dir   string
}

type chained struct {
	std  types.Importer
	have map[string]*types.Package
}

func (c chained) Import(path string) (*types.Package, error) {
	if p, ok := c.have[path]; ok {
		return p, nil
	}
	return c.std.Import(path)
}

func load(dir string, fset *token.FileSet, imp types.Importer) *pkgInfo {
	ctx := build.Default
	ents, err := os.ReadDir(dir)
	must(err)
	var files []*ast.File
	for _, e := range ents {
		n := e.Name()
		if !strings.HasSuffix(n, ".go") || strings.HasSuffix(n, "_test.go") {
			continue
		}
		if ok, _ := ctx.MatchFile(dir, n); !ok {
			continue
		}
		f, err := parser.ParseFile(fset, filepath.Join(dir, n), nil, parser.ParseComments)
		must(err)
		files = append(files, f)
	}
	info := &types.Info{
		Types: map[ast.Expr]types.TypeAndValue{},
		Defs:  map[*ast.Ident]types.Object{},
		Uses:  map[*ast.Ident]types.Object{},
	}
	conf := types.Config{Importer: imp, Error: func(error) {}}
	pkg, _ := conf.Check(files[0].Name.Name, fset, files, info)
	return &pkgInfo{fset, files, pkg, info, dir}
}

func must(err error) {
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

// ---------------------------------------------------------------- output shapes

type Pos struct {
	File string `json:"file"`
	Line int    `json:"line"`
}

type Const struct {
	Name  string `json:"name"`
	Value string `json:"value"`
	Doc   string `json:"doc"`
	Pos
}

type Field struct {
	Name     string `json:"name"`
	Type     string `json:"type"`
	Doc      string `json:"doc"`
	Exported bool   `json:"exported"`
}

type Method struct {
	Name string `json:"name"`
	Sig  string `json:"sig"`
	Doc  string `json:"doc"`
	Ptr  bool   `json:"ptr"`
	Pos
}

type Satisfier struct {
	Name     string `json:"name"`
	Pkg      string `json:"pkg"`
	Exported bool   `json:"exported"`
	Ptr      bool   `json:"ptr"`
	Under    string `json:"under"`
	Pos
}

type Site struct {
	Pkg    string `json:"pkg"`
	Caller string `json:"caller"`
	Text   string `json:"text"`
	Pos
}

type Kind struct {
	Type     string   `json:"type"`     // the error's type, or "" for a sentinel / plain error
	Sentinel string   `json:"sentinel"` // ErrHelp
	Messages []string `json:"messages"` // the Error() formats this kind can print
	Makers   []string `json:"makers"`   // the functions whose bodies build it
	Through  string   `json:"through"`  // an interface call it is carried up from
	Wraps    bool     `json:"wraps"`    // has Unwrap: wraps the cause
	Pos
}

type Policy struct {
	Case   string `json:"case"`
	Action string `json:"action"` // return | exit N | panic
	Note   string `json:"note"`
}

type Object struct {
	Name     string      `json:"name"`
	Kind     string      `json:"kind"`
	Doc      string      `json:"doc"`
	Decl     string      `json:"decl"`
	Pos
	Consts   []Const     `json:"consts,omitempty"`
	Fields   []Field     `json:"fields,omitempty"`
	Methods  []Method    `json:"methods,omitempty"`
	Sat      []Satisfier `json:"satisfiers,omitempty"`
	Params   []Field     `json:"params,omitempty"`
	Results  []Field     `json:"results,omitempty"`
	Recv     *Field      `json:"recv,omitempty"`
	Kinds    []Kind      `json:"kinds,omitempty"`
	Policy   []Policy    `json:"policy,omitempty"`
	Makers   []Method    `json:"makers,omitempty"`  // producers: funcs returning it
	Takers   []Method    `json:"takers,omitempty"`  // funcs taking it
	Uses     []Site      `json:"uses,omitempty"`
	UseCount map[string]int `json:"useCount,omitempty"`
}

type Out struct {
	Pkg      string            `json:"pkg"`
	Path     string            `json:"path"`
	Doc      string            `json:"doc"`
	Consumer string            `json:"consumer"`
	Objects  map[string]Object `json:"objects"`
	Counts   map[string]int    `json:"counts"`
}

// ---------------------------------------------------------------- helpers

func (p *pkgInfo) pos(n token.Pos) Pos {
	ps := p.fset.Position(n)
	return Pos{File: filepath.Base(ps.Filename), Line: ps.Line}
}

func (p *pkgInfo) src(n ast.Node) string {
	a := p.fset.Position(n.Pos())
	b := p.fset.Position(n.End())
	data, err := os.ReadFile(a.Filename)
	must(err)
	return string(data[a.Offset:b.Offset])
}

func (p *pkgInfo) line(n token.Pos) string {
	ps := p.fset.Position(n)
	data, err := os.ReadFile(ps.Filename)
	must(err)
	lines := strings.Split(string(data), "\n")
	return strings.TrimSpace(lines[ps.Line-1])
}

func qual(self *types.Package) types.Qualifier {
	return func(o *types.Package) string {
		if o == self {
			return ""
		}
		return o.Name()
	}
}

func docOf(g *ast.CommentGroup) string {
	if g == nil {
		return ""
	}
	return strings.TrimSpace(g.Text())
}

// the declaring AST nodes of every package-level name and method
type decls struct {
	types   map[string]*ast.TypeSpec
	typeDoc map[string]*ast.CommentGroup
	funcs   map[*types.Func]*ast.FuncDecl
	values  map[string]*ast.ValueSpec
	valDoc  map[string]*ast.CommentGroup
}

func (p *pkgInfo) decls() decls {
	d := decls{map[string]*ast.TypeSpec{}, map[string]*ast.CommentGroup{}, map[*types.Func]*ast.FuncDecl{}, map[string]*ast.ValueSpec{}, map[string]*ast.CommentGroup{}}
	for _, f := range p.files {
		for _, dd := range f.Decls {
			switch x := dd.(type) {
			case *ast.GenDecl:
				for _, s := range x.Specs {
					switch s := s.(type) {
					case *ast.TypeSpec:
						d.types[s.Name.Name] = s
						if s.Doc != nil {
							d.typeDoc[s.Name.Name] = s.Doc
						} else {
							d.typeDoc[s.Name.Name] = x.Doc
						}
					case *ast.ValueSpec:
						for _, n := range s.Names {
							d.values[n.Name] = s
							if s.Doc != nil {
								d.valDoc[n.Name] = s.Doc
							} else if s.Comment != nil {
								d.valDoc[n.Name] = s.Comment
							} else if len(x.Specs) == 1 {
								d.valDoc[n.Name] = x.Doc
							}
						}
					}
				}
			case *ast.FuncDecl:
				if fn, ok := p.info.Defs[x.Name].(*types.Func); ok {
					d.funcs[fn] = x
				}
			}
		}
	}
	return d
}

func named(t types.Type) *types.Named {
	if p, ok := t.(*types.Pointer); ok {
		t = p.Elem()
	}
	n, _ := t.(*types.Named)
	return n
}

func funcName(fn *types.Func) string {
	sig := fn.Type().(*types.Signature)
	if r := sig.Recv(); r != nil {
		if n := named(r.Type()); n != nil {
			return n.Obj().Name() + "." + fn.Name()
		}
	}
	return fn.Name()
}

func (p *pkgInfo) method(fn *types.Func, d decls) Method {
	sig := fn.Type().(*types.Signature)
	m := Method{Name: funcName(fn), Sig: types.TypeString(sig, qual(p.pkg))}
	if r := sig.Recv(); r != nil {
		_, m.Ptr = r.Type().(*types.Pointer)
	}
	if fd, ok := d.funcs[fn]; ok {
		m.Doc = docOf(fd.Doc)
		m.Pos = p.pos(fd.Pos())
	}
	return m
}

func allFuncs(p *pkgInfo) []*types.Func {
	var out []*types.Func
	sc := p.pkg.Scope()
	for _, n := range sc.Names() {
		o := sc.Lookup(n)
		if f, ok := o.(*types.Func); ok {
			out = append(out, f)
		}
		if tn, ok := o.(*types.TypeName); ok {
			if nt, ok := tn.Type().(*types.Named); ok {
				for i := 0; i < nt.NumMethods(); i++ {
					out = append(out, nt.Method(i))
				}
			}
		}
	}
	return out
}

func mentions(t types.Type, target *types.Named) bool {
	switch x := t.(type) {
	case *types.Named:
		return x == target
	case *types.Pointer:
		return mentions(x.Elem(), target)
	case *types.Slice:
		return mentions(x.Elem(), target)
	case *types.Map:
		return mentions(x.Key(), target) || mentions(x.Elem(), target)
	case *types.Signature:
		for i := 0; i < x.Params().Len(); i++ {
			if mentions(x.Params().At(i).Type(), target) {
				return true
			}
		}
	}
	return false
}

// producers return the type (or a pointer to it) directly; takers take it as a parameter
func (p *pkgInfo) makersTakers(target *types.Named, d decls) (makers, takers []Method) {
	for _, fn := range allFuncs(p) {
		if !fn.Exported() {
			continue
		}
		sig := fn.Type().(*types.Signature)
		if r := sig.Recv(); r != nil {
			if rn := named(r.Type()); rn != nil && !rn.Obj().Exported() {
				continue
			}
		}
		res := sig.Results()
		made := false
		for i := 0; i < res.Len(); i++ {
			if n := named(res.At(i).Type()); n == target {
				made = true
			}
		}
		if made {
			makers = append(makers, p.method(fn, d))
			continue
		}
		for i := 0; i < sig.Params().Len(); i++ {
			if mentions(sig.Params().At(i).Type(), target) {
				takers = append(takers, p.method(fn, d))
				break
			}
		}
	}
	sort.Slice(makers, func(i, j int) bool { return makers[i].Name < makers[j].Name })
	sort.Slice(takers, func(i, j int) bool { return takers[i].Name < takers[j].Name })
	return
}

// every resolved reference to obj in p, attributed to its enclosing top-level function
func (p *pkgInfo) uses(obj types.Object, pkgName string) (sites []Site) {
	for _, f := range p.files {
		for _, dd := range f.Decls {
			fd, ok := dd.(*ast.FuncDecl)
			caller := ""
			if ok {
				caller = fd.Name.Name
				if fd.Recv != nil && len(fd.Recv.List) > 0 {
					t := fd.Recv.List[0].Type
					if s, ok := t.(*ast.StarExpr); ok {
						t = s.X
					}
					if id, ok := t.(*ast.Ident); ok {
						caller = id.Name + "." + caller
					}
				}
			}
			ast.Inspect(dd, func(n ast.Node) bool {
				id, ok := n.(*ast.Ident)
				if !ok {
					return true
				}
				if p.info.Uses[id] == obj {
					sites = append(sites, Site{Pkg: pkgName, Caller: caller, Text: p.line(id.Pos()), Pos: p.pos(id.Pos())})
				}
				return true
			})
		}
	}
	return
}

// ---------------------------------------------------------------- failure analysis

type failWalk struct {
	p       *pkgInfo
	d       decls
	errT    *types.Interface
	seen    map[*types.Func]bool
	kinds   map[string]*Kind
	order   []string
	msgOf   func(*types.Named, string) []string
}

func (w *failWalk) add(key string, k Kind, maker string) {
	if have, ok := w.kinds[key]; ok {
		for _, m := range k.Messages {
			if !contains(have.Messages, m) {
				have.Messages = append(have.Messages, m)
			}
		}
		if maker != "" && !contains(have.Makers, maker) {
			have.Makers = append(have.Makers, maker)
		}
		return
	}
	if maker != "" {
		k.Makers = []string{maker}
	}
	w.kinds[key] = &k
	w.order = append(w.order, key)
}

func contains(xs []string, s string) bool {
	for _, x := range xs {
		if x == s {
			return true
		}
	}
	return false
}

func (w *failWalk) walk(fn *types.Func) {
	if w.seen[fn] {
		return
	}
	w.seen[fn] = true
	fd, ok := w.d.funcs[fn]
	if !ok || fd.Body == nil {
		return
	}
	me := funcName(fn)
	ast.Inspect(fd.Body, func(n ast.Node) bool {
		switch x := n.(type) {
		case *ast.CompositeLit:
			t := w.p.info.Types[x].Type
			nt := named(t)
			if nt == nil {
				return true
			}
			if types.Implements(types.NewPointer(nt), w.errT) || types.Implements(nt, w.errT) {
				// which message flavour, when the literal says (messageType: X)
				flavour := ""
				for _, e := range x.Elts {
					if kv, ok := e.(*ast.KeyValueExpr); ok {
						if k, ok := kv.Key.(*ast.Ident); ok && k.Name == "messageType" {
							if v, ok := kv.Value.(*ast.Ident); ok {
								flavour = v.Name
							}
						}
					}
				}
				k := Kind{Type: nt.Obj().Name(), Messages: w.msgOf(nt, flavour), Pos: w.p.pos(x.Pos())}
				k.Wraps = hasMethod(nt, "Unwrap")
				w.add(nt.Obj().Name(), k, me)
			}
		case *ast.Ident:
			if v, ok := w.p.info.Uses[x].(*types.Var); ok && v.Parent() == w.p.pkg.Scope() && types.Implements(v.Type(), w.errT) {
				if spec, ok := w.d.values[v.Name()]; ok {
					msg := ""
					for _, val := range spec.Values {
						if c, ok := val.(*ast.CallExpr); ok && len(c.Args) > 0 {
							if lit, ok := c.Args[0].(*ast.BasicLit); ok {
								msg, _ = strconv.Unquote(lit.Value)
							}
						}
					}
					w.add(v.Name(), Kind{Sentinel: v.Name(), Messages: []string{msg}, Pos: w.p.pos(spec.Pos())}, me)
				}
			}
		case *ast.CallExpr:
			var callee types.Object
			switch f := x.Fun.(type) {
			case *ast.Ident:
				callee = w.p.info.Uses[f]
			case *ast.SelectorExpr:
				callee = w.p.info.Uses[f.Sel]
				// a call through an interface method of this package: carried up from its implementers
				if fn, ok := callee.(*types.Func); ok {
					if sig := fn.Type().(*types.Signature); sig.Recv() != nil {
						if _, isIface := sig.Recv().Type().Underlying().(*types.Interface); isIface && fn.Pkg() == w.p.pkg {
							rn := named(sig.Recv().Type())
							if rn != nil && returnsError(sig) {
								w.add("through:"+rn.Obj().Name()+"."+fn.Name(), Kind{Through: rn.Obj().Name() + "." + fn.Name(), Pos: w.p.pos(x.Pos())}, me)
							}
						}
					}
				}
			}
			if fn, ok := callee.(*types.Func); ok {
				if fn.Pkg() == w.p.pkg {
					w.walk(fn)
				} else if fn.Pkg() != nil && fn.Pkg().Path() == "fmt" && fn.Name() == "Errorf" && len(x.Args) > 0 {
					if lit, ok := x.Args[0].(*ast.BasicLit); ok {
						s, _ := strconv.Unquote(lit.Value)
						w.add("errorf:"+s, Kind{Messages: []string{s}, Pos: w.p.pos(x.Pos())}, me)
					}
				}
			}
		}
		return true
	})
}

func returnsError(sig *types.Signature) bool {
	r := sig.Results()
	return r.Len() > 0 && r.At(r.Len()-1).Type().String() == "error"
}

func hasMethod(n *types.Named, name string) bool {
	ms := types.NewMethodSet(types.NewPointer(n))
	for i := 0; i < ms.Len(); i++ {
		if ms.At(i).Obj().Name() == name {
			return true
		}
	}
	return false
}

// the Error() method's Sprintf formats: all of them, or the one under `case flavour:`
func (p *pkgInfo) messages(d decls) func(*types.Named, string) []string {
	return func(n *types.Named, flavour string) []string {
		var fd *ast.FuncDecl
		for fn, decl := range d.funcs {
			if fn.Name() != "Error" {
				continue
			}
			if r := fn.Type().(*types.Signature).Recv(); r != nil && named(r.Type()) == n {
				fd = decl
			}
		}
		if fd == nil {
			return nil
		}
		var out []string
		collect := func(node ast.Node) {
			ast.Inspect(node, func(m ast.Node) bool {
				c, ok := m.(*ast.CallExpr)
				if !ok {
					return true
				}
				if sel, ok := c.Fun.(*ast.SelectorExpr); ok && sel.Sel.Name == "Sprintf" && len(c.Args) > 0 {
					if lit, ok := c.Args[0].(*ast.BasicLit); ok {
						s, _ := strconv.Unquote(lit.Value)
						if !contains(out, s) {
							out = append(out, s)
						}
					}
				}
				return true
			})
		}
		if flavour != "" {
			ast.Inspect(fd.Body, func(m ast.Node) bool {
				cc, ok := m.(*ast.CaseClause)
				if !ok {
					return true
				}
				for _, e := range cc.List {
					if id, ok := e.(*ast.Ident); ok && id.Name == flavour {
						for _, s := range cc.Body {
							collect(s)
						}
					}
				}
				return false
			})
			if len(out) > 0 {
				return out
			}
		}
		// the last Sprintf is the message; earlier ones build its parts
		collect(fd.Body)
		if len(out) > 1 {
			return out[len(out)-1:]
		}
		return out
	}
}

// the switch on the error-handling policy in fn's own body
func (p *pkgInfo) policy(fd *ast.FuncDecl) (out []Policy) {
	ast.Inspect(fd.Body, func(n ast.Node) bool {
		sw, ok := n.(*ast.SwitchStmt)
		if !ok {
			return true
		}
		for _, s := range sw.Body.List {
			cc := s.(*ast.CaseClause)
			name := ""
			for _, e := range cc.List {
				name = p.src(e)
			}
			var acts []string
			note := ""
			for _, st := range cc.Body {
				ast.Inspect(st, func(m ast.Node) bool {
					switch x := m.(type) {
					case *ast.ReturnStmt:
						acts = append(acts, "return")
					case *ast.IfStmt:
						note = p.src(x.Cond)
					case *ast.CallExpr:
						src := p.src(x.Fun)
						if src == "os.Exit" && len(x.Args) == 1 {
							acts = append(acts, "exit "+p.src(x.Args[0]))
						}
						if src == "panic" {
							acts = append(acts, "panic")
						}
					}
					return true
				})
			}
			out = append(out, Policy{Case: name, Action: strings.Join(acts, " | "), Note: note})
		}
		return false
	})
	return
}

func main() {
	pkgDir, consDir := os.Args[1], os.Args[2]
	fset := token.NewFileSet()
	std := importer.ForCompiler(fset, "source", nil)
	p := load(pkgDir, fset, std)
	c := load(consDir, fset, chained{std, map[string]*types.Package{"github.com/spf13/pflag": p.pkg}})
	d := p.decls()
	sc := p.pkg.Scope()
	errT := types.Universe.Lookup("error").Type().Underlying().(*types.Interface)

	out := Out{Pkg: p.pkg.Name(), Path: "github.com/spf13/pflag", Consumer: "github.com/spf13/cobra", Objects: map[string]Object{}, Counts: map[string]int{}}
	for _, f := range p.files {
		if f.Doc != nil {
			out.Doc = docOf(f.Doc)
		}
	}
	consName := c.pkg.Name()
	useOf := func(o types.Object) ([]Site, map[string]int) {
		s := p.uses(o, p.pkg.Name())
		cs := c.uses(o, consName)
		return append(cs, s...), map[string]int{p.pkg.Name(): len(s), consName: len(cs)}
	}

	// ---- the choice: a named type with a const block
	{
		tn := sc.Lookup("ErrorHandling").(*types.TypeName)
		spec := d.types["ErrorHandling"]
		o := Object{Name: "ErrorHandling", Kind: "type", Doc: docOf(d.typeDoc["ErrorHandling"]), Decl: "type " + p.src(spec), Pos: p.pos(spec.Pos())}
		for _, n := range sc.Names() {
			k, ok := sc.Lookup(n).(*types.Const)
			if !ok || k.Type() != tn.Type() {
				continue
			}
			v, _ := constant.Int64Val(k.Val())
			o.Consts = append(o.Consts, Const{Name: n, Value: strconv.FormatInt(v, 10), Doc: docOf(d.valDoc[n]), Pos: p.pos(k.Pos())})
		}
		sort.Slice(o.Consts, func(i, j int) bool { return o.Consts[i].Value < o.Consts[j].Value })
		o.Makers, o.Takers = p.makersTakers(tn.Type().(*types.Named), d)
		o.Uses, o.UseCount = useOf(tn)
		for _, k := range o.Consts {
			s, n := useOf(sc.Lookup(k.Name))
			o.Uses = append(o.Uses, s...)
			for key, v := range n {
				o.UseCount[key] += v
			}
		}
		out.Objects["choice"] = o
	}

	// ---- the record: a struct
	{
		tn := sc.Lookup("Flag").(*types.TypeName)
		nt := tn.Type().(*types.Named)
		spec := d.types["Flag"]
		o := Object{Name: "Flag", Kind: "struct", Doc: docOf(d.typeDoc["Flag"]), Decl: "type " + p.src(spec), Pos: p.pos(spec.Pos())}
		st := spec.Type.(*ast.StructType)
		for _, fl := range st.Fields.List {
			doc := docOf(fl.Doc)
			if doc == "" {
				doc = docOf(fl.Comment)
			}
			for _, n := range fl.Names {
				o.Fields = append(o.Fields, Field{Name: n.Name, Type: types.TypeString(p.info.Types[fl.Type].Type, qual(p.pkg)), Doc: doc, Exported: n.IsExported()})
			}
		}
		ms := types.NewMethodSet(types.NewPointer(nt))
		for i := 0; i < ms.Len(); i++ {
			if fn := ms.At(i).Obj().(*types.Func); fn.Exported() {
				o.Methods = append(o.Methods, p.method(fn, d))
			}
		}
		o.Makers, o.Takers = p.makersTakers(nt, d)
		o.Uses, o.UseCount = useOf(tn)
		out.Objects["record"] = o
	}

	// ---- the contract: an interface, and every type that satisfies it (Go never says so; we compute it)
	{
		tn := sc.Lookup("Value").(*types.TypeName)
		iface := tn.Type().Underlying().(*types.Interface)
		spec := d.types["Value"]
		o := Object{Name: "Value", Kind: "interface", Doc: docOf(d.typeDoc["Value"]), Decl: "type " + p.src(spec), Pos: p.pos(spec.Pos())}
		it := spec.Type.(*ast.InterfaceType)
		for _, m := range it.Methods.List {
			for _, n := range m.Names {
				o.Methods = append(o.Methods, Method{Name: n.Name, Sig: types.TypeString(p.info.Types[m.Type].Type, qual(p.pkg)), Doc: docOf(m.Doc), Pos: p.pos(n.Pos())})
			}
		}
		for _, pi := range []*pkgInfo{p, c} {
			s := pi.pkg.Scope()
			for _, n := range s.Names() {
				t, ok := s.Lookup(n).(*types.TypeName)
				if !ok || t.IsAlias() {
					continue
				}
				if _, isIface := t.Type().Underlying().(*types.Interface); isIface {
					continue
				}
				val := types.Implements(t.Type(), iface)
				ptr := !val && types.Implements(types.NewPointer(t.Type()), iface)
				if val || ptr {
					o.Sat = append(o.Sat, Satisfier{Name: n, Pkg: pi.pkg.Name(), Exported: t.Exported(), Ptr: ptr, Under: types.TypeString(t.Type().Underlying(), qual(pi.pkg)), Pos: pi.pos(t.Pos())})
				}
			}
		}
		o.Makers, o.Takers = p.makersTakers(tn.Type().(*types.Named), d)
		o.Uses, o.UseCount = useOf(tn)
		out.Objects["contract"] = o
		// the secondary interface, for the "also" line
		if sv, ok := sc.Lookup("SliceValue").(*types.TypeName); ok {
			svI := sv.Type().Underlying().(*types.Interface)
			n := 0
			for _, name := range sc.Names() {
				if t, ok := sc.Lookup(name).(*types.TypeName); ok && !t.IsAlias() {
					if _, isIface := t.Type().Underlying().(*types.Interface); !isIface && (types.Implements(t.Type(), svI) || types.Implements(types.NewPointer(t.Type()), svI)) {
						n++
					}
				}
			}
			out.Counts["SliceValue"] = n
		}
	}

	// ---- the callable: a method, what it can fail with, and the policy that turns failure into exit or panic
	{
		fs := sc.Lookup("FlagSet").(*types.TypeName).Type().(*types.Named)
		var parse *types.Func
		for i := 0; i < fs.NumMethods(); i++ {
			if fs.Method(i).Name() == "Parse" {
				parse = fs.Method(i)
			}
		}
		fd := d.funcs[parse]
		sig := parse.Type().(*types.Signature)
		o := Object{Name: "FlagSet.Parse", Kind: "method", Doc: docOf(fd.Doc), Decl: p.src(fd.Type), Pos: p.pos(fd.Pos())}
		o.Decl = "func (f *FlagSet) Parse" + strings.TrimPrefix(o.Decl, "func")
		o.Recv = &Field{Name: "f", Type: types.TypeString(sig.Recv().Type(), qual(p.pkg))}
		for i := 0; i < sig.Params().Len(); i++ {
			v := sig.Params().At(i)
			o.Params = append(o.Params, Field{Name: v.Name(), Type: types.TypeString(v.Type(), qual(p.pkg))})
		}
		for i := 0; i < sig.Results().Len(); i++ {
			v := sig.Results().At(i)
			o.Results = append(o.Results, Field{Name: v.Name(), Type: types.TypeString(v.Type(), qual(p.pkg))})
		}
		w := &failWalk{p: p, d: d, errT: errT, seen: map[*types.Func]bool{}, kinds: map[string]*Kind{}, msgOf: p.messages(d)}
		w.walk(parse)
		for _, k := range w.order {
			o.Kinds = append(o.Kinds, *w.kinds[k])
		}
		o.Policy = p.policy(fd)
		o.Uses, o.UseCount = useOf(parse)
		// how to get the receiver: who makes a *FlagSet
		o.Makers, _ = p.makersTakers(fs, d)
		out.Objects["callable"] = o
	}

	// package-wide counts, for the package page
	exported := 0
	for _, n := range sc.Names() {
		if sc.Lookup(n).Exported() {
			exported++
		}
	}
	out.Counts["exported"] = exported
	out.Counts["files"] = len(p.files)

	enc := json.NewEncoder(os.Stdout)
	enc.SetIndent("", " ")
	must(enc.Encode(out))
}
