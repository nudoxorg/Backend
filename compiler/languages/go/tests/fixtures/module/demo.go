package demo

type Inner struct{}
func (Inner) Read() {}
type Outer struct{ Inner }
const (
	First = iota
	Second
)
var CJK名前 = Outer{}
type Reader interface{ Read() }
