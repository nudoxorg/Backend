package genericcalls

import . "example.com/genericcalls/exports"

func DotSingle() { _ = One[int](1) }
func DotMultiple() { _ = Two[int, string](1, "x") }
