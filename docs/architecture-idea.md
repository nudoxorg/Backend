## Dev Server
- Hosts the nixos cache for both packages and machines (basically instant start-up)
- Hosts all of our developer infrastructure
  - Git, comms, task tracking, etc.
  - CI/CD
  - Observability dashboards
- Handles internal authentication

## Production Server
- Serves the websites
- Handles the main registry
- Blob storage
- Handles authentication to all of our services

## Orchestration Server
- Manages instances of the compiler
  - Mainly acts as a load-balancer
  - NOT latency-aware
- Manages instances of the production server
  - Eventually we're going to need to have zoning for latency reasons, this would be handled here.
  - For the time being it's completely setup, but with only one zone (U.S)
- Pulls changes from the dev server to deploy the Nix machine configurations
- Regression/fuzz testing with extra compute
