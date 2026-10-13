package foreignscope

import renamed "example.com/foreignscope/exports"

type Holder struct {
	Field   renamed.External
	Alias   renamed.Alias
	Generic renamed.Generic[int]
}

func Renamed() renamed.External {
	_ = renamed.Limit
	_ = renamed.Default
	return renamed.Make()
}

func Methods() {
	var value renamed.External
	value.Method()
}

type LocalType struct{}
type LocalGeneric[T any] struct{ Value T }
type UsesError error

const LocalLimit = 3
const UsesIota = iota

var LocalDefault LocalType

func Local() LocalType {
	type Shadow struct{}
	const ShadowLimit = 4
	var value Shadow
	_ = value
	_ = ShadowLimit
	_ = len("x")
	_ = LocalLimit
	return LocalDefault
}

func GenericLocal[T any](value T) T { return value }

func Shadowed() {
	Limit := 1
	Default := 2
	External := 3
	_ = Limit
	_ = Default
	_ = External
}
