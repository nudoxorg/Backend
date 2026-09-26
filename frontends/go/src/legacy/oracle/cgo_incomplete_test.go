package main

import (
	"go/token"
	"go/types"
	"testing"
)

// An incomplete named type (nil underlying, no checker) makes go/types panic
// inside under() when NewMethodSet or IsInterface expands it. sqlite3's cgo
// types hit that panic and used to abort the oracle. The name must be
// recorded and the call must return.
func TestIncompleteNamedPromotedMethodsDoNotPanic(t *testing.T) {
	pkg := types.NewPackage("example.com/cgo", "cgo")
	obj := types.NewTypeName(token.NoPos, pkg, "Row", nil)
	named := types.NewNamed(obj, nil, nil)

	s := &serializer{}
	methods := s.promotedMethods(named, &docCatalog{})
	if len(methods) != 0 {
		t.Fatalf("incomplete named must not produce promoted methods, got %#v", methods)
	}
	assertUnresolved(t, s, "example.com/cgo.Row")
}

// A struct field whose type is package C must be listed and must not be
// expanded via Underlying. Expanding it is what panics on a real cgo type.
func TestCgoFieldIsRecordedWithoutExpansion(t *testing.T) {
	cPkg := types.NewPackage("C", "C")
	cObj := types.NewTypeName(token.NoPos, cPkg, "sqlite3", nil)
	cType := types.NewNamed(cObj, nil, nil)

	ownerPkg := types.NewPackage("example.com/cgo", "cgo")
	field := types.NewField(token.NoPos, ownerPkg, "db", cType, false)
	st := types.NewStruct([]*types.Var{field}, nil)
	owner := types.NewTypeName(token.NoPos, ownerPkg, "Conn", nil)
	named := types.NewNamed(owner, st, nil)

	s := &serializer{}
	_ = s.promotedMethods(named, &docCatalog{})
	assertUnresolved(t, s, "C.sqlite3")
}

func assertUnresolved(t *testing.T, s *serializer, want string) {
	t.Helper()
	for _, name := range s.unresolvedList() {
		if name == want {
			return
		}
	}
	t.Fatalf("unresolved = %v, want %s", s.unresolvedList(), want)
}
