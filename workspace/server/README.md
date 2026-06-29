# The Server
The layer responsible for coordinating updates between subsystems, and the main(only) entrypoint for requests and the like

##

*This means*
- Running the compiler to get an index over an input package, and then alerting the global store of the update, and dispensing the result into the registry.
- Pulling packages down from something
