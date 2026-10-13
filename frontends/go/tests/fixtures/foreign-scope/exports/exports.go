package exports

type External struct{}
type Alias = External
type Generic[T any] struct{ Value T }

const Limit = 7

var Default External

func Make() External { return Default }

func (External) Method() {}
