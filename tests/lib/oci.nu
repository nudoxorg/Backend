# OCI / nix2container image-config assertions.

# Assert packages.backend (nix2container JSON) has the expected runtime config.
export def assert-backend-image [image_json: string] {
  print "==> asserting packages.backend OCI config"
  let cfg = open $image_json | get image-config

  if $cfg.Entrypoint != ["/bin/driver"] {
    error make { msg: $"expected Entrypoint [/bin/driver], got ($cfg.Entrypoint | to json)" }
  }
  if not ("8080/tcp" in ($cfg.ExposedPorts | columns)) {
    error make { msg: "expected ExposedPorts to include 8080/tcp" }
  }
  let image_env = $cfg.Env
  if not ("NUDOX_SERVING_ADDRESS=0.0.0.0:8080" in $image_env) {
    error make { msg: "missing NUDOX_SERVING_ADDRESS=0.0.0.0:8080 in image Env" }
  }
  let component = $cfg.Labels | get "org.nudox.component"
  if $component != "backend" {
    error make { msg: $"expected label org.nudox.component=backend, got ($component)" }
  }
  print "    image config ok (entrypoint /bin/driver, port 8080, env, labels)"
}
