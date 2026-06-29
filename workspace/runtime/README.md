# Runtime

Our runtime data layer, which is backs all of the search functionality, either directly or through a thin abstraction.

## Goals
- To be the canonical store for all runtime-relevant data
	- This means that expected uptime here should be as close to 100% as possible.
- Very quick response time and uptime
- To handle the lifecycle of these components, including init and de-init

## Non Goals
- Persistence
- Coordination
