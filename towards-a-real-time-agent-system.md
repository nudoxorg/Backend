## Hivemind
The first party, triple A, ACD client.

- Imagine google search auto complete. As you're tying, you're watching all these paths of thought you could take assume form.
	- They're ranked by relevance/liklihood.
	- But they're completely stateless, and hold no context beyond you're input.
	- And you're expected to choose the best option based on the results.
- As people use agents, they are creating trees. Each decision and prompt is a fork in the road.
	- This naturally conforms to a tree-ui. No tabs, just leaves within a tree.
	- Trees share a parent, the project that you're working in. But they also share information. If you revisit a node, and change something there, it revisits any connected leaf in the tree, and adapts the previous work/context to the new migration.
- Every agent is alive, always.
	- The chat interface is largely an illusion. Agents can interrupt you, can propose multiple plans/ideas as you're still finishing your thought, or even begin to do the work in the background if the task is determined simple enough.
- Hivemind composes itself around a project, automatically culling dead leaves as they become irrelevant, spinning up new ones as it receives new tasks.
	- As it explores the codebase, it'll propose things that could be improved, things that could be changed -- according to your preferences.
	- Even though it will cull nodes, memory is first-class. If it needs to bring something back, it can and it will.
- Hivemind is built to be durable. And durable-first. This means that by whatever means possible, workflows are entirely reproducible. Every file edited is recorded, and given the same repository state, it can replicate those same changes.
	- This means that for sharing work between employees, the exact operations can be replayed to get to an identical state without first going through the git layer.
	- For certain smaller tasks, this also means that it will be capable of spinning up the work remotely and sending the information back from a sandbox, to save on IO and compute for your own computer.
