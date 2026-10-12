# Original publication attempt diagnostics

Actual native102 source was `42b04e1024b6c0aac58f83873979e22254bb4438`.
The local-service run failed
`publication_worker_removes_native_selection_after_staging` at the generic
30-second terminal wait. Its captured output first shows the separate initial
native Python operation Published in 4.533 seconds; that does not time the
subsequent descriptor-volume publication. Both this control and the paired
synchronous legacy control were reported running for over 60 seconds. The
legacy control eventually passed. The publication test source and worker are
unchanged between that run and base `07f52a9cf3404f4c7464e29c274999717b2fba1a`.

The volume control starts with a genuine compiled Python selection, then builds
6,000 typed Unavailable records to cross the staged-intent boundary. It invokes
the real publication worker directly, with no new selected claims. The normal
dispatcher currently routes that empty-selected case through synchronous
`finish_add`. Neither fact proves that a 30-second worker completion is safe:
the same durable candidate, selector removal, view and projection must still
finish. The cause cannot be determined from the generic timeout alone.

This atom retains the 30-second wait, all 6,000 records, CAS staging assertion,
genuine native prerequisite, exact ticket terminal and selected-generation
removal assertions. It adds observations on the original owned worker only:
stage-intent, workspace candidate, candidate snapshot, view/read-head,
selector/barrier, owner grant, physical publication, projection/receipt and
payload retirement. Test-only observations retain a monotonic elapsed clock
and the accepted-grant bit; no lock spans publication work. They consume no
grant/completion event, admit no retry, change no receipt and select no new
basis. The existing held AfterSelection control verifies that observing the
original ticket-only worker preserves the old owner/view/cursor and reports
its actual accepted grant.

`phase` means the last work stage entered, not an active-stack sample. In the
held AfterSelection control, physical publication has returned while that
last-entered marker remains `physical-publish`. `elapsed` is total monotonic
time since the observation was created before writer reservation; it is not
time spent in the displayed stage. Neither value alone proves an I/O stall or
isolates a stage's cost. Completion disposition, retained head/grant state and
the original receipt must be read with the marker before choosing a repair.

On timeout, the existing read-only attempt diagnostic now includes these
observations, completion disposition, head state, proof/outcome, terminal,
captured compiler readiness and current original keyed receipt where one was
requested. A ticket-only job is explicitly identified without fabricating a
key or making a keyed status call.

This is a diagnostic repair, not a publication/performance fix. The actual
native102 failure remains **RED**. Rustfmt and `git diff --check` passed; Cargo
compile and every new assertion are **UNRUN**. The sole ILO broker must capture
the original failing phase in the next admitted cohort before an algorithm
change is justified. No timeout increase or volume reduction is proposed.
