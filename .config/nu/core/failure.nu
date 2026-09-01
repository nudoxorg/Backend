# Defines the command surface's single structured failure constructor.
# Gives every tooling rejection a stable category and actionable recovery.
# Prevents subprocess text from becoming an untyped success or error signal.

# Raises a machine-classified tooling error with optional recovery and causes.
def tooling-fail [
    code: string
    summary: string
    recovery?: string
    --inner: list<record> = []
]: nothing -> error {
    let failure = {
        code: $"backend::($code)"
        msg: $summary
        inner: $inner
    }
    if $recovery == null {
        error make $failure
    } else {
        error make ($failure | insert help $recovery)
    }
}
