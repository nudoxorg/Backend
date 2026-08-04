// Package fidelity exercises high-fidelity IR lowering contracts.
package fidelity

// Stringer is a single-method interface satisfied by Widget.
type Stringer interface {
	// String returns a human-readable representation.
	String() string
}

// Widget is a concrete type that implements Stringer.
//
// Deprecated: use NewWidget instead.
//
// See [Go] for details.
//
// [Go]: https://go.dev
type Widget struct {
	// Name labels the widget.
	Name string
	// Raw holds raw bytes (universe alias `byte`).
	Raw []byte
	// Octets is the same storage with the primitive spelling `uint8`.
	Octets []uint8
}

// String implements Stringer.
func (w Widget) String() string {
	return w.Name
}

// Close releases resources with a pointer receiver.
func (w *Widget) Close() error {
	return nil
}

// MaxRetries is a typed integer constant with an explicit value.
const MaxRetries int = 3

// Greeting is a string constant.
const Greeting = "hello"

// Enabled is a boolean constant.
const Enabled = true

// AliasName is a true type alias of Widget.
type AliasName = Widget

// Direction is an iota-based enum over int.
type Direction int

const (
	North Direction = iota
	East
	South
	West
)

// Ptr is a named pointer type over unsafe-adjacent territory (uses byte).
type Buffer []byte
