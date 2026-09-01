# Generates foreign tool configuration from the evaluated Nix control plane.
# Keeps TOML and YAML as immutable transport formats rather than authoring surfaces.
# Exposes exact paths so every Nu command consumes the same configuration.
{ pkgs, control }:
let
  toml = pkgs.formats.toml { };
  yaml = pkgs.formats.yaml { };
in
{
  koji = toml.generate "backend-koji.toml" control.commit.koji;
  nextest = toml.generate "backend-nextest.toml" control.testing.nextest;
  otelCollector = yaml.generate "backend-otel-collector.yaml" control.observability.collector;
}
