# Live Ask keyboard flow rubric

Run `shell_capture` against a freshly admitted **real local index**, with a new evidence directory for each run. Repeat the native flow at 100%, 150%, and 200% text scale in both a small (480×900) and large (1440×900) window. The current `orbit-ask-keyboard-journey` is the 100%, large-window case; the remaining combinations are still acceptance work.

For every case, keep each frame's PNG, AccessKit tree, exact route/owner receipt, and rendered Reader text before evaluating it. Confirm that typed text reaches Ask's native TextInput; Tab and Shift-Tab remain among its mounted Links and editor; arrows preview the exact indexed routes; and the second Link's `lib.rs:103::RelationLabel` Reader settles to its own source text while native focus remains on that Link. Then confirm Enter, Code, Back, Escape, re-entry, exit, and live resize use the same keyboard path. A passing model fixture or route change alone does not establish this native result.
