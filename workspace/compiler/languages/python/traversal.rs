//! Version resolution over PyPI / git / local source.
//!
//! IMPLEMENT HERE:
//!   - Walking PEP 440 versions on PyPI (JSON API) newest → oldest.
//!   - Or git-tag based resolution for dependencies pinned in `pyproject.toml`.
//!   - Returning the first commit / sdist whose metadata matches the requested
//!     version range, materialised into a workspace for pyrefly checking.
