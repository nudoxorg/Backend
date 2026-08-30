# Shared capability journals

Each active capability owns exactly one journal:

```text
.codex/learning/capabilities/<capability-id>/insights.md
```

Create it with the first red test or implementation commit. Luna, Terra, the Terra reviewer, and Sol
append to the same document; do not create role-specific reports. Copy this minimal shape:

```markdown
# <capability> insights

## Observed

## Explained

## Corrected

## Promoted
```

Each issue uses the schema in `../../AGENT_LEARNING.md`. Keep entries causal and terse. Journal work
accompanies technical work and never authorizes, scores, or blocks implementation.
