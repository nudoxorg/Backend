// Package parity is the old-oracle parity fixture.
package parity

// Service computes a value.
type Service interface { Run(value string) string }

// Worker implements Service.
type Worker struct{}

// Execute invokes the service.
func Execute(worker Service) string { return worker.Run("") }
