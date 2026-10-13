# Initial GUI root failure

A definitive actor failure of the initial Root previously left host observation waiting for its full timeout. The runtime now carries the exact submitted gate, ready attachment, request identity, and typed failure to the UI delivery boundary. Delivery rechecks the latest Root identity; Stop and replacement requests retire earlier receipts. The gate atomically refuses withdrawal for a different owner, a non-ready attachment, or any already-certified publication.

Cancellation, supersession, non-Root requests, mapping failures, and stale authority do not produce this carrier. It reports an observation failure rather than owner death and adds no mutation replay or timeout changes. Exhaustive enum consumers and both inflight constructors were reviewed.

Root source `95ee6da9972c778b27036b1430cfd54e67a6b8ed`, tree `2f0db8841578a9bf130a86fdfce686a4ce3f2c8e`, exactly matches author `88cad635b16421df954fbf85b2e32d8cbf179924`. Root read the complete atom and relevant lifecycle paths; Sol independently reviewed every enum consumer and delayed-delivery branch. Four postimages, syntax parsing, and diff checks pass. Source receipt SHA-256: `2ef0f59e3e29360fd0fac366e0c67e9553922d61092b6de3874ba0ad388a72ea`.

Five native laws are prepared, including the real host observation primitive with its original 60-second bound, cancellation, supersession, wrong attachment, publication winning over failure, and delayed new-Root/Stop delivery. The waiter signals before entering the pause, so the prepared law does not prove a thread was already parked inside the condition variable. Native/type checks and mounted GUI startup/retry/pixel flows remain **unrun**. This source repair does not establish recovery of Report 21's complete GUI journey.
