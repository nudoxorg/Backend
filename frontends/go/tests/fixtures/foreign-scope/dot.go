package foreignscope

import . "example.com/foreignscope/exports"

func Dot() External {
	_ = Limit
	_ = Default
	return Make()
}

func DotShadow() {
	type External struct{}
	const Limit = 1
	var Default External
	_ = Limit
	_ = Default
}
