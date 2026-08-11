// Doc-comment harvesting from package ASTs.
//
// go/types deliberately drops comments, so doc text is collected in a
// separate pass over the syntax trees and joined to type-checker objects
// by name. Comment text is emitted via ast.CommentGroup.Text(), which
// strips the comment markers and removes //go:-style directives — the
// Rust side receives clean doc prose and parses its structure there.
package main

import (
	"go/ast"
	"go/token"
	"strings"

	"golang.org/x/tools/go/packages"
)

// constGroupInfo describes one `const (...)` declaration block.
type constGroupInfo struct {
	// id uniquely identifies the block within its package.
	id int
	// hasIota reports whether any value expression in the block
	// mentions iota.
	hasIota bool
}

// posRange is a raw [start, end) pair of token.Pos values, resolved to byte
// offsets later by the serializer (which owns the FileSet). Kept as
// token.Pos rather than resolved eagerly because docCatalog is built before
// a serializer exists for this package.
type posRange struct {
	start, end token.Pos
}

// docCatalog maps declared names to their harvested doc comments.
type docCatalog struct {
	// packageDoc is the package comment (joined across files).
	packageDoc string
	// declDoc maps a package-level identifier to its doc comment.
	declDoc map[string]string
	// methodDoc maps "TypeName.MethodName" to the method's doc comment.
	methodDoc map[string]string
	// fieldDocs maps a struct type name to {field name -> doc comment}.
	fieldDocs map[string]map[string]string
	// ifaceMethodDocs maps an interface type name to
	// {method name -> doc comment}.
	ifaceMethodDocs map[string]map[string]string
	// constGroup maps a constant name to its declaration block.
	constGroup map[string]constGroupInfo
	// declSpan maps a package-level identifier to the token.Pos range of
	// its full declaration: for an ungrouped decl, from its leading
	// keyword (`func`/`type`/`const`/`var`) through its end; for one
	// member of a grouped decl (`const ( A; B )`), just that spec's own
	// range, mirroring the declDoc-vs-spec-doc split in harvestGenDecl —
	// the keyword documents (and spans) the group, not any one member.
	declSpan map[string]posRange
	// methodSpan maps "TypeName.MethodName" to the token.Pos range of
	// that method's full `func (recv T) Name(...) { ... }` declaration.
	methodSpan map[string]posRange
}

// harvestDocs walks every syntax file of pkg and collects doc comments
// and const-group structure.
func harvestDocs(pkg *packages.Package) *docCatalog {
	c := &docCatalog{
		declDoc:         map[string]string{},
		methodDoc:       map[string]string{},
		fieldDocs:       map[string]map[string]string{},
		ifaceMethodDocs: map[string]map[string]string{},
		constGroup:      map[string]constGroupInfo{},
		declSpan:        map[string]posRange{},
		methodSpan:      map[string]posRange{},
	}

	var pkgDocs []string
	groupID := 0

	for _, file := range pkg.Syntax {
		if file.Doc != nil {
			if text := strings.TrimSpace(file.Doc.Text()); text != "" {
				pkgDocs = append(pkgDocs, text)
			}
		}
		for _, decl := range file.Decls {
			switch decl := decl.(type) {
			case *ast.FuncDecl:
				c.harvestFuncDecl(decl)
			case *ast.GenDecl:
				groupID++
				c.harvestGenDecl(decl, groupID)
			}
		}
	}

	c.packageDoc = strings.Join(pkgDocs, "\n\n")
	return c
}

// harvestFuncDecl records a function or method doc comment, and — always,
// doc comment or not — the token.Pos range of the whole declaration
// (`func` keyword through the end of the signature or body).
func (c *docCatalog) harvestFuncDecl(decl *ast.FuncDecl) {
	span := posRange{start: decl.Pos(), end: decl.End()}
	if decl.Recv != nil && len(decl.Recv.List) > 0 {
		recvType := receiverTypeName(decl.Recv.List[0].Type)
		if recvType == "" {
			return
		}
		c.methodSpan[recvType+"."+decl.Name.Name] = span
	} else {
		c.declSpan[decl.Name.Name] = span
	}

	if decl.Doc == nil {
		return
	}
	text := strings.TrimSpace(decl.Doc.Text())
	if text == "" {
		return
	}
	if decl.Recv != nil && len(decl.Recv.List) > 0 {
		if recvType := receiverTypeName(decl.Recv.List[0].Type); recvType != "" {
			c.methodDoc[recvType+"."+decl.Name.Name] = text
		}
		return
	}
	c.declDoc[decl.Name.Name] = text
}

// harvestGenDecl records docs for type/const/var specs, struct field and
// interface method docs, and const-group membership.
func (c *docCatalog) harvestGenDecl(decl *ast.GenDecl, groupID int) {
	group := constGroupInfo{id: groupID, hasIota: false}
	if decl.Tok == token.CONST {
		group.hasIota = usesIota(decl)
	}

	// The decl-level doc only documents a spec when the declaration has
	// exactly one (go/doc convention); in a grouped block it documents
	// the group, not each member. The same split applies to the span: a
	// single `const Foo = 1` declaration's meaningful range includes the
	// `const` keyword, but in `const ( Foo = 1; Bar = 2 )` that keyword
	// (and the parens) belong to the group, not to Foo or Bar alone — so
	// each member's span is just its own spec.
	declDoc := decl.Doc
	grouped := len(decl.Specs) > 1
	if grouped {
		declDoc = nil
	}

	for _, spec := range decl.Specs {
		switch spec := spec.(type) {
		case *ast.TypeSpec:
			c.recordSpecDoc(spec.Name.Name, spec.Doc, spec.Comment, declDoc)
			c.declSpan[spec.Name.Name] = declSpecSpan(decl, spec, grouped)
			switch st := spec.Type.(type) {
			case *ast.StructType:
				c.harvestStructFields(spec.Name.Name, st)
			case *ast.InterfaceType:
				c.harvestInterfaceMethods(spec.Name.Name, st)
			}

		case *ast.ValueSpec:
			// A ValueSpec can bind several names to one line (`var a, b
			// int`); the whole-spec span would then be identical for both
			// and could not distinguish them, so multi-name specs record
			// each identifier's own (narrower, but distinct) range instead.
			shared := declSpecSpan(decl, spec, grouped)
			for _, name := range spec.Names {
				c.recordSpecDoc(name.Name, spec.Doc, spec.Comment, declDoc)
				if len(spec.Names) > 1 {
					c.declSpan[name.Name] = posRange{start: name.Pos(), end: name.End()}
				} else {
					c.declSpan[name.Name] = shared
				}
				if decl.Tok == token.CONST {
					c.constGroup[name.Name] = group
				}
			}
		}
	}
}

// declSpecSpan returns the token.Pos range documenting one spec of a
// GenDecl: the whole declaration (keyword through end) when it is the
// declaration's only spec, or just the spec's own range when the
// declaration is grouped and the keyword covers siblings too.
func declSpecSpan(decl *ast.GenDecl, spec ast.Spec, grouped bool) posRange {
	if grouped {
		return posRange{start: spec.Pos(), end: spec.End()}
	}
	return posRange{start: decl.Pos(), end: decl.End()}
}

// recordSpecDoc stores the best available doc for a spec'd name: the
// spec's own doc comment, else its trailing line comment, else — for a
// single-spec declaration — the decl-level doc.
func (c *docCatalog) recordSpecDoc(name string, doc, comment, declDoc *ast.CommentGroup) {
	for _, cg := range []*ast.CommentGroup{doc, comment, declDoc} {
		if cg == nil {
			continue
		}
		if text := strings.TrimSpace(cg.Text()); text != "" {
			c.declDoc[name] = text
			return
		}
	}
}

// harvestStructFields collects per-field docs for a named struct type.
func (c *docCatalog) harvestStructFields(typeName string, st *ast.StructType) {
	if st.Fields == nil {
		return
	}
	docs := map[string]string{}
	for _, field := range st.Fields.List {
		text := fieldDocText(field)
		if text == "" {
			continue
		}
		if len(field.Names) == 0 {
			// Embedded field: keyed by its implicit name.
			if name := embeddedFieldName(field.Type); name != "" {
				docs[name] = text
			}
			continue
		}
		for _, name := range field.Names {
			docs[name.Name] = text
		}
	}
	if len(docs) > 0 {
		c.fieldDocs[typeName] = docs
	}
}

// harvestInterfaceMethods collects per-method docs for a named interface.
func (c *docCatalog) harvestInterfaceMethods(typeName string, it *ast.InterfaceType) {
	if it.Methods == nil {
		return
	}
	docs := map[string]string{}
	for _, field := range it.Methods.List {
		text := fieldDocText(field)
		if text == "" {
			continue
		}
		for _, name := range field.Names {
			docs[name.Name] = text
		}
	}
	if len(docs) > 0 {
		c.ifaceMethodDocs[typeName] = docs
	}
}

// fieldDocText prefers a field's doc comment over its trailing comment.
func fieldDocText(field *ast.Field) string {
	if field.Doc != nil {
		if text := strings.TrimSpace(field.Doc.Text()); text != "" {
			return text
		}
	}
	if field.Comment != nil {
		return strings.TrimSpace(field.Comment.Text())
	}
	return ""
}

// receiverTypeName extracts the bare type name from a receiver
// expression: `T`, `*T`, `T[P]`, `*T[P, Q]` all yield "T".
func receiverTypeName(expr ast.Expr) string {
	switch expr := expr.(type) {
	case *ast.StarExpr:
		return receiverTypeName(expr.X)
	case *ast.IndexExpr:
		return receiverTypeName(expr.X)
	case *ast.IndexListExpr:
		return receiverTypeName(expr.X)
	case *ast.Ident:
		return expr.Name
	default:
		return ""
	}
}

// embeddedFieldName resolves the implicit field name of an embedded
// field expression: `T`, `*T`, `pkg.T`, `*pkg.T`, and generic forms all
// yield "T".
func embeddedFieldName(expr ast.Expr) string {
	switch expr := expr.(type) {
	case *ast.StarExpr:
		return embeddedFieldName(expr.X)
	case *ast.IndexExpr:
		return embeddedFieldName(expr.X)
	case *ast.IndexListExpr:
		return embeddedFieldName(expr.X)
	case *ast.SelectorExpr:
		return expr.Sel.Name
	case *ast.Ident:
		return expr.Name
	default:
		return ""
	}
}

// usesIota reports whether any value expression in a const decl mentions
// the predeclared iota.
func usesIota(decl *ast.GenDecl) bool {
	found := false
	for _, spec := range decl.Specs {
		vs, ok := spec.(*ast.ValueSpec)
		if !ok {
			continue
		}
		for _, value := range vs.Values {
			ast.Inspect(value, func(n ast.Node) bool {
				if ident, ok := n.(*ast.Ident); ok && ident.Name == "iota" {
					found = true
					return false
				}
				return !found
			})
		}
		if vs.Type != nil {
			// `iota` can also appear in array lengths etc.; values are
			// the conventional carrier, so this is intentionally narrow.
			continue
		}
	}
	return found
}
