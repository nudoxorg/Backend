# Cargo provenance and permit gate

The first protocol run failed because its simulated compiler released the only permit after one second, before the second refusal assertion ran. The successor keeps that lease until both refusals have been observed, then releases it explicitly. Both raw logs are retained. The complete successor protocol passed, including pools of four and six, overflow refusal, role graph isolation, exact compiler provenance, stale lease recovery, and cache daemon isolation. Cargo is simulated here; this does not claim a workspace build or fleet-wide capacity reservation.

The default stays four permits; six is explicit. Retained role graphs remain independently bounded at four. Fleet admission is a separate, stricter policy.
