//! Resolving a Python package: local source tree discovery, PyPI metadata, and
//! materialising the package into pyrefly's module store.
//!
//! IMPLEMENT HERE:
//!   - `pyproject.toml` / `setup.py` / `setup.cfg` parsing (name, version, deps).
//!   - PyPI JSON API lookup for external packages → tarball download + extraction.
//!   - Building the list of pyrefly `Handle`s for all `.py`/`.pyi` files in the tree.
//!   - Mapping each file to pyrefly's `ModulePath::memory` or `ModulePath::source`.
//!   - Invoking pyrefly's `State::new` + `Transaction::run` to populate the type DB.
