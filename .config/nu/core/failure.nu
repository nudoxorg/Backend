# Defines the command surface's single structured failure constructor.
# Gives every tooling rejection a stable category and actionable recovery.
# Prevents subprocess text from becoming an untyped success or error signal.

# Raises a categorized tooling error with an optional recovery hint.
def tooling-fail [category: string, message: string, recovery?: string] {
  let body = $"($category): ($message)"
  if $recovery == null {
    error make { msg: $body }
  } else {
    error make { msg: $body, help: $recovery }
  }
}
