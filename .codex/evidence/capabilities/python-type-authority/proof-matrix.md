# Proof matrix — python-type-authority

law | weakened implementation | falsifier | required evidence | state | owner

R1 | Render text produced by re-reading source or re-parsing instead of from published IR lanes | Publish a fragment, reopen its bytes, mutate no source; render must be byte-identical, and a hand-built Ir must render a lane fact absent from source | `cargo test -p compiler-driver --test python_render` | RED | luna-render
R2 | Lifecycle skips real PyPI fetch or skips reopen/validation of older generations | Parse `pypi:six@2.16.0` PURL → fetch → unpack → detect → authorities → publish gen1; modify source → publish gen2; reopen both; gen1 validates and renders unchanged | `cargo test -p compiler-driver --test python_purl_lifecycle` | RED | luna-purl
R3 | Package tests assert only "no error" | requests/attrs/flask/six/wcwidth pinned versions: exact named declarations, parameter kinds, decorator facts, docstrings, occurrence tiers with pypi package keys, compound annotations asserted from decoded lanes | `cargo test -p compiler-driver --test python_packages` | RED | luna-packages
R4 | Overloads collapse or lose their decorator/extension evidence | `typing.overload` defs of one name land as distinct typed rows, each carrying the overload decorator atom; implementation row distinct from overload rows | lane test in compiler/languages/python/tests or driver python test | RED | luna-packages
R5 | Edge-case fixes weaken instead of generalize | Each discovered real-package edge case returns as a one-card repair with a red lane test first | repair card commits | RED | terra
R6 | Bounded pyrefly transaction regresses (timeout/limit/spawn terminals) | Existing checker tests stay green; unavailable pyrefly degrades lane to syntax tiers honestly | `cargo test -p compiler-languages-python` | PROVED BY WORKER (35/35 green at baseline) | prior checkpoint
R7 | Publication round-trip of python fragments already green | Application journey python publish path | `cargo test -p compiler-application --test local_compiler` | REPRODUCED BY TERRA (green run 2026-09-02) | prior checkpoint
