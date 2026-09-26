package main

import (
	"crypto/sha256"
	"encoding/binary"
	"testing"
)

func TestUnresolvedCgoPlaneMarshalsBothNames(t *testing.T) {
	output := &Output{
		Packages: []*Package{{
			ImportPath:    "example.com/cgo",
			Name:          "cgo",
			UnresolvedCgo: []string{"C.sqlite3", "example.com/cgo.Conn"},
		}},
	}
	plan, err := buildAuthorityPlan(output, "")
	if err != nil {
		t.Fatalf("buildAuthorityPlan: %v", err)
	}
	image, err := plan.marshal(sha256.Sum256([]byte("package cgo\n")))
	if err != nil {
		t.Fatalf("marshal: %v", err)
	}
	count := binary.LittleEndian.Uint32(image[132:136])
	if count != 2 {
		t.Fatalf("unresolved cgo count = %d, want 2", count)
	}
	atomBytes := int(binary.LittleEndian.Uint32(image[12:16]))
	bodyBytes := int(binary.LittleEndian.Uint32(image[16:20]))
	atomOffset := 136 + bodyBytes - atomBytes
	cgoOffset := atomOffset - int(count)*8
	if cgoOffset < 136 {
		t.Fatalf("cgo plane offset %d before body", cgoOffset)
	}
	names := make([]string, 0, count)
	for i := 0; i < int(count); i++ {
		row := image[cgoOffset+i*8 : cgoOffset+(i+1)*8]
		offset := binary.LittleEndian.Uint32(row[0:4])
		length := binary.LittleEndian.Uint32(row[4:8])
		start := atomOffset + int(offset)
		names = append(names, string(image[start:start+int(length)]))
	}
	want := []string{"C.sqlite3", "example.com/cgo.Conn"}
	if names[0] != want[0] || names[1] != want[1] {
		t.Fatalf("unresolved cgo plane = %v, want %v", names, want)
	}
}
