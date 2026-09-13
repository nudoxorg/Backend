// Command backend-go-semantic projects go/packages facts onto the backend
// native semantic protocol. Compiler analysis stays language-native; framing,
// bounds, cancellation, and evidence remain owned by backend-compile.
package main

import (
	"bufio"
	"bytes"
	"encoding/binary"
	"errors"
	"fmt"
	"go/ast"
	"go/token"
	"go/types"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"sort"
	"strings"

	"golang.org/x/tools/go/packages"
)

const schema = "go-semantic-v1"

type request struct {
	language                     string
	session, manifest, authority [32]byte
	inputs                       map[string][]byte
}

type record struct {
	kind       byte
	key, value string
}

func main() {
	if err := run(os.Stdin, os.Stdout, os.Stderr); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

func run(input io.Reader, output, payload io.Writer) error {
	buffered := bufio.NewReaderSize(input, 4)
	prefix, err := buffered.Peek(4)
	if err != nil {
		return err
	}
	if bytes.Equal(prefix, []byte("BCF\x00")) {
		return runSession(buffered, output, payload)
	}
	cold, err := io.ReadAll(io.LimitReader(buffered, 2<<20))
	if err != nil {
		return err
	}
	req, err := decodeRequest(cold)
	if err != nil {
		return err
	}
	records, err := analyze(req)
	if err != nil {
		return err
	}
	return encodeEnvelope(output, req, 0, records)
}

func runSession(input *bufio.Reader, output, payload io.Writer) error {
	hello, err := readFrame(input)
	if err != nil {
		return err
	}
	if hello[5] != 1 {
		return errors.New("expected BCF hello")
	}
	if _, err := output.Write(hello); err != nil {
		return err
	}
	for {
		frame, err := readFrame(input)
		if errors.Is(err, io.EOF) {
			return nil
		}
		if err != nil {
			return err
		}
		if frame[5] != 2 {
			return errors.New("expected BCF request")
		}
		req, err := readRequest(input)
		if err != nil {
			return err
		}
		records, err := analyze(req)
		if err != nil {
			return err
		}
		revision := binary.BigEndian.Uint64(frame[46:54])
		if err := encodeEnvelope(payload, req, revision, records); err != nil {
			return err
		}
		frame[5] = 5
		if _, err := output.Write(frame); err != nil {
			return err
		}
	}
}

func readFrame(input io.Reader) ([]byte, error) {
	header := make([]byte, 6)
	if _, err := io.ReadFull(input, header); err != nil {
		return nil, err
	}
	if string(header[:3]) != "BCF" || binary.BigEndian.Uint16(header[3:5]) != 1 {
		return nil, errors.New("invalid BCF frame")
	}
	sizes := map[byte]int{1: 38, 2: 54, 3: 14, 4: 6, 5: 54}
	size, ok := sizes[header[5]]
	if !ok {
		return nil, errors.New("unknown BCF frame kind")
	}
	frame := make([]byte, size)
	copy(frame, header)
	_, err := io.ReadFull(input, frame[6:])
	return frame, err
}

func decodeRequest(data []byte) (request, error) {
	reader := bytes.NewReader(data)
	req, err := readRequest(reader)
	if err != nil {
		return request{}, err
	}
	if reader.Len() != 0 {
		return request{}, errors.New("trailing BCQ bytes")
	}
	return req, nil
}

func readRequest(reader io.Reader) (request, error) {
	take := func(size int) ([]byte, error) {
		value := make([]byte, size)
		_, err := io.ReadFull(reader, value)
		return value, err
	}
	magic, err := take(4)
	if err != nil || string(magic) != "BCQ\x00" {
		return request{}, errors.New("invalid BCQ request")
	}
	version, err := readU16(reader)
	if err != nil || version != 1 {
		return request{}, errors.New("unsupported BCQ version")
	}
	languageLength, err := readU16(reader)
	if err != nil {
		return request{}, err
	}
	count, err := readU16(reader)
	if err != nil {
		return request{}, err
	}
	var req request
	for _, target := range []*[32]byte{&req.session, &req.manifest, &req.authority} {
		value, err := take(len(target))
		if err != nil {
			return request{}, err
		}
		copy(target[:], value)
	}
	language, err := take(int(languageLength))
	if err != nil {
		return request{}, err
	}
	req.language = string(language)
	if req.language != "go" {
		return request{}, errors.New("BCQ language is not go")
	}
	req.inputs = make(map[string][]byte, count)
	for range count {
		nameLength, err := readU16(reader)
		if err != nil {
			return request{}, err
		}
		valueLength, err := readU32(reader)
		if err != nil {
			return request{}, err
		}
		name, err := take(int(nameLength))
		if err != nil {
			return request{}, err
		}
		value, err := take(int(valueLength))
		if err != nil {
			return request{}, err
		}
		if _, exists := req.inputs[string(name)]; exists {
			return request{}, errors.New("duplicate BCQ input")
		}
		req.inputs[string(name)] = value
	}
	return req, nil
}

func analyze(req request) ([]record, error) {
	source, ok := req.inputs["module/source.go"]
	if !ok {
		return nil, errors.New("missing module/source.go")
	}
	module, ok := req.inputs["go.mod"]
	if !ok {
		return nil, errors.New("missing go.mod")
	}
	dir, err := os.MkdirTemp("", "backend-go-semantic-")
	if err != nil {
		return nil, err
	}
	defer os.RemoveAll(dir)
	if err := os.WriteFile(filepath.Join(dir, "source.go"), source, 0o600); err != nil {
		return nil, err
	}
	if err := os.WriteFile(filepath.Join(dir, "go.mod"), module, 0o600); err != nil {
		return nil, err
	}
	toolchain := os.Getenv("BACKEND_NATIVE_TOOLCHAIN")
	if toolchain == "" {
		return nil, errors.New("missing BACKEND_NATIVE_TOOLCHAIN")
	}
	if _, err := exec.Command(toolchain, "version").Output(); err != nil {
		return nil, fmt.Errorf("verify go toolchain: %w", err)
	}
	toolchainPath := filepath.Dir(toolchain) + string(os.PathListSeparator) + os.Getenv("PATH")
	if err := os.Setenv("PATH", toolchainPath); err != nil {
		return nil, fmt.Errorf("bind go toolchain path: %w", err)
	}
	tags := string(req.inputs["build-tags"])
	flags := []string(nil)
	if tags != "" {
		flags = []string{"-tags=" + tags}
	}
	config := &packages.Config{
		Mode: packages.NeedName | packages.NeedFiles | packages.NeedSyntax |
			packages.NeedTypes | packages.NeedTypesInfo | packages.NeedImports |
			packages.NeedDeps | packages.NeedModule,
		Dir:        dir,
		BuildFlags: flags,
		Env: append(os.Environ(),
			"GOWORK=off",
			"GOPROXY=off",
			"GOSUMDB=off",
			"GOTOOLCHAIN=local",
			"HOME="+dir,
			"GOCACHE="+filepath.Join(dir, ".cache", "go-build"),
			"GOMAXPROCS=2",
			"GOFLAGS=-p=2",
			"PATH="+toolchainPath),
	}
	loaded, err := packages.Load(config, "./...")
	if err != nil {
		return nil, fmt.Errorf("load Go package: %w", err)
	}
	var records []record
	for _, pkg := range loaded {
		records = append(records, packageRecords(pkg)...)
	}
	records = append(records, record{6, "go/vendor/optional", value("package", "go/vendor/optional", "absent")})
	sort.Slice(records, func(i, j int) bool {
		if records[i].kind != records[j].kind {
			return records[i].kind < records[j].kind
		}
		return records[i].key < records[j].key
	})
	return deduplicate(records), nil
}

func packageRecords(pkg *packages.Package) []record {
	var records []record
	for _, problem := range pkg.Errors {
		records = append(records, record{4, "go/package-error/" + problem.Pos,
			value("error", "go/packages", "0", "0", problem.Msg)})
	}
	if pkg.Types == nil || pkg.TypesInfo == nil {
		return records
	}
	docs := documentation(pkg.Syntax)
	for identifier, object := range pkg.TypesInfo.Defs {
		if object == nil || identifier == nil {
			continue
		}
		start, end := offsets(pkg.Fset, identifier.Pos(), identifier.End())
		key := objectKey(pkg.PkgPath, object)
		signature := types.TypeString(object.Type(), qualifier)
		records = append(records,
			record{1, key, value(objectKind(object), pkg.PkgPath, signature, docs[identifier.Pos()], number(start), number(end))},
			record{2, key, value(key, signature)})
	}
	for identifier, object := range pkg.TypesInfo.Uses {
		if object == nil || identifier == nil {
			continue
		}
		start, end := offsets(pkg.Fset, identifier.Pos(), identifier.End())
		target := objectKey(packagePath(object), object)
		owner := pkg.PkgPath
		resolution := "foreign"
		if packagePath(object) == pkg.PkgPath {
			resolution = "local"
		}
		key := fmt.Sprintf("%s->%s@%d:%d", owner, target, start, end)
		records = append(records, record{3, key, value(owner, target, number(start), number(end), resolution)})
	}
	for path := range pkg.Imports {
		records = append(records, record{5, "go/" + path, value("package", path, "present")})
	}
	return records
}

func documentation(files []*ast.File) map[token.Pos]string {
	result := make(map[token.Pos]string)
	for _, file := range files {
		for _, declaration := range file.Decls {
			switch declaration := declaration.(type) {
			case *ast.FuncDecl:
				if declaration.Doc != nil {
					result[declaration.Name.Pos()] = declaration.Doc.Text()
				}
			case *ast.GenDecl:
				for _, spec := range declaration.Specs {
					if named, ok := spec.(*ast.TypeSpec); ok && declaration.Doc != nil {
						result[named.Name.Pos()] = declaration.Doc.Text()
					}
				}
			}
		}
	}
	return result
}

func objectKind(object types.Object) string {
	switch object.(type) {
	case *types.TypeName:
		return "type"
	case *types.Func:
		return "function"
	case *types.Const:
		return "constant"
	case *types.Var:
		return "variable"
	case *types.PkgName:
		return "import"
	case *types.Label:
		return "label"
	default:
		return "unknown"
	}
}

func packagePath(object types.Object) string {
	if object.Pkg() == nil {
		return "builtin"
	}
	return object.Pkg().Path()
}

func objectKey(pkg string, object types.Object) string {
	if pkg == "" {
		pkg = packagePath(object)
	}
	return "go/" + strings.Trim(pkg, "/") + "/" + object.Name()
}

func qualifier(pkg *types.Package) string { return pkg.Path() }

func offsets(files *token.FileSet, start, end token.Pos) (int, int) {
	return files.PositionFor(start, false).Offset, files.PositionFor(end, false).Offset
}

func value(fields ...string) string { return strings.Join(append([]string{schema}, fields...), "\x00") }
func number(value int) string       { return fmt.Sprintf("%d", value) }

func deduplicate(records []record) []record {
	output := records[:0]
	for _, current := range records {
		if len(output) == 0 || output[len(output)-1].kind != current.kind || output[len(output)-1].key != current.key {
			output = append(output, current)
		}
	}
	return output
}

func encodeEnvelope(output io.Writer, req request, revision uint64, records []record) error {
	buffer := new(bytes.Buffer)
	buffer.WriteString("BCN\x00")
	writeU16(buffer, 1)
	buffer.Write([]byte{0, 0})
	writeU32(buffer, uint32(len(records)))
	buffer.Write(req.session[:])
	buffer.Write(req.manifest[:])
	buffer.Write(req.authority[:])
	binary.Write(buffer, binary.BigEndian, revision)
	writeU16(buffer, uint16(len(req.language)))
	buffer.WriteString(req.language)
	for _, record := range records {
		key, payload := []byte(record.key), []byte(record.value)
		buffer.Write([]byte{record.kind, 0})
		writeU32(buffer, uint32(len(key)))
		writeU32(buffer, uint32(len(payload)))
		buffer.Write(key)
		buffer.Write(payload)
	}
	_, err := output.Write(buffer.Bytes())
	return err
}

func readU16(reader io.Reader) (uint16, error) {
	var value uint16
	err := binary.Read(reader, binary.BigEndian, &value)
	return value, err
}
func readU32(reader io.Reader) (uint32, error) {
	var value uint32
	err := binary.Read(reader, binary.BigEndian, &value)
	return value, err
}
func writeU16(writer io.Writer, value uint16) { _ = binary.Write(writer, binary.BigEndian, value) }
func writeU32(writer io.Writer, value uint32) { _ = binary.Write(writer, binary.BigEndian, value) }
