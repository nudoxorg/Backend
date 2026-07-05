PYREFLY_REV = "3e17a690edbde7d1f2341864d6f1c6523ba207c5"
RUFF_REV = "db5aa0a5f1b92cb91d910bf0866a967554dd94f5"
TERMINUS_REV = "4fefb0434f65baa847af5af9fc00175b3e4d827b"
LSP_TYPES_REV = "395d6bfcd6c3696a64cfe9cd93b86f981fb85112"

GIT = [
    {
        "archive_name": "pyrefly-repo",
        "urls": ["https://github.com/facebook/pyrefly/archive/" + PYREFLY_REV + ".tar.gz"],
        "sha256": "3356755ba96cfc1cd31925f95ad12937974b7aa7eacdf8bcb8d99fa14cc1d020",
        "strip_prefix": "pyrefly-" + PYREFLY_REV,
        "crates": [
            {"name": "pyrefly", "subdir": "pyrefly"},
            {"name": "pyrefly_types", "subdir": "pyrefly_types"},
            {"name": "pyrefly_build", "subdir": "pyrefly_build"},
            {"name": "pyrefly_config", "subdir": "pyrefly_config"},
            {"name": "pyrefly_python", "subdir": "pyrefly_python"},
            {"name": "pyrefly_util", "subdir": "pyrefly_util"},
        ],
    },
    {
        "archive_name": "ruff-repo",
        "urls": ["https://github.com/astral-sh/ruff/archive/" + RUFF_REV + ".tar.gz"],
        "sha256": "8f4e600a1b71abce71f49ae3e1438e404fa0e668d755721532b91bcdf4bb6e2f",
        "strip_prefix": "ruff-" + RUFF_REV,
        "crates": [
            {"name": "ruff_python_ast", "subdir": "crates/ruff_python_ast"},
            {"name": "ruff_python_parser", "subdir": "crates/ruff_python_parser"},
            {"name": "ruff_python_trivia", "subdir": "crates/ruff_python_trivia"},
            {"name": "ruff_source_file", "subdir": "crates/ruff_source_file"},
            {"name": "ruff_text_size", "subdir": "crates/ruff_text_size"},
            {"name": "ruff_cache", "subdir": "crates/ruff_cache"},
            {"name": "ruff_diagnostics", "subdir": "crates/ruff_diagnostics"},
            {"name": "ruff_notebook", "subdir": "crates/ruff_notebook"},
            {"name": "ruff_annotate_snippets", "subdir": "crates/ruff_annotate_snippets"},
        ],
    },
    {
        "archive_name": "terminusdb-rs-repo",
        "urls": ["https://github.com/ParapluOU/terminusdb-rs/archive/" + TERMINUS_REV + ".tar.gz"],
        "sha256": "b46576a8fb3be1ffab03a244084caf52766fc9e1d1bcf3e7d2b83076e0cd359e",
        "strip_prefix": "terminusdb-rs-" + TERMINUS_REV,
        "crates": [
            {
                "name": "terminusdb_schema",
                "subdir": "crates/schema",
                "edition": "2018",
                "patch": "remove-rocket.patch",
            },
            {
                "name": "terminusdb_schema_derive",
                "subdir": "crates/schema-derive",
                "edition": "2018",
                "proc_macro": True,
            },
            {
                "name": "typestate",
                "subdir": "crates/typestate",
                "edition": "2018",
            },
        ],
    },
    {
        "archive_name": "lsp-types-repo",
        "urls": ["https://github.com/yangdanny97/lsp-types/archive/" + LSP_TYPES_REV + ".tar.gz"],
        "sha256": "2c9984223652831a3a49e689bd649ca5ed7898b764747890f2655c1ca52272e8",
        "strip_prefix": "lsp-types-" + LSP_TYPES_REV,
        "crates": [
            {"name": "lsp_types", "subdir": ""},
        ],
    },
]
