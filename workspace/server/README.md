# The Server
The layer responsible for coordinating updates between subsystems, and the main(only) entrypoint for requests and the like

## Goals
- Providing a single interface for interacting with the data that we provide
- Reporting errors dutifully
- Providing for a full multitude of use-cases, especially MCP and GUI.
- Communicating and responding to the events it creates
	- It's the connective mesh between modules

## Non Goals
- Handling the implementation details for any particular interface/databse/system
	- This means that it's isolating from any of the database mechanisms, and can only be used/constructed when the assumptions has been verified that that infrastructure already exists and is ready to go.
- Worrying about anything related to the registry
