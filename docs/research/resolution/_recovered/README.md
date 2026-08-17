# Recovered agent artifacts

| Agent | ID | Status at session limit |
|---|---|---|
| Internal resolution architecture audit | `afaa7790f40fb2b50` | **Complete** final report (~33KB). |
| Prior-art resolution research | `af03c686d58e1338b` | **Incomplete** — launched 5 angle agents then rate-limited (429). |
| Angle 1 Stack graphs | `a6993454b6fc4822c` | Partial tool results; no final synthesis. |
| Angle 2 SCIP/LSIF | `a68853925b2ddd28a` | Partial tool results; no final synthesis. |
| Angle 3 Kythe/Glean | `a3d5f5efc4f2dcc82` | Partial tool results; no final synthesis. |
| Angle 4 tree-sitter/oracle | `ab2ed38334b9ddea0` | Partial tool results; no final synthesis. |
| Angle 5 hybrid/usage mining | `a0b45a72c2fdf6e5e` | Mostly sub-launches; rate-limited. |

Parent session: `17cc2883-6b32-44ea-abc5-c1c9f07d0bf2`  
User goal: convert treesitter of arbitrary codebases into IR-resolved occurrences for example usage / instances across all languages.

Finished deliverables (this directory's parents):
- `../01-internal-architecture-audit.md` — verified ground-truth audit
- `../02-prior-art-resolution.md` — completed prior-art report (synthesized from angle tool results + existing industrial research + verification)
- `../03-exhaustive-plan.md` — reconciling implementation plan
