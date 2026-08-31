# Normalizes external command execution into typed Nushell records.
# Preserves program, arguments, status, standard output, and standard error.
# Makes every caller explicitly decide whether a failed process is acceptable.

# Executes a program and returns its complete observable process result.
def process-result [program: string, arguments: list<string> = []]: nothing -> record {
  let result = (run-external $program ...$arguments | complete)
  {
    program: $program
    arguments: $arguments
    status: $result.exit_code
    stdout: $result.stdout
    stderr: $result.stderr
  }
}

# Executes a program and raises a lossless error when it exits unsuccessfully.
def process-require [program: string, arguments: list<string> = []]: nothing -> record {
  let result = (process-result $program $arguments)
  if $result.status != 0 {
    let details = {
      program: $result.program
      arguments: $result.arguments
      status: $result.status
      stdout: $result.stdout
      stderr: $result.stderr
    } | to nuon
    tooling-fail "process-failed" $details
  }
  $result
}
