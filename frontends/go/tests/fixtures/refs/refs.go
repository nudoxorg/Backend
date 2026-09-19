// Package refs exercises every widened reference class the v4 oracle
// records: type uses, method calls, method values, field reads and writes,
// foreign calls and imports, and one structural satisfaction edge.
package refs

import "fmt"

// Greeter is satisfied structurally by Lang.
type Greeter interface {
	Greet() string
}

// Lang satisfies Greeter without naming it.
type Lang struct {
	Name string
	n    int
}

// Greet reads the exported field.
func (l *Lang) Greet() string {
	return "hello " + l.Name
}

// SetName writes the exported field.
func (l *Lang) SetName(name string) {
	l.Name = name
}

// Use calls a method, binds a method value, reads and writes an unexported
// field, and makes one foreign call through one imported package.
func Use() {
	l := &Lang{Name: "go"}
	l.SetName("go")
	fn := l.Greet
	l.n = 1
	if l.n < 0 {
		fn = nil
	}
	fmt.Println(l.Greet(), l.n)
	var g Greeter = l
	_ = g
	_ = fn
}
