package genericcalls

import renamed "example.com/genericcalls/exports"

func ExplicitSingle() { _ = renamed.One[int](1) }
func ExplicitMultiple() { _ = renamed.Two[int, string](1, "x") }
func Inferred() { _ = renamed.One(1) }
func Parenthesized() { _ = (renamed.One[int])(1) }
func MultipleParentheses() { _ = ((renamed.Two[int, string]))(1, "x") }
func PlainParentheses() { _ = (renamed.Plain)(1) }
func InstantiatedValue() { _ = renamed.One[int] }

func LocalOne[T any](value T) T { return value }
func LocalExplicit() { _ = LocalOne[int](1) }
func LocalValue() { _ = LocalOne[int] }

var Callbacks []func(int) int

func Indexed() {
	_ = Callbacks[0](1)
	_ = (Callbacks[0])(2)
}

type Registry struct{ Callbacks []func(int) int }

var GlobalRegistry Registry

func FieldIndexed() { _ = GlobalRegistry.Callbacks[0](1) }

type LocalNamed[T ~int] int

func TypeConversion() { _ = LocalNamed[int](1) }
