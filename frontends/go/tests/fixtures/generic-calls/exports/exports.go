package exports

func One[T any](value T) T { return value }
func Two[A, B any](first A, second B) B { return second }
func Plain(value int) int { return value }
