// Package interfaces exercises Go interface declarations, embedded interfaces,
// generics (Go 1.18+ type parameters), and constraint type sets.
package interfaces

// Reader is a simple exported interface.
//
// It has one required method with a single parameter.
type Reader interface {
	// Read fills p and returns the count and any error.
	Read(p []byte) (n int, err error)
}

// Writer is a complementary interface.
type Writer interface {
	Write(p []byte) (n int, err error)
}

// ReadWriter embeds both Reader and Writer (method-set union via embedding).
type ReadWriter interface {
	Reader
	Writer
}

// Closer closes a resource.
type Closer interface {
	Close() error
}

// ReadWriteCloser embeds all three.
type ReadWriteCloser interface {
	ReadWriter
	Closer
}

// Stringer is a common single-method interface with no parameters.
type Stringer interface {
	String() string
}

// Container is a generic interface constrained to ordered types (Go 1.18+).
//
// T must satisfy the Ordered constraint.
type Container[T Ordered] interface {
	Add(item T)
	Len() int
	Get(i int) T
}

// Ordered is a constraint type set: integers, floats, and strings.
type Ordered interface {
	~int | ~int8 | ~int16 | ~int32 | ~int64 |
		~uint | ~uint8 | ~uint16 | ~uint32 | ~uint64 |
		~float32 | ~float64 | ~string
}

// Stack is a generic struct implementing a container pattern.
//
// Items are stored in a slice and the zero value is ready to use.
type Stack[T any] struct {
	items []T
}

// Push adds an item to the top of the stack with a pointer receiver.
func (s *Stack[T]) Push(item T) {
	s.items = append(s.items, item)
}

// Pop removes and returns the top item, and whether it was present.
func (s *Stack[T]) Pop() (T, bool) {
	if len(s.items) == 0 {
		var zero T
		return zero, false
	}
	top := s.items[len(s.items)-1]
	s.items = s.items[:len(s.items)-1]
	return top, true
}

// Transform applies a function to every item in a slice and returns results.
//
// This exercises a top-level generic function with multiple type parameters.
func Transform[In, Out any](items []In, fn func(In) Out) []Out {
	out := make([]Out, len(items))
	for i, v := range items {
		out[i] = fn(v)
	}
	return out
}

// Map is a named type over a generic map, demonstrating type params on a named type.
type Map[K comparable, V any] map[K]V

// unexportedHelper is private and should lower with unexported visibility.
func unexportedHelper(s string) string {
	return s
}
