#!/usr/bin/env python3
"""Record a source-bound Linux Stylo worker build in an admitted managed scope."""

from record_embedding_helper_build import main


if __name__ == "__main__":
    raise SystemExit(main(stylo=True))
