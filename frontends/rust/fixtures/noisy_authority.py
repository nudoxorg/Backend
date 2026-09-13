#!/usr/bin/python3
"""Output-limit native authority fixture."""
import sys

sys.stdout.buffer.write(b"x" * 400_000)
sys.stdout.buffer.flush()
