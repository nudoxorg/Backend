#!/usr/bin/python3
"""Malformed native authority output fixture."""
import sys

sys.stdout.buffer.write(b"compiler text is not BCN")
sys.stdout.buffer.flush()
