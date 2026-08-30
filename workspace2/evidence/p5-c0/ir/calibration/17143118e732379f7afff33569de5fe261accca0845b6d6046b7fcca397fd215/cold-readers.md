# Luna cold-reader raw returns

Reader one (`/root/p5_c0_ir_manager_fresh/p5_c0_ir_variance_cold_reader_one`, `gpt-5.6-luna`,
`fork_turns: none`) verified the exact frozen tuple. It found the direct public type proof, 93/110/17
budget, 79-delta variance rule, exact-one resolver, target binding, literal mutation/predicate, and
known raw limitation clear. It rejected shadows, doctests, extra errors, noncausal mutations, stale
artifacts, reserve consumption, and source mutation. No blocker or ambiguity; host portability and
runtime evidence unverified. Verdict: **CALIBRATION-ONLY PASS**.

Reader two (`/root/p5_c0_ir_manager_fresh/p5_c0_ir_variance_cold_reader_two`, `gpt-5.6-luna`,
`fork_turns: none`) verified the same SHA. It found the folded resolver preserves zero/one/multiple
causality and the variance reference now coherent; source/rlib/stderr evidence remains unverified by
cold read. Verdict: **CALIBRATION-ONLY AGREE / no blocker or major ambiguity**.
