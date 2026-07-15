# HTTP helpers: wait loops, package POST, poll until Stored.

# Poll until GET $url returns success (or timeout).
export def wait-http [url: string, tries: int = 60] {
  mut i = 0
  while $i < $tries {
    let result = (^curl -sf --max-time 2 $url | complete)
    if $result.exit_code == 0 { return }
    sleep 250ms
    $i = $i + 1
  }
  error make { msg: $"timed out waiting for ($url)" }
}

# Same as wait-http but with basic auth.
export def wait-http-auth [url: string, user: string, pass: string, tries: int = 60] {
  mut i = 0
  while $i < $tries {
    let result = (^curl -sf --max-time 2 -u $"($user):($pass)" $url | complete)
    if $result.exit_code == 0 { return }
    sleep 250ms
    $i = $i + 1
  }
  error make { msg: $"timed out waiting for ($url) \(auth\)" }
}

# Classify GET /packages/:id body → lifecycle label.
# ResolutionState is externally tagged serde JSON:
#   {"Stored":{"hash":[…]}} | {"Progressing":"Compiling"} | …
export def state-label [path: string] {
  let s = open $path | get state
  let ty = ($s | describe)
  if ($ty | str starts-with "record") {
    let cols = ($s | columns)
    if ("Stored" in $cols) { "Stored"
    } else if ("Failed" in $cols) { "Failed"
    } else if ("DeadLettered" in $cols) { "DeadLettered"
    } else if ("Progressing" in $cols) { "Progressing"
    } else if ("Unindexed" in $cols) { "Unindexed"
    } else { "Unknown" }
  } else {
    $s | into string
  }
}

# POST /packages; returns package id. Writes response to $work/add-$label.json.
export def post-package [
  base_url: string
  work: string
  ecosystem: string
  name: string
  version: string
  label: string
] {
  print --stderr $"==> POST /packages \(($label): ($ecosystem)/($name)@($version)\)"
  let body = {
    ecosystem: $ecosystem
    name: $name
    version: $version
  } | to json
  let out = $work | path join $"add-($label).json"
  let code = (^curl -s -o $out -w "%{http_code}" -X POST $"($base_url)/packages" -H "Content-Type: application/json" -d $body | str trim)

  if $code != "200" {
    print --stderr $"POST /packages \(($label)\) expected 200, got ($code):"
    try { open --raw $out | print --stderr } catch { }
    error make { msg: $"POST /packages \(($label)\) failed with HTTP ($code)" }
  }

  let resp = open $out
  let pkg = ($resp.package? | default "")
  if ($pkg | is-empty) {
    error make { msg: $"POST /packages \(($label)\) missing package id" }
  }
  let enqueued = ($resp.enqueued? | default "?")
  print --stderr $"    200 package=($pkg) enqueued=($enqueued)"
  $pkg
}

# Poll GET /packages/:id until Stored; fail on Failed/DeadLettered or deadline.
export def await-stored [
  base_url: string
  work: string
  pkg: string
  label: string
  deadline_secs: int = 300
] {
  let started = (date now)
  print $"==> polling GET /packages/($pkg) until Stored \(($label), deadline ($deadline_secs)s\)"
  loop {
    let out = $work | path join $"status-($label).json"
    let code = (^curl -s -o $out -w "%{http_code}" $"($base_url)/packages/($pkg)" | str trim)
    if $code == "200" {
      let label_state = (state-label $out)
      print $"    state=($label_state)"
      if $label_state == "Stored" {
        print $"    Stored ok for ($label)"
        return
      } else if $label_state == "Failed" or $label_state == "DeadLettered" {
        print --stderr $"pipeline terminal failure for ($label) \(($label_state)\):"
        try { open $out | to json | print --stderr } catch { open --raw $out | print --stderr }
        error make { msg: $"pipeline terminal failure for ($label): ($label_state)" }
      }
    } else {
      print $"    GET /packages/($pkg) → HTTP ($code) \(retrying\)"
    }

    let elapsed_ns = ((date now) - $started | into int)
    let elapsed = $elapsed_ns / 1_000_000_000
    if $elapsed > $deadline_secs {
      print --stderr $"timed out after ($elapsed)s waiting for Stored \(($label)\)"
      try { open $out | to json | print --stderr } catch { }
      error make { msg: $"timed out waiting for Stored \(($label)\)" }
    }
    sleep 1sec
  }
}
