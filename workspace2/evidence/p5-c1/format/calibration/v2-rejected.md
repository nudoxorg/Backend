# Rejected C1-FORMAT card v2

The card digest `e88fe437c8fc739b82ebb13cf464d40b5f3a8f04a36653cff494796bca0416b2` was evaluated at
commit `6531ba7a85480b7f678b508e67f3c0adfc52c102`.

| Role | Task identity | Explicit model | Result |
| --- | --- | --- | --- |
| Independent reader | `/root/p5_c1_fragment_manager/c1_format_v2_cold_reader_one` | `gpt-5.6-luna`, non-inheriting | CLEAR: terminal, baseline/checkpoint custody, paths, public surface, falsifiers, and budgets agree. |
| Plausible misreader | `/root/p5_c1_fragment_manager/c1_format_v2_misreader` | `gpt-5.6-luna`, non-inheriting | Rejected because the mandated actual-rlib fixture was only represented by a helper declaration, making omission or same-crate substitution plausible. |

The successor freezes exact forged-view, raw-conversion, and legal-validation fixtures; artifact
selection/cardinality; expected diagnostics; and the legal mutation. All v2 readings are stale after
that semantic repair.
