package main

import (
	"encoding/binary"
	"go/ast"
	"go/parser"
	"go/token"
	"go/types"
	"strings"
	"testing"

	"golang.org/x/tools/go/packages"
)

func TestFieldWitnessBindsTypedASTObjectsAndFiles(t *testing.T) {
	sources := map[string]string{
		"main.go":    "// λ byte offsets\npackage demo\ntype Embed struct{}\ntype Group struct { wg, µ int; _ int; Embed; nested struct { wg int } }\ntype Other struct { wg int }\n",
		"sibling.go": "package demo\ntype Sibling struct { wg int }\n",
	}
	fileset := token.NewFileSet()
	var syntax []*ast.File
	for _, path := range []string{"main.go", "sibling.go"} {
		file, err := parser.ParseFile(fileset, path, sources[path], 0)
		if err != nil {
			t.Fatal(err)
		}
		syntax = append(syntax, file)
	}
	info := &types.Info{Defs: make(map[*ast.Ident]types.Object)}
	configuration := &types.Config{}
	checked, err := configuration.Check("example.com/demo", fileset, syntax, info)
	if err != nil {
		t.Fatal(err)
	}
	serializer := newSerializer(&packages.Package{
		Fset: fileset, Syntax: syntax, Types: checked, TypesInfo: info,
	})
	structure := func(name string) *Type {
		return serializer.typ(checked.Scope().Lookup(name).Type().Underlying())
	}
	group, other, sibling := structure("Group"), structure("Other"), structure("Sibling")
	if len(group.Fields) != 5 {
		t.Fatalf("field inventory: %d", len(group.Fields))
	}
	for _, field := range []*StructField{group.Fields[0], group.Fields[1], group.Fields[4], other.Fields[0], sibling.Fields[0]} {
		if field.sourceSpan == nil || field.nameSpan == nil || field.sourcePos == nil {
			t.Fatalf("missing exact typed AST witness: %s", field.Name)
		}
		source := sources[field.sourcePos.File]
		if source[field.nameSpan.Start:field.nameSpan.End] != field.Name {
			t.Fatalf("wrong identifier extent for %q", field.Name)
		}
	}
	if *group.Fields[0].sourceSpan != *group.Fields[1].sourceSpan ||
		*group.Fields[0].nameSpan == *group.Fields[1].nameSpan {
		t.Fatal("grouped fields must share the full AST field but retain distinct name extents")
	}
	if *group.Fields[0].sourceSpan == *other.Fields[0].sourceSpan {
		t.Fatal("same spelling in another owner cannot borrow an extent")
	}
	if group.Fields[2].sourceSpan != nil || group.Fields[3].sourceSpan != nil ||
		group.Fields[4].Type.Fields[0].sourceSpan != nil {
		t.Fatal("blank, embedded and anonymous-owner fields have no invented witness")
	}
	generated := serializer.typ(types.NewStruct([]*types.Var{
		types.NewField(token.NoPos, checked, "wg", types.Typ[types.Int], false),
	}, nil))
	if generated.Fields[0].sourceSpan != nil {
		t.Fatal("positionless generated object borrowed source")
	}

	plan := &imagePlan{boundSource: "main.go"}
	for _, typ := range []*Type{group, other, sibling, generated} {
		if _, err := plan.emitType(typ); err != nil {
			t.Fatal(err)
		}
	}
	if err := plan.finalize(); err != nil {
		t.Fatal(err)
	}
	bound, foreign, absent := 0, 0, 0
	for _, member := range plan.members {
		if member.spanStart == authorityNone {
			if member.bound {
				t.Fatal("absent field witness claims selected source")
			}
			absent++
		} else if member.bound {
			bound++
		} else {
			foreign++
		}
	}
	if bound != 4 || foreign != 1 || absent != 4 {
		t.Fatalf("wrong field witness partition: bound=%d foreign=%d absent=%d", bound, foreign, absent)
	}
	image, err := plan.marshal([32]byte{})
	if err != nil {
		t.Fatal(err)
	}
	if binary.LittleEndian.Uint16(image[4:6]) != 7 || authorityMemberBytes != 72 {
		t.Fatal("field witnesses require the additive v7 transport")
	}
}

func TestFieldWitnessProducerLeavesUnprovenRowsAbsentAndRefusesPartialRanges(t *testing.T) {
	plain := &StructField{Name: "wg", Type: &Type{Kind: "basic", Name: "int"}}
	plan := &imagePlan{boundSource: "main.go"}
	if _, err := plan.emitType(&Type{Kind: "struct", Fields: []*StructField{plain}}); err != nil {
		t.Fatal(err)
	}
	if err := plan.finalize(); err != nil {
		t.Fatal(err)
	}
	if plan.members[0].spanStart != authorityNone || plan.members[0].bound {
		t.Fatal("legacy/JSON-derived field invented an authority extent")
	}
	invalid := *plain
	invalid.sourceSpan = &Span{Start: 10, End: 20}
	invalid.nameSpan = &Span{Start: 19, End: 21}
	invalid.sourcePos = &Pos{File: "main.go", Offset: 19}
	if _, err := (&imagePlan{boundSource: "main.go"}).emitType(
		&Type{Kind: "struct", Fields: []*StructField{&invalid}}); err == nil ||
		!strings.Contains(err.Error(), "invalid field source witness") {
		t.Fatalf("uncontained producer range was not refused: %v", err)
	}
}
