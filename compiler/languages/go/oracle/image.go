// Emits the fixed Go authority image directly from go/packages and go/types facts.
// Binds the image to one caller-selected source file with a SHA-256 digest.
// Keeps semantic transport binary and fixed-width; JSON is not an IR boundary.
package main

import (
	"bytes"
	"crypto/sha256"
	"encoding/binary"
	"fmt"
	"io"
	"os"
)

const (
	imageHeaderBytes      = 88
	imageDeclarationBytes = 12
	imageMaxUint32         = uint64(^uint32(0))
)

var imageDigestDomain = []byte("nudox.go.authority.image.sha256.v1\x00")

type imageDeclaration struct {
	kind     byte
	exported bool
	offset   uint32
	length   uint32
}

func writeAuthorityImage(destination io.Writer, sourcePath string, output *Output) error {
	source, err := os.ReadFile(sourcePath)
	if err != nil {
		return fmt.Errorf("read authority source %s: %w", sourcePath, err)
	}
	sourceDigest := sha256.Sum256(source)
	declarations := make([]imageDeclaration, 0)
	names := make([]byte, 0)
	for _, pkg := range output.Packages {
		for _, declaration := range pkg.Decls {
			kind, err := imageDeclarationKind(declaration.Kind)
			if err != nil {
				return err
			}
			if declaration.Name == "" {
				return fmt.Errorf("go/types emitted an empty declaration name")
			}
			if uint64(len(names)) > imageMaxUint32 || uint64(len(declaration.Name)) > imageMaxUint32 {
				return fmt.Errorf("Go authority atom plane exceeds u32 capacity")
			}
			declarations = append(declarations, imageDeclaration{
				kind:     kind,
				exported: declaration.Exported,
				offset:   uint32(len(names)),
				length:   uint32(len(declaration.Name)),
			})
			names = append(names, declaration.Name...)
		}
	}
	if uint64(len(declarations)) > imageMaxUint32 || uint64(len(names)) > imageMaxUint32 {
		return fmt.Errorf("Go authority image exceeds u32 capacity")
	}
	declarationBytes := len(declarations) * imageDeclarationBytes
	bodyBytes := declarationBytes + len(names)
	if declarationBytes/imageDeclarationBytes != len(declarations) || bodyBytes < declarationBytes || uint64(bodyBytes) > imageMaxUint32 {
		return fmt.Errorf("Go authority image body exceeds u32 capacity")
	}
	image := make([]byte, imageHeaderBytes+bodyBytes)
	copy(image[:4], []byte("NGAI"))
	binary.LittleEndian.PutUint16(image[4:6], 1)
	binary.LittleEndian.PutUint16(image[6:8], imageHeaderBytes)
	binary.LittleEndian.PutUint32(image[8:12], uint32(len(declarations)))
	binary.LittleEndian.PutUint32(image[12:16], uint32(len(names)))
	binary.LittleEndian.PutUint32(image[16:20], uint32(bodyBytes))
	copy(image[20:52], sourceDigest[:])
	for index, declaration := range declarations {
		start := imageHeaderBytes + index*imageDeclarationBytes
		image[start] = declaration.kind
		if declaration.exported {
			image[start+1] = 1
		}
		binary.LittleEndian.PutUint32(image[start+4:start+8], declaration.offset)
		binary.LittleEndian.PutUint32(image[start+8:start+12], declaration.length)
	}
	copy(image[imageHeaderBytes+declarationBytes:], names)
	digest := sha256.New()
	_, _ = digest.Write(imageDigestDomain)
	_, _ = digest.Write(image[:52])
	_, _ = digest.Write(image[84:imageHeaderBytes])
	_, _ = digest.Write(image[imageHeaderBytes:])
	copy(image[52:84], digest.Sum(nil))
	if _, err := io.Copy(destination, bytes.NewReader(image)); err != nil {
		return fmt.Errorf("write Go authority image: %w", err)
	}
	return nil
}

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
