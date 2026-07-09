"""nudox subcommand modules.

Each command module exposes:
  - ``add_arguments(sub: argparse.ArgumentParser) -> None``
  - ``run(args: argparse.Namespace) -> int``

(``verbs`` additionally exposes build/test-specific variants.)
"""

from __future__ import annotations

__all__ = ["add", "update", "check", "new", "verbs"]
