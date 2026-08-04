// Package sub is a sub-package within the multipackage module.
// It demonstrates that the context assembles multi-package modules correctly.
package sub

// Helper provides utility operations for the parent module.
type Helper struct {
	// Name is the helper's identifier.
	Name string
}

// Run executes the helper's primary action.
func (h *Helper) Run() error {
	return nil
}

// Describe returns a description string with a value receiver.
func (h Helper) Describe() string {
	return h.Name
}

// New constructs a Helper.
func New(name string) *Helper {
	return &Helper{Name: name}
}
