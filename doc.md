


## The agent flow

- For most cases, it just wants to find api fast
"How do I do blank"

For those cases, we return a simple rendered version of the API, for familiarity and cheap token costs.

This looks like

fn rust_fn() -> RETURNS 

x 50 or whatever for all the results


### For the deeper queries

We enable the agent to get more information on demand

Stuff like

"For this symbol, what are its relations"

OR

"Whats the body of this function look like"

OR

"Give me examples of this in real codebases"


For those, they make use of the graph representation, alongside the qdrant store. These results still return rendered variants, but the full information is available always on request. (JSON IR)
