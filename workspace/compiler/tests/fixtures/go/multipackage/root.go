// Package multipackage is the root package of a multi-package module.
package multipackage

// Registry holds key-value entries.
type Registry struct {
	entries map[string]string
}

// NewRegistry creates an empty registry.
func NewRegistry() *Registry {
	return &Registry{entries: make(map[string]string)}
}

// Set stores a value under key.
func (r *Registry) Set(key, value string) {
	r.entries[key] = value
}

// Get retrieves the value for key.
func (r *Registry) Get(key string) (string, bool) {
	v, ok := r.entries[key]
	return v, ok
}
