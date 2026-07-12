// Package basics exercises exported vs unexported identifiers, struct fields,
// named types, type aliases, slices, maps, channels, and pointers.
package basics

import "time"

// Server is an exported struct with mixed exported/unexported fields and struct tags.
type Server struct {
	// Addr is the listen address.
	Addr string `json:"addr"`
	// Port is the port number.
	Port int `json:"port"`
	// timeout is an unexported field.
	timeout time.Duration
}

// Close shuts down the server with a pointer receiver.
func (s *Server) Close() error {
	return nil
}

// Status returns the current status string with a value receiver.
func (s Server) Status() string {
	return s.Addr
}

// Config is an exported struct that embeds Server.
type Config struct {
	Server
	// Name identifies the configuration.
	Name string `json:"name"`
}

// Direction is an iota-based enum (Go convention: defined type + iota consts).
type Direction int

const (
	North Direction = iota
	East
	South
	West
)

// Celsius is a named (defined) type over float64 (becomes a newtype RecordType).
type Celsius float64

// Temperature is a true type alias.
type Temperature = Celsius

// StringSlice is a named type over a slice.
type StringSlice []string

// StringMap is a named type over a map.
type StringMap map[string]string

// Chan is a named type over a channel.
type Chan chan string

// Ptr is a named type over a pointer.
type Ptr *Server

// MaxRetries is an exported constant.
const MaxRetries = 3

// defaultTimeout is an unexported constant.
const defaultTimeout = 30

// Count is an exported package-level variable.
var Count int

// Process is a top-level exported function: multiple params, multiple returns.
//
// It accepts a name and a count and returns the processed string and an error.
func Process(name string, count int) (string, error) {
	return name, nil
}

// Variadic accepts a variadic argument.
func Variadic(prefix string, items ...string) []string {
	return nil
}

// namedReturn demonstrates named return values (unexported, for coverage).
func namedReturn(n int) (result string, err error) {
	return
}
