// Emits the fixed Go authority image (format version 5) directly from
// go/packages and go/types facts.
//
// The image carries the complete Output: module metadata; one row per
// package with its import path, package clause, source files, and contiguous
// declaration run; declarations with exact constant values, const groups,
// and iota flags; the recursive type graph as flat type rows over a pooled
// child plane; the exact source name and source position of every func
// parameter and result; methods
// (declared and promoted); generic type parameters; struct fields, interface
// method signatures, and the complete post-embedding interface method set;
// documentation rows (declarations, methods, members, and whole packages);
// resolved call references; build-constraint exclusions; and interface
// satisfaction edges.
//
// The binary layout is the single semantic transport: JSON is not an IR
// boundary. The Rust reader in compiler/languages/go/image.rs owns the
// format contract; every plane ordering, tiling, and canonical-sort law it
// validates is produced directly here.
//
// The image binds to one caller-selected source file with a SHA-256 digest
// so the compiler can prove the authority describes exactly the bytes it is
// compiling.
package main

import (
	"bytes"
	"crypto/sha256"
	"encoding/binary"
	"fmt"
	"io"
	"os"
	"sort"
	"strings"
)

// Image format constants. Keep in lockstep with compiler/languages/go/image.rs.
const (
	authorityVersion     = 5
	authorityHeaderBytes = 136

	authorityModuleBytes    = 32
	authorityPackageBytes   = 28
	authorityDeclBytes      = 56
	authorityTypeBytes      = 52
	authoritySigParamBytes  = 28
	authorityMethodBytes    = 64
	authorityParamBytes     = 16
	authorityMemberBytes    = 40
	authorityMethodSetBytes = 24
	authorityDocBytes       = 16
	authorityRefBytes       = 48
	authorityConBytes       = 28
	authoritySatBytes       = 20
	authorityChildBytes     = 8

	authorityNone = uint32(^uint32(0))
	authorityMax  = uint64(^uint32(0))
)

var authorityDigestDomain = []byte("nudox.go.authority.image.sha256.v5\x00")

// atomCell locates one byte run in the shared atom plane.
type atomCell struct {
	offset uint32
	length uint32
}

// modulePlan is the single module-metadata row.
type modulePlan struct {
	path      atomCell
	dir       atomCell
	goVersion atomCell
	version   atomCell
}

// packagePlan is one package row: identity and source files.
type packagePlan struct {
	importPath atomCell
	name       atomCell
	files      atomCell
	fileCount  uint32
}

// sigParamPlan is one signature-parameter row: the exact source name and
// source position of one parameter or result of one func type row.
type sigParamPlan struct {
	owner   uint32
	ordinal uint32
	name    atomCell
	file    atomCell
	offset  uint32
}

// methodSetPlan is one interface method-set row: one method of an
// interface type row's complete post-embedding method set.
type methodSetPlan struct {
	owner   uint32
	name    atomCell
	sigRoot uint32
	pkg     atomCell
}

// declPlan is one package-level declaration row.
type declPlan struct {
	kind       byte
	exported   bool
	iota       bool
	name       atomCell
	pkg        atomCell
	typeRoot   uint32
	spanStart  uint32
	spanEnd    uint32
	file       atomCell
	value      atomCell
	constGroup int64
}

// typePlan is one recursive type-graph row.
type typePlan struct {
	kind        byte
	dir         byte
	variadic    bool
	name        atomCell
	pkg         atomCell
	length      int64
	paramCount  uint32
	childStart  uint32
	childCount  uint32
	memberStart uint32
	memberCount uint32
}

// childPlan is one pooled child cell: a type-row target plus term flags.
type childPlan struct {
	target uint32
	flags  uint32
}

// methodPlan is one method row (declared or promoted).
type methodPlan struct {
	owner          uint32
	exported       bool
	pointerRecv    bool
	promoted       bool
	name           atomCell
	typeRoot       uint32
	receiver       atomCell
	recvParams     atomCell
	recvParamCount uint32
	origin         atomCell
	spanStart      uint32
	spanEnd        uint32
	file           atomCell
}

// paramPlan is one generic type-parameter row.
type paramPlan struct {
	owner      uint32
	name       atomCell
	constraint uint32
}

// memberRequest is one struct field or interface method signature awaiting
// its member-plane layout, carrying the documentation the declaration
// recorded for the member by name.
type memberRequest struct {
	plan memberPlan
	doc  string
}

// memberPlan is one laid-out member row.
type memberPlan struct {
	owner    uint32
	kind     byte // 0 field, 1 interface method signature
	embedded bool
	exported bool
	name     atomCell
	typeRoot uint32
	tag      atomCell
	pkg      atomCell
}

// docPlan is one documentation row.
type docPlan struct {
	ownerKind byte // 0 declaration, 1 method, 2 member, 3 package
	owner     uint32
	text      atomCell
}

// refPlan is one resolved call reference.
type refPlan struct {
	owner     uint32
	target    atomCell
	targetPkg atomCell
	start     uint32
	end       uint32
	file      atomCell
	receiver  atomCell
}

// conPlan is one build-constraint exclusion row.
type conPlan struct {
	file       atomCell
	constraint atomCell
	blob       atomCell
	count      uint32
}

// satPlan is one interface-satisfaction edge.
type satPlan struct {
	subject   uint32
	target    atomCell
	targetPkg atomCell
}

// imagePlan accumulates every plane before the single fixed-layout marshal.
// Rows stay structured until marshal so planes can be laid out in the exact
// canonical orders the reader validates.
type imagePlan struct {
	module     modulePlan
	packages   []packagePlan
	atoms      []byte
	decls      []declPlan
	types      []typePlan
	sigParams  []sigParamPlan
	children   []childPlan
	methods    []methodPlan
	params     []paramPlan
	members    []memberPlan
	methodSets []methodSetPlan
	docs       []docPlan
	refs       []refPlan
	cons       []conPlan
	sats       []satPlan

	// memberRequests stashes each struct/interface row's member rows until
	// finalize lays the member plane out in type-row order. Nested anonymous
	// struct fields emit their inner members during the outer row's
	// recursion, so member rows cannot be appended inline without breaking
	// the owner-contiguity law the reader proves.
	memberRequests [][]memberRequest

	// packageDocs holds the packages' doc comments; they sort after member
	// documentation rows, whose row indices only exist after finalize.
	packageDocs []docPlan

	// declarationDocs and methodDocs hold rows whose owner indices exist
	// immediately, but documentation rows must be globally sorted by owner
	// kind, so every bucket is appended in kind order during finalize.
	declarationDocs []docPlan
	methodDocs      []docPlan
	memberDocs      []docPlan
}

// u32 converts a length that must fit the u32 cell width.
func u32(length int, what string) (uint32, error) {
	if uint64(length) > authorityMax {
		return 0, fmt.Errorf("Go authority image %s exceeds u32 capacity: %d", what, length)
	}
	return uint32(length), nil
}

// nulBlob renders names as the reader's NUL-terminated name blob: every
// name is followed by one 0x00 byte, so an empty blob holds exactly zero
// names and the reader's splitter can never accept a dangling tail.
func nulBlob(names []string) string {
	var blob strings.Builder
	for _, name := range names {
		blob.WriteString(name)
		blob.WriteByte(0)
	}
	return blob.String()
}

// atom interns one UTF-8 byte run and returns its plane cell.
func (p *imagePlan) atom(s string) (atomCell, error) {
	offset, err := u32(len(p.atoms), "atom plane")
	if err != nil {
		return atomCell{}, err
	}
	p.atoms = append(p.atoms, s...)
	length, err := u32(len(s), "atom")
	if err != nil {
		return atomCell{}, err
	}
	return atomCell{offset: offset, length: length}, nil
}

// growMembers extends the per-type-row member stash through index.
func (p *imagePlan) growMembers(index uint32) {
	for uint32(len(p.memberRequests)) <= index {
		p.memberRequests = append(p.memberRequests, nil)
	}
}

// reserveChildren appends count placeholder child cells and returns the run
// start. The caller fills each placeholder after emitting the child subtree,
// which keeps every parent's run contiguous while subtrees append beyond it.
func (p *imagePlan) reserveChildren(count int) (uint32, error) {
	start, err := u32(len(p.children), "child plane")
	if err != nil {
		return 0, err
	}
	for i := 0; i < count; i++ {
		p.children = append(p.children, childPlan{})
	}
	return start, nil
}

// emitType lowers one recursive Type tree into pre-order type rows and
// returns the root row index. The root row is always allocated before its
// subtree rows; every write back into the root row re-indexes the plane
// because subtree emission may reallocate it. Structural cycles cannot
// occur: the oracle serializes named types by reference and never expands
// them inline.
func (p *imagePlan) emitType(t *Type) (uint32, error) {
	if t == nil {
		return 0, fmt.Errorf("go/types emitted a missing type where one is required")
	}
	var kind byte
	switch t.Kind {
	case "basic":
		kind = 0
	case "named":
		kind = 1
	case "alias":
		kind = 2
	case "typeParam":
		kind = 3
	case "pointer":
		kind = 4
	case "slice":
		kind = 5
	case "array":
		kind = 6
	case "map":
		kind = 7
	case "chan":
		kind = 8
	case "func":
		kind = 9
	case "struct":
		kind = 10
	case "interface":
		kind = 11
	case "union":
		kind = 12
	case "tuple":
		kind = 13
	case "invalid":
		kind = 14
	default:
		return 0, fmt.Errorf("go/types emitted unknown type kind %q", t.Kind)
	}
	if len(p.types) >= int(authorityMax) {
		return 0, fmt.Errorf("Go authority image type plane exceeds u32 capacity")
	}
	row := typePlan{
		kind:       kind,
		childStart: uint32(len(p.children)),
	}
	// Name-bearing rows; every other kind must leave the name empty.
	switch kind {
	case 0, 1, 2, 3:
		if t.Name == "" {
			return 0, fmt.Errorf("go/types emitted a nameless type of kind %q", t.Kind)
		}
		name, err := p.atom(t.Name)
		if err != nil {
			return 0, err
		}
		row.name = name
		if kind == 1 || kind == 2 {
			pkg, err := p.atom(t.Pkg)
			if err != nil {
				return 0, err
			}
			row.pkg = pkg
		}
	default:
		if t.Name != "" {
			return 0, fmt.Errorf("go/types emitted kind %q carrying a name %q", t.Kind, t.Name)
		}
	}
	index := uint32(len(p.types))
	p.types = append(p.types, row)

	switch kind {
	case 1, 2: // named, alias: children are the instantiation arguments
		count := len(t.TypeArgs)
		start, err := p.reserveChildren(count)
		if err != nil {
			return 0, err
		}
		for i, arg := range t.TypeArgs {
			target, err := p.emitType(arg)
			if err != nil {
				return 0, err
			}
			p.children[int(start)+i].target = target
		}
		cellCount, err := u32(count, "type arguments")
		if err != nil {
			return 0, err
		}
		p.types[index].childStart = start
		p.types[index].childCount = cellCount
	case 4, 5, 8: // pointer, slice, chan: exactly one element child
		if t.Elem == nil {
			return 0, fmt.Errorf("go/types emitted kind %q without an element type", t.Kind)
		}
		start, err := p.reserveChildren(1)
		if err != nil {
			return 0, err
		}
		target, err := p.emitType(t.Elem)
		if err != nil {
			return 0, err
		}
		p.children[int(start)].target = target
		p.types[index].childStart = start
		p.types[index].childCount = 1
		if kind == 8 {
			switch t.Dir {
			case "send":
				p.types[index].dir = 1
			case "recv":
				p.types[index].dir = 2
			case "", "both":
				p.types[index].dir = 0
			default:
				return 0, fmt.Errorf("go/types emitted unknown channel direction %q", t.Dir)
			}
		}
	case 6: // array: one element child plus the fixed length
		if t.Elem == nil {
			return 0, fmt.Errorf("go/types emitted an array without an element type")
		}
		if t.Len < 0 {
			return 0, fmt.Errorf("go/types emitted array length %d", t.Len)
		}
		start, err := p.reserveChildren(1)
		if err != nil {
			return 0, err
		}
		target, err := p.emitType(t.Elem)
		if err != nil {
			return 0, err
		}
		p.children[int(start)].target = target
		p.types[index].childStart = start
		p.types[index].childCount = 1
		p.types[index].length = t.Len
	case 7: // map: children are exactly [key, value]
		if t.Key == nil || t.Value == nil {
			return 0, fmt.Errorf("go/types emitted a map without a key or value type")
		}
		start, err := p.reserveChildren(2)
		if err != nil {
			return 0, err
		}
		key, err := p.emitType(t.Key)
		if err != nil {
			return 0, err
		}
		value, err := p.emitType(t.Value)
		if err != nil {
			return 0, err
		}
		p.children[int(start)].target = key
		p.children[int(start)+1].target = value
		p.types[index].childStart = start
		p.types[index].childCount = 2
	case 9: // func: children are parameters followed by results
		// The row's signature-parameter rows come first so owner runs stay
		// contiguous in type-row order: parameters, then results, in child
		// order. Unnamed parameters and results carry empty name atoms, and
		// an unresolved position is fully absent (empty file, NONE offset).
		for ordinal, parameter := range t.Params {
			if parameter == nil {
				return 0, fmt.Errorf("go/types emitted a missing func parameter")
			}
			if err := p.emitSigParam(index, uint32(ordinal), parameter); err != nil {
				return 0, err
			}
		}
		for ordinal, result := range t.Results {
			if result == nil {
				return 0, fmt.Errorf("go/types emitted a missing func result")
			}
			if err := p.emitSigParam(index, uint32(len(t.Params)+ordinal), result); err != nil {
				return 0, err
			}
		}
		count := len(t.Params) + len(t.Results)
		start, err := p.reserveChildren(count)
		if err != nil {
			return 0, err
		}
		for i, param := range t.Params {
			if param == nil {
				return 0, fmt.Errorf("go/types emitted a missing func parameter")
			}
			target, err := p.emitType(param.Type)
			if err != nil {
				return 0, err
			}
			p.children[int(start)+i].target = target
		}
		for i, result := range t.Results {
			if result == nil {
				return 0, fmt.Errorf("go/types emitted a missing func result")
			}
			target, err := p.emitType(result.Type)
			if err != nil {
				return 0, err
			}
			p.children[int(start)+len(t.Params)+i].target = target
		}
		cellCount, err := u32(count, "func parameters and results")
		if err != nil {
			return 0, err
		}
		paramCount, err := u32(len(t.Params), "func parameters")
		if err != nil {
			return 0, err
		}
		p.types[index].childStart = start
		p.types[index].childCount = cellCount
		p.types[index].paramCount = paramCount
		p.types[index].variadic = t.Variadic
	case 10: // struct: fields become member rows finalized in row order
		if len(t.Fields) > 0 {
			requests := make([]memberRequest, 0, len(t.Fields))
			for _, field := range t.Fields {
				if field == nil {
					return 0, fmt.Errorf("go/types emitted a missing struct field")
				}
				root, err := p.emitType(field.Type)
				if err != nil {
					return 0, err
				}
				name, err := p.atom(field.Name)
				if err != nil {
					return 0, err
				}
				tag, err := p.atom(field.Tag)
				if err != nil {
					return 0, err
				}
				requests = append(requests, memberRequest{
					plan: memberPlan{
						kind:     0,
						embedded: field.Embedded,
						exported: field.Exported,
						name:     name,
						typeRoot: root,
						tag:      tag,
					},
					doc: "",
				})
			}
			p.growMembers(index)
			p.memberRequests[index] = requests
		}
	case 11: // interface: children are embeddeds, members the method signatures
		// The embeddeds' child run is reserved before anything recurses, so
		// this row's child run stays contiguous at its type-row position.
		start, err := p.reserveChildren(len(t.Embeddeds))
		if err != nil {
			return 0, err
		}
		// The complete post-embedding method set is a carried fact: when an
		// embedded interface declares in another package, its methods cannot
		// be re-expanded locally because named references carry no body.
		// Rows are emitted here, inside this row's case, so owner runs stay
		// contiguous in type-row order; go/types already returns the set
		// sorted by unique method name.
		for _, method := range t.AllMethods {
			if method == nil || method.Name == "" {
				return 0, fmt.Errorf("go/types emitted a missing interface method-set entry")
			}
			name, err := p.atom(method.Name)
			if err != nil {
				return 0, err
			}
			pkg, err := p.atom(method.Pkg)
			if err != nil {
				return 0, err
			}
			root := authorityNone
			if method.Signature != nil {
				if root, err = p.emitType(method.Signature); err != nil {
					return 0, err
				}
			}
			p.methodSets = append(p.methodSets, methodSetPlan{
				owner:   index,
				name:    name,
				sigRoot: root,
				pkg:     pkg,
			})
		}
		for i, embedded := range t.Embeddeds {
			target, err := p.emitType(embedded)
			if err != nil {
				return 0, err
			}
			p.children[int(start)+i].target = target
		}
		cellCount, err := u32(len(t.Embeddeds), "interface embeddeds")
		if err != nil {
			return 0, err
		}
		p.types[index].childStart = start
		p.types[index].childCount = cellCount
		if len(t.ExplicitMethods) > 0 {
			requests := make([]memberRequest, 0, len(t.ExplicitMethods))
			for _, method := range t.ExplicitMethods {
				if method == nil {
					return 0, fmt.Errorf("go/types emitted a missing interface method")
				}
				root, err := p.emitType(method.Signature)
				if err != nil {
					return 0, err
				}
				name, err := p.atom(method.Name)
				if err != nil {
					return 0, err
				}
				pkg, err := p.atom(method.Pkg)
				if err != nil {
					return 0, err
				}
				requests = append(requests, memberRequest{
					plan: memberPlan{
						kind:     1,
						exported: method.Exported,
						name:     name,
						typeRoot: root,
						pkg:      pkg,
					},
					doc: "",
				})
			}
			p.growMembers(index)
			p.memberRequests[index] = requests
		}
	case 12: // union: children are the term types, tilde flags on the cells
		start, err := p.reserveChildren(len(t.Terms))
		if err != nil {
			return 0, err
		}
		for i, term := range t.Terms {
			if term == nil {
				return 0, fmt.Errorf("go/types emitted a missing union term")
			}
			target, err := p.emitType(term.Type)
			if err != nil {
				return 0, err
			}
			p.children[int(start)+i].target = target
			if term.Tilde {
				p.children[int(start)+i].flags = 1
			}
		}
		cellCount, err := u32(len(t.Terms), "union terms")
		if err != nil {
			return 0, err
		}
		p.types[index].childStart = start
		p.types[index].childCount = cellCount
	case 13: // tuple: children are the component types
		start, err := p.reserveChildren(len(t.Types))
		if err != nil {
			return 0, err
		}
		for i, component := range t.Types {
			target, err := p.emitType(component)
			if err != nil {
				return 0, err
			}
			p.children[int(start)+i].target = target
		}
		cellCount, err := u32(len(t.Types), "tuple components")
		if err != nil {
			return 0, err
		}
		p.types[index].childStart = start
		p.types[index].childCount = cellCount
	}
	return index, nil
}

// emitSigParam lowers one parameter/result's signature-parameter row: the
// exact source name plus the identifier's file and byte offset, or a fully
// absent position when go/types resolved none.
func (p *imagePlan) emitSigParam(owner uint32, ordinal uint32, parameter *Param) error {
	name, err := p.atom(parameter.Name)
	if err != nil {
		return err
	}
	file := atomCell{}
	offset := authorityNone
	if parameter.Pos != nil {
		if file, err = p.atom(parameter.Pos.File); err != nil {
			return err
		}
		if parameter.Pos.Offset < 0 || uint64(parameter.Pos.Offset) > authorityMax {
			return fmt.Errorf("go/types emitted parameter offset %d", parameter.Pos.Offset)
		}
		offset = uint32(parameter.Pos.Offset)
	}
	p.sigParams = append(p.sigParams, sigParamPlan{
		owner:   owner,
		ordinal: ordinal,
		name:    name,
		file:    file,
		offset:  offset,
	})
	return nil
}

// imageDeclarationKind maps one Decl kind spelling onto its closed byte tag.
func imageDeclarationKind(kind string) (byte, error) {
	switch kind {
	case "type":
		return 1, nil
	case "alias":
		return 2, nil
	case "func":
		return 3, nil
	case "const":
		return 4, nil
	case "var":
		return 5, nil
	default:
		return 0, fmt.Errorf("go/types emitted unknown declaration kind %q", kind)
	}
}

// emitMethods lowers one type declaration's declared and promoted methods,
// owner-ordered by construction.
func (p *imagePlan) emitMethods(declaration *Decl, owner uint32) error {
	for _, method := range declaration.Methods {
		if err := p.emitMethod(method, owner, false); err != nil {
			return err
		}
	}
	for _, method := range declaration.PromotedMethods {
		if err := p.emitMethod(method, owner, true); err != nil {
			return err
		}
	}
	return nil
}

// emitMethod lowers one method row and its documentation.
func (p *imagePlan) emitMethod(method *Method, owner uint32, promoted bool) error {
	if method == nil || method.Name == "" {
		return fmt.Errorf("go/types emitted a missing method name")
	}
	for _, name := range method.RecvTypeParams {
		if name == "" {
			return fmt.Errorf(
				"go/types emitted an empty receiver type-parameter name on %s",
				method.Name)
		}
	}
	name, err := p.atom(method.Name)
	if err != nil {
		return err
	}
	receiver, err := p.atom(method.RecvName)
	if err != nil {
		return err
	}
	origin, err := p.atom(method.Origin)
	if err != nil {
		return err
	}
	blob, err := p.atom(nulBlob(method.RecvTypeParams))
	if err != nil {
		return err
	}
	count, err := u32(len(method.RecvTypeParams), "receiver type parameters")
	if err != nil {
		return err
	}
	root := authorityNone
	if method.Signature != nil {
		if root, err = p.emitType(method.Signature); err != nil {
			return err
		}
	}
	file := atomCell{}
	if method.Pos != nil {
		if file, err = p.atom(method.Pos.File); err != nil {
			return err
		}
	}
	spanStart, spanEnd := authorityNone, authorityNone
	if method.Span != nil {
		start, err := u32(method.Span.Start, "method span")
		if err != nil {
			return err
		}
		end, err := u32(method.Span.End, "method span")
		if err != nil {
			return err
		}
		if start > end {
			return fmt.Errorf("go/types emitted inverted method span %d..%d for %s",
				start, end, method.Name)
		}
		spanStart, spanEnd = start, end
	}
	p.methods = append(p.methods, methodPlan{
		owner:          owner,
		exported:       method.Exported,
		pointerRecv:    method.PointerRecv,
		promoted:       promoted,
		name:           name,
		typeRoot:       root,
		receiver:       receiver,
		recvParams:     blob,
		recvParamCount: count,
		origin:         origin,
		spanStart:      spanStart,
		spanEnd:        spanEnd,
		file:           file,
	})
	if method.Doc != "" {
		text, err := p.atom(method.Doc)
		if err != nil {
			return err
		}
		p.methodDocs = append(p.methodDocs, docPlan{
			ownerKind: 1,
			owner:     uint32(len(p.methods) - 1),
			text:      text,
		})
	}
	return nil
}

// buildAuthorityPlan flattens the complete Output into ordered planes. Every
// plane emerges in the canonical order the Rust reader validates: methods and
// type parameters sorted by owner declaration, documentation rows sorted by
// (owner kind, owner), references sorted by (file, start), constraints sorted
// by file, satisfaction rows sorted by subject.
func buildAuthorityPlan(output *Output) (*imagePlan, error) {
	p := &imagePlan{}
	// The single module-metadata row; every cell stays empty when the
	// oracle resolved no module (e.g. GOPATH-mode analysis).
	if output.Module != nil {
		if output.Module.Path == "" {
			return nil, fmt.Errorf("go/types emitted a module with an empty path")
		}
		var err error
		if p.module.path, err = p.atom(output.Module.Path); err != nil {
			return nil, err
		}
		if p.module.dir, err = p.atom(output.Module.Dir); err != nil {
			return nil, err
		}
		if p.module.goVersion, err = p.atom(output.Module.GoVersion); err != nil {
			return nil, err
		}
		if p.module.version, err = p.atom(output.Module.Version); err != nil {
			return nil, err
		}
	}
	for _, pkg := range output.Packages {
		if pkg == nil {
			return nil, fmt.Errorf("go/types emitted a missing package")
		}
		// The reader validates strict import-path order and that every
		// declaration names one of the package rows' import paths, so the
		// producer refuses out-of-order or duplicate paths instead of
		// emitting an image the reader must reject.
		if len(p.packages) > 0 {
			previous := p.packages[len(p.packages)-1]
			if string(p.atoms[previous.importPath.offset:previous.importPath.offset+previous.importPath.length]) >= pkg.ImportPath {
				return nil, fmt.Errorf(
					"go/packages emitted packages out of import-path order: %s after %s",
					pkg.ImportPath,
					string(p.atoms[previous.importPath.offset:previous.importPath.offset+previous.importPath.length]))
			}
		}
		packageRow := packagePlan{}
		var err error
		if packageRow.importPath, err = p.atom(pkg.ImportPath); err != nil {
			return nil, err
		}
		if packageRow.name, err = p.atom(pkg.Name); err != nil {
			return nil, err
		}
		if packageRow.files, err = p.atom(nulBlob(pkg.Files)); err != nil {
			return nil, err
		}
		if packageRow.fileCount, err = u32(len(pkg.Files), "package files"); err != nil {
			return nil, err
		}
		p.packages = append(p.packages, packageRow)
		// Local name resolution for reference owners (functions and receiver
		// types both declare in this package's scope).
		local := make(map[string]uint32, len(pkg.Decls))
		firstIndex := -1
		for _, declaration := range pkg.Decls {
			if declaration == nil {
				return nil, fmt.Errorf("go/types emitted a missing declaration")
			}
			if declaration.Name == "" {
				return nil, fmt.Errorf("go/types emitted an empty declaration name")
			}
			kind, err := imageDeclarationKind(declaration.Kind)
			if err != nil {
				return nil, err
			}
			local[declaration.Name] = uint32(len(p.decls))
			if firstIndex < 0 {
				firstIndex = len(p.decls)
			}
			plan := declPlan{kind: kind, exported: declaration.Exported}
			name, err := p.atom(declaration.Name)
			if err != nil {
				return nil, err
			}
			plan.name = name
			pkgCell, err := p.atom(pkg.ImportPath)
			if err != nil {
				return nil, err
			}
			plan.pkg = pkgCell
			value, err := p.atom(declaration.Value)
			if err != nil {
				return nil, err
			}
			plan.value = value
			plan.constGroup = int64(declaration.ConstGroup)
			plan.iota = declaration.GroupHasIota
			if declaration.Pos != nil {
				file, err := p.atom(declaration.Pos.File)
				if err != nil {
					return nil, err
				}
				plan.file = file
			}
			if declaration.Span != nil {
				start, err := u32(declaration.Span.Start, "declaration span")
				if err != nil {
					return nil, err
				}
				end, err := u32(declaration.Span.End, "declaration span")
				if err != nil {
					return nil, err
				}
				if start > end {
					return nil, fmt.Errorf(
						"go/types emitted inverted declaration span %d..%d for %s",
						start, end, declaration.Name)
				}
				plan.spanStart, plan.spanEnd = start, end
			} else {
				plan.spanStart, plan.spanEnd = authorityNone, authorityNone
			}
			// The declaration's type root by closed kind.
			root := authorityNone
			switch declaration.Kind {
			case "type":
				root, err = p.emitType(declaration.Underlying)
			case "alias":
				root, err = p.emitType(declaration.Target)
			case "func":
				root, err = p.emitType(declaration.Signature)
			case "const", "var":
				root, err = p.emitType(declaration.Type)
			}
			if err != nil {
				return nil, err
			}
			plan.typeRoot = root
			owner := uint32(len(p.decls))
			// Generic type parameters, owner-ordered by construction.
			for _, param := range declaration.TypeParams {
				if param == nil || param.Name == "" {
					return nil, fmt.Errorf(
						"go/types emitted a missing type-parameter name on %s",
						declaration.Name)
				}
				name, err := p.atom(param.Name)
				if err != nil {
					return nil, err
				}
				constraint := authorityNone
				if param.Constraint != nil {
					if constraint, err = p.emitType(param.Constraint); err != nil {
						return nil, err
					}
				}
				p.params = append(p.params, paramPlan{
					owner:      owner,
					name:       name,
					constraint: constraint,
				})
			}
			// Methods: declared first, then promoted.
			if err := p.emitMethods(declaration, owner); err != nil {
				return nil, err
			}
			// Documentation: the declaration's own comment.
			if declaration.Doc != "" {
				text, err := p.atom(declaration.Doc)
				if err != nil {
					return nil, err
				}
				p.declarationDocs = append(p.declarationDocs, docPlan{
					ownerKind: 0,
					owner:     owner,
					text:      text,
				})
			}
			// Interface-satisfaction edges, subject-ordered by construction.
			// The oracle populates Implements only for kind "type"
			// declarations that are not themselves interfaces.
			for _, target := range declaration.Implements {
				if target == nil || target.Name == "" {
					return nil, fmt.Errorf(
						"go/types emitted a missing satisfaction target on %s",
						declaration.Name)
				}
				name, err := p.atom(target.Name)
				if err != nil {
					return nil, err
				}
				targetPkg, err := p.atom(target.Pkg)
				if err != nil {
					return nil, err
				}
				p.sats = append(p.sats, satPlan{
					subject:   owner,
					target:    name,
					targetPkg: targetPkg,
				})
			}
			// Struct-field and interface-method documentation rides on the
			// member requests so finalize can bind it to member row indices.
			// The requests are stashed by TYPE-ROW index, so the lookup uses
			// this declaration's underlying-type root row, not its own
			// declaration index.
			if declaration.Kind == "type" && root < uint32(len(p.memberRequests)) &&
				p.memberRequests[root] != nil {
				for i := range p.memberRequests[root] {
					request := &p.memberRequests[root][i]
					name := string(p.atoms[request.plan.name.offset : request.plan.name.offset+request.plan.name.length])
					if request.plan.kind == 0 {
						request.doc = declaration.FieldDocs[name]
					} else {
						request.doc = declaration.MethodDocs[name]
					}
				}
			}
			p.decls = append(p.decls, plan)
		}
		// The package's doc comment; the owner names the package's first
		// declaration, so a declaration-less package carries none. These rows
		// are appended after finalize lays out member documentation because
		// package rows sort last by owner kind.
		if pkg.Doc != "" && firstIndex >= 0 {
			text, err := p.atom(pkg.Doc)
			if err != nil {
				return nil, err
			}
			p.packageDocs = append(p.packageDocs, docPlan{
				ownerKind: 3,
				owner:     uint32(firstIndex),
				text:      text,
			})
		}
		// Resolved call references, owner-resolved within this package.
		for _, reference := range pkg.References {
			if reference == nil {
				return nil, fmt.Errorf("go/types emitted a missing reference")
			}
			owner, ok := local[reference.Owner]
			if !ok {
				return nil, fmt.Errorf(
					"go/types emitted a reference from undeclared owner %q",
					reference.Owner)
			}
			if reference.OwnerRecv != "" {
				receiver, ok := local[reference.OwnerRecv]
				if !ok {
					return nil, fmt.Errorf(
						"go/types emitted a reference from undeclared receiver %q",
						reference.OwnerRecv)
				}
				owner = receiver
			}
			target, err := p.atom(reference.Target)
			if err != nil {
				return nil, err
			}
			targetPkg, err := p.atom(reference.TargetPkg)
			if err != nil {
				return nil, err
			}
			file, err := p.atom(reference.File)
			if err != nil {
				return nil, err
			}
			receiver, err := p.atom(reference.OwnerRecv)
			if err != nil {
				return nil, err
			}
			start, err := u32(reference.Start, "reference span")
			if err != nil {
				return nil, err
			}
			end, err := u32(reference.End, "reference span")
			if err != nil {
				return nil, err
			}
			p.refs = append(p.refs, refPlan{
				owner:     owner,
				target:    target,
				targetPkg: targetPkg,
				start:     start,
				end:       end,
				file:      file,
				receiver:  receiver,
			})
		}
		// Build-constraint exclusions with their exported-declaration blob.
		for _, constraint := range pkg.BuildConstraints {
			if constraint == nil || constraint.File == "" {
				return nil, fmt.Errorf("go/types emitted a missing build-constraint file")
			}
			if len(constraint.Constraints) == 0 || constraint.Constraints[0] == "" {
				return nil, fmt.Errorf(
					"go/types emitted an empty build constraint for %s", constraint.File)
			}
			file, err := p.atom(constraint.File)
			if err != nil {
				return nil, err
			}
			spelling, err := p.atom(constraint.Constraints[0])
			if err != nil {
				return nil, err
			}
			var blob bytes.Buffer
			for _, decl := range constraint.ExportedDecls {
				if decl == nil || decl.Name == "" {
					return nil, fmt.Errorf(
						"go/types emitted a missing constrained declaration name")
				}
				kind, err := imageDeclarationKind(decl.Kind)
				if err != nil {
					return nil, err
				}
				blob.WriteByte(kind)
				blob.WriteString(decl.Name)
				blob.WriteByte(0)
			}
			blobCell, err := p.atom(blob.String())
			if err != nil {
				return nil, err
			}
			count, err := u32(len(constraint.ExportedDecls), "constrained declarations")
			if err != nil {
				return nil, err
			}
			p.cons = append(p.cons, conPlan{
				file:       file,
				constraint: spelling,
				blob:       blobCell,
				count:      count,
			})
		}
	}
	// References must be canonically ordered by (file, start) across the
	// whole image, not only within one package.
	sort.SliceStable(p.refs, func(i, j int) bool {
		left := p.refs[i].file
		right := p.refs[j].file
		leftFile := string(p.atoms[left.offset : left.offset+left.length])
		rightFile := string(p.atoms[right.offset : right.offset+right.length])
		if leftFile != rightFile {
			return leftFile < rightFile
		}
		return p.refs[i].start < p.refs[j].start
	})
	sort.SliceStable(p.cons, func(i, j int) bool {
		left := p.cons[i].file
		right := p.cons[j].file
		return string(p.atoms[left.offset:left.offset+left.length]) <
			string(p.atoms[right.offset:right.offset+right.length])
	})
	return p, nil
}

// finalize lays the member plane out in type-row order, binds each
// struct/interface row's member run to its contiguous slice of the plane,
// and appends the member and package documentation rows whose owner
// coordinates only exist now.
func (p *imagePlan) finalize() error {
	for index := range p.memberRequests {
		requests := p.memberRequests[index]
		if len(requests) == 0 {
			continue
		}
		start, err := u32(len(p.members), "member plane")
		if err != nil {
			return err
		}
		for _, request := range requests {
			member := request.plan
			member.owner = uint32(index)
			p.members = append(p.members, member)
			if request.doc != "" {
				text, err := p.atom(request.doc)
				if err != nil {
					return err
				}
				p.memberDocs = append(p.memberDocs, docPlan{
					ownerKind: 2,
					owner:     uint32(len(p.members) - 1),
					text:      text,
				})
			}
		}
		count, err := u32(len(requests), "members")
		if err != nil {
			return err
		}
		p.types[index].memberStart = start
		p.types[index].memberCount = count
	}
	// Documentation rows are globally sorted by owner kind: declarations,
	// then methods, then members (whose row indices exist only now), then
	// whole packages.
	p.docs = append(p.docs, p.declarationDocs...)
	p.docs = append(p.docs, p.methodDocs...)
	p.docs = append(p.docs, p.memberDocs...)
	p.docs = append(p.docs, p.packageDocs...)
	return nil
}

// marshal produces the fixed-layout image bytes: header, ordered planes,
// pooled children, and the atom plane, sealed with the domain-separated
// SHA-256 checksum over the header (digest cell excepted) and the body.
func (p *imagePlan) marshal(sourceDigest [32]byte) ([]byte, error) {
	if err := p.finalize(); err != nil {
		return nil, err
	}
	counts := []int{
		len(p.decls), len(p.types), len(p.sigParams), len(p.refs), len(p.methods),
		len(p.params), len(p.members), len(p.methodSets), len(p.docs), len(p.cons),
		len(p.sats), len(p.children), len(p.atoms),
	}
	for _, count := range counts {
		if uint64(count) > authorityMax {
			return nil, fmt.Errorf("Go authority image plane exceeds u32 capacity: %d", count)
		}
	}

	module := make([]byte, 0, authorityModuleBytes)
	if p.module.path.length != 0 {
		module = make([]byte, authorityModuleBytes)
		binary.LittleEndian.PutUint32(module[0:4], p.module.path.offset)
		binary.LittleEndian.PutUint32(module[4:8], p.module.path.length)
		binary.LittleEndian.PutUint32(module[8:12], p.module.dir.offset)
		binary.LittleEndian.PutUint32(module[12:16], p.module.dir.length)
		binary.LittleEndian.PutUint32(module[16:20], p.module.goVersion.offset)
		binary.LittleEndian.PutUint32(module[20:24], p.module.goVersion.length)
		binary.LittleEndian.PutUint32(module[24:28], p.module.version.offset)
		binary.LittleEndian.PutUint32(module[28:32], p.module.version.length)
	}
	packages := make([]byte, 0, len(p.packages)*authorityPackageBytes)
	for _, row := range p.packages {
		rowBytes := make([]byte, authorityPackageBytes)
		binary.LittleEndian.PutUint32(rowBytes[0:4], row.importPath.offset)
		binary.LittleEndian.PutUint32(rowBytes[4:8], row.importPath.length)
		binary.LittleEndian.PutUint32(rowBytes[8:12], row.name.offset)
		binary.LittleEndian.PutUint32(rowBytes[12:16], row.name.length)
		binary.LittleEndian.PutUint32(rowBytes[16:20], row.files.offset)
		binary.LittleEndian.PutUint32(rowBytes[20:24], row.files.length)
		binary.LittleEndian.PutUint32(rowBytes[24:28], row.fileCount)
		packages = append(packages, rowBytes...)
	}
	decls := make([]byte, 0, len(p.decls)*authorityDeclBytes)
	for _, row := range p.decls {
		rowBytes := make([]byte, authorityDeclBytes)
		rowBytes[0] = row.kind
		if row.exported {
			rowBytes[1] = 1
		}
		if row.iota {
			rowBytes[2] = 1
		}
		binary.LittleEndian.PutUint32(rowBytes[4:8], row.name.offset)
		binary.LittleEndian.PutUint32(rowBytes[8:12], row.name.length)
		binary.LittleEndian.PutUint32(rowBytes[12:16], row.pkg.offset)
		binary.LittleEndian.PutUint32(rowBytes[16:20], row.pkg.length)
		binary.LittleEndian.PutUint32(rowBytes[20:24], row.typeRoot)
		binary.LittleEndian.PutUint32(rowBytes[24:28], row.spanStart)
		binary.LittleEndian.PutUint32(rowBytes[28:32], row.spanEnd)
		binary.LittleEndian.PutUint32(rowBytes[32:36], row.file.offset)
		binary.LittleEndian.PutUint32(rowBytes[36:40], row.file.length)
		binary.LittleEndian.PutUint32(rowBytes[40:44], row.value.offset)
		binary.LittleEndian.PutUint32(rowBytes[44:48], row.value.length)
		binary.LittleEndian.PutUint64(rowBytes[48:56], uint64(row.constGroup))
		decls = append(decls, rowBytes...)
	}
	types := make([]byte, 0, len(p.types)*authorityTypeBytes)
	for _, row := range p.types {
		rowBytes := make([]byte, authorityTypeBytes)
		rowBytes[0] = row.kind
		rowBytes[1] = row.dir
		if row.variadic {
			rowBytes[2] = 1
		}
		binary.LittleEndian.PutUint32(rowBytes[4:8], row.name.offset)
		binary.LittleEndian.PutUint32(rowBytes[8:12], row.name.length)
		binary.LittleEndian.PutUint32(rowBytes[12:16], row.pkg.offset)
		binary.LittleEndian.PutUint32(rowBytes[16:20], row.pkg.length)
		binary.LittleEndian.PutUint64(rowBytes[20:28], uint64(row.length))
		binary.LittleEndian.PutUint32(rowBytes[28:32], row.childStart)
		binary.LittleEndian.PutUint32(rowBytes[32:36], row.childCount)
		binary.LittleEndian.PutUint32(rowBytes[36:40], row.memberStart)
		binary.LittleEndian.PutUint32(rowBytes[40:44], row.memberCount)
		binary.LittleEndian.PutUint32(rowBytes[48:52], row.paramCount)
		types = append(types, rowBytes...)
	}
	sigParams := make([]byte, 0, len(p.sigParams)*authoritySigParamBytes)
	for _, row := range p.sigParams {
		rowBytes := make([]byte, authoritySigParamBytes)
		binary.LittleEndian.PutUint32(rowBytes[0:4], row.owner)
		binary.LittleEndian.PutUint32(rowBytes[4:8], row.ordinal)
		binary.LittleEndian.PutUint32(rowBytes[8:12], row.name.offset)
		binary.LittleEndian.PutUint32(rowBytes[12:16], row.name.length)
		binary.LittleEndian.PutUint32(rowBytes[16:20], row.file.offset)
		binary.LittleEndian.PutUint32(rowBytes[20:24], row.file.length)
		binary.LittleEndian.PutUint32(rowBytes[24:28], row.offset)
		sigParams = append(sigParams, rowBytes...)
	}
	methods := make([]byte, 0, len(p.methods)*authorityMethodBytes)
	for _, row := range p.methods {
		rowBytes := make([]byte, authorityMethodBytes)
		binary.LittleEndian.PutUint32(rowBytes[0:4], row.owner)
		if row.exported {
			rowBytes[4] = 1
		}
		if row.pointerRecv {
			rowBytes[5] = 1
		}
		if row.promoted {
			rowBytes[6] = 1
		}
		binary.LittleEndian.PutUint32(rowBytes[8:12], row.name.offset)
		binary.LittleEndian.PutUint32(rowBytes[12:16], row.name.length)
		binary.LittleEndian.PutUint32(rowBytes[16:20], row.typeRoot)
		binary.LittleEndian.PutUint32(rowBytes[20:24], row.receiver.offset)
		binary.LittleEndian.PutUint32(rowBytes[24:28], row.receiver.length)
		binary.LittleEndian.PutUint32(rowBytes[28:32], row.recvParams.offset)
		binary.LittleEndian.PutUint32(rowBytes[32:36], row.recvParams.length)
		binary.LittleEndian.PutUint32(rowBytes[36:40], row.recvParamCount)
		binary.LittleEndian.PutUint32(rowBytes[40:44], row.origin.offset)
		binary.LittleEndian.PutUint32(rowBytes[44:48], row.origin.length)
		binary.LittleEndian.PutUint32(rowBytes[48:52], row.spanStart)
		binary.LittleEndian.PutUint32(rowBytes[52:56], row.spanEnd)
		binary.LittleEndian.PutUint32(rowBytes[56:60], row.file.offset)
		binary.LittleEndian.PutUint32(rowBytes[60:64], row.file.length)
		methods = append(methods, rowBytes...)
	}
	params := make([]byte, 0, len(p.params)*authorityParamBytes)
	for _, row := range p.params {
		rowBytes := make([]byte, authorityParamBytes)
		binary.LittleEndian.PutUint32(rowBytes[0:4], row.owner)
		binary.LittleEndian.PutUint32(rowBytes[4:8], row.name.offset)
		binary.LittleEndian.PutUint32(rowBytes[8:12], row.name.length)
		binary.LittleEndian.PutUint32(rowBytes[12:16], row.constraint)
		params = append(params, rowBytes...)
	}
	members := make([]byte, 0, len(p.members)*authorityMemberBytes)
	for _, row := range p.members {
		rowBytes := make([]byte, authorityMemberBytes)
		binary.LittleEndian.PutUint32(rowBytes[0:4], row.owner)
		rowBytes[4] = row.kind
		if row.embedded {
			rowBytes[5] = 1
		}
		if row.exported {
			rowBytes[6] = 1
		}
		binary.LittleEndian.PutUint32(rowBytes[8:12], row.name.offset)
		binary.LittleEndian.PutUint32(rowBytes[12:16], row.name.length)
		binary.LittleEndian.PutUint32(rowBytes[16:20], row.typeRoot)
		binary.LittleEndian.PutUint32(rowBytes[20:24], row.tag.offset)
		binary.LittleEndian.PutUint32(rowBytes[24:28], row.tag.length)
		binary.LittleEndian.PutUint32(rowBytes[28:32], row.pkg.offset)
		binary.LittleEndian.PutUint32(rowBytes[32:36], row.pkg.length)
		members = append(members, rowBytes...)
	}
	methodSets := make([]byte, 0, len(p.methodSets)*authorityMethodSetBytes)
	for _, row := range p.methodSets {
		rowBytes := make([]byte, authorityMethodSetBytes)
		binary.LittleEndian.PutUint32(rowBytes[0:4], row.owner)
		binary.LittleEndian.PutUint32(rowBytes[4:8], row.name.offset)
		binary.LittleEndian.PutUint32(rowBytes[8:12], row.name.length)
		binary.LittleEndian.PutUint32(rowBytes[12:16], row.sigRoot)
		binary.LittleEndian.PutUint32(rowBytes[16:20], row.pkg.offset)
		binary.LittleEndian.PutUint32(rowBytes[20:24], row.pkg.length)
		methodSets = append(methodSets, rowBytes...)
	}
	docs := make([]byte, 0, len(p.docs)*authorityDocBytes)
	for _, row := range p.docs {
		rowBytes := make([]byte, authorityDocBytes)
		rowBytes[0] = row.ownerKind
		binary.LittleEndian.PutUint32(rowBytes[4:8], row.owner)
		binary.LittleEndian.PutUint32(rowBytes[8:12], row.text.offset)
		binary.LittleEndian.PutUint32(rowBytes[12:16], row.text.length)
		docs = append(docs, rowBytes...)
	}
	refs := make([]byte, 0, len(p.refs)*authorityRefBytes)
	for _, row := range p.refs {
		rowBytes := make([]byte, authorityRefBytes)
		binary.LittleEndian.PutUint32(rowBytes[0:4], row.owner)
		binary.LittleEndian.PutUint32(rowBytes[4:8], row.target.offset)
		binary.LittleEndian.PutUint32(rowBytes[8:12], row.target.length)
		binary.LittleEndian.PutUint32(rowBytes[12:16], row.targetPkg.offset)
		binary.LittleEndian.PutUint32(rowBytes[16:20], row.targetPkg.length)
		binary.LittleEndian.PutUint32(rowBytes[20:24], row.start)
		binary.LittleEndian.PutUint32(rowBytes[24:28], row.end)
		binary.LittleEndian.PutUint32(rowBytes[28:32], row.file.offset)
		binary.LittleEndian.PutUint32(rowBytes[32:36], row.file.length)
		binary.LittleEndian.PutUint32(rowBytes[36:40], row.receiver.offset)
		binary.LittleEndian.PutUint32(rowBytes[40:44], row.receiver.length)
		refs = append(refs, rowBytes...)
	}
	cons := make([]byte, 0, len(p.cons)*authorityConBytes)
	for _, row := range p.cons {
		rowBytes := make([]byte, authorityConBytes)
		binary.LittleEndian.PutUint32(rowBytes[0:4], row.file.offset)
		binary.LittleEndian.PutUint32(rowBytes[4:8], row.file.length)
		binary.LittleEndian.PutUint32(rowBytes[8:12], row.constraint.offset)
		binary.LittleEndian.PutUint32(rowBytes[12:16], row.constraint.length)
		binary.LittleEndian.PutUint32(rowBytes[16:20], row.blob.offset)
		binary.LittleEndian.PutUint32(rowBytes[20:24], row.blob.length)
		binary.LittleEndian.PutUint32(rowBytes[24:28], row.count)
		cons = append(cons, rowBytes...)
	}
	sats := make([]byte, 0, len(p.sats)*authoritySatBytes)
	for _, row := range p.sats {
		rowBytes := make([]byte, authoritySatBytes)
		binary.LittleEndian.PutUint32(rowBytes[0:4], row.subject)
		binary.LittleEndian.PutUint32(rowBytes[4:8], row.target.offset)
		binary.LittleEndian.PutUint32(rowBytes[8:12], row.target.length)
		binary.LittleEndian.PutUint32(rowBytes[12:16], row.targetPkg.offset)
		binary.LittleEndian.PutUint32(rowBytes[16:20], row.targetPkg.length)
		sats = append(sats, rowBytes...)
	}
	children := make([]byte, 0, len(p.children)*authorityChildBytes)
	for _, cell := range p.children {
		cellBytes := make([]byte, authorityChildBytes)
		binary.LittleEndian.PutUint32(cellBytes[0:4], cell.target)
		binary.LittleEndian.PutUint32(cellBytes[4:8], cell.flags)
		children = append(children, cellBytes...)
	}

	// Frozen body order: declarations, types, methods, type parameters,
	// members, docs, references, constraints, satisfactions, module,
	// packages, signature parameters, interface method sets, children,
	// atoms. The Rust reader locates every plane from this exact sequence.
	bodyParts := [][]byte{
		decls, types, methods, params, members, docs, refs, cons, sats,
		module, packages, sigParams, methodSets, children, p.atoms,
	}
	bodyBytes := 0
	for _, part := range bodyParts {
		bodyBytes += len(part)
	}
	if uint64(bodyBytes) > authorityMax {
		return nil, fmt.Errorf("Go authority image body exceeds u32 capacity: %d", bodyBytes)
	}
	image := make([]byte, authorityHeaderBytes+bodyBytes)
	copy(image[:4], []byte("NGAI"))
	binary.LittleEndian.PutUint16(image[4:6], authorityVersion)
	binary.LittleEndian.PutUint16(image[6:8], authorityHeaderBytes)
	binary.LittleEndian.PutUint32(image[8:12], uint32(len(p.decls)))
	binary.LittleEndian.PutUint32(image[12:16], uint32(len(p.atoms)))
	binary.LittleEndian.PutUint32(image[16:20], uint32(bodyBytes))
	copy(image[20:52], sourceDigest[:])
	binary.LittleEndian.PutUint32(image[84:88], uint32(len(p.types)))
	binary.LittleEndian.PutUint32(image[88:92], uint32(len(p.refs)))
	binary.LittleEndian.PutUint32(image[92:96], uint32(len(p.methods)))
	binary.LittleEndian.PutUint32(image[96:100], uint32(len(p.params)))
	binary.LittleEndian.PutUint32(image[100:104], uint32(len(p.members)))
	binary.LittleEndian.PutUint32(image[104:108], uint32(len(p.docs)))
	binary.LittleEndian.PutUint32(image[108:112], uint32(len(p.cons)))
	binary.LittleEndian.PutUint32(image[112:116], uint32(len(p.sats)))
	moduleCount := 0
	if len(module) != 0 {
		moduleCount = 1
	}
	binary.LittleEndian.PutUint32(image[116:120], uint32(moduleCount))
	binary.LittleEndian.PutUint32(image[120:124], uint32(len(p.packages)))
	binary.LittleEndian.PutUint32(image[124:128], uint32(len(p.sigParams)))
	binary.LittleEndian.PutUint32(image[128:132], uint32(len(p.methodSets)))
	cursor := authorityHeaderBytes
	for _, part := range bodyParts {
		copy(image[cursor:], part)
		cursor += len(part)
	}
	digest := sha256.New()
	_, _ = digest.Write(authorityDigestDomain)
	_, _ = digest.Write(image[:52])
	_, _ = digest.Write(image[84:authorityHeaderBytes])
	_, _ = digest.Write(image[authorityHeaderBytes:])
	copy(image[52:84], digest.Sum(nil))
	return image, nil
}

// writeAuthorityImage emits the complete authority image for one
// caller-selected source file onto destination.
func writeAuthorityImage(destination io.Writer, sourcePath string, output *Output) error {
	source, err := os.ReadFile(sourcePath)
	if err != nil {
		return fmt.Errorf("read authority source %s: %w", sourcePath, err)
	}
	plan, err := buildAuthorityPlan(output)
	if err != nil {
		return err
	}
	image, err := plan.marshal(sha256.Sum256(source))
	if err != nil {
		return err
	}
	if _, err := io.Copy(destination, bytes.NewReader(image)); err != nil {
		return fmt.Errorf("write Go authority image: %w", err)
	}
	return nil
}
