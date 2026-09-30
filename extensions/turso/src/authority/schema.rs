//! Greenfield tables for selected package-index generations.
//!
//! These tables deliberately live beside, but outside, the disposable search
//! and graph projection schema. The generation head is the durable selection
//! authority; catalog, graph, and lexical indexes report their own exact
//! watermarks so a crash between authority commit and projection repair is
//! visible instead of being mistaken for a current answer.

pub(super) const AUTHORITY_SCHEMA_VERSION: i64 = 6;

pub(super) const AUTHORITY_SCHEMA: &str = r"
CREATE TABLE IF NOT EXISTS backend_index_authority_meta (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    schema_version INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS backend_index_authority_scopes (
    package TEXT NOT NULL,
    source TEXT NOT NULL,
    branch TEXT NOT NULL,
    environment TEXT NOT NULL,
    plane_kind INTEGER NOT NULL CHECK (plane_kind IN (0, 1)),
    profile TEXT NOT NULL,
    attempt_epoch INTEGER NOT NULL DEFAULT 0 CHECK (attempt_epoch >= 0),
    latest_attempt BLOB,
    latest_observation INTEGER NOT NULL DEFAULT 0 CHECK (latest_observation >= 0),
    PRIMARY KEY (package, source, branch, environment, plane_kind, profile),
    CHECK (length(package) > 0 AND length(source) > 0 AND length(branch) > 0 AND length(environment) > 0),
    CHECK ((plane_kind = 0 AND length(profile) = 0) OR (plane_kind = 1 AND length(profile) > 0))
);

CREATE TABLE IF NOT EXISTS backend_index_authority_observations (
    package TEXT NOT NULL,
    source TEXT NOT NULL,
    branch TEXT NOT NULL,
    environment TEXT NOT NULL,
    plane_kind INTEGER NOT NULL CHECK (plane_kind IN (0, 1)),
    profile TEXT NOT NULL,
    sequence INTEGER NOT NULL CHECK (sequence > 0),
    revision BLOB,
    observed_at_ms INTEGER NOT NULL CHECK (observed_at_ms >= 0),
    value_kind INTEGER NOT NULL CHECK (value_kind IN (1, 2, 3)),
    known_count INTEGER,
    unavailable_reason TEXT,
    PRIMARY KEY (package, source, branch, environment, plane_kind, profile, sequence),
    CHECK ((plane_kind = 0 AND length(profile) = 0) OR (plane_kind = 1 AND length(profile) > 0)),
    CHECK ((revision IS NULL) OR length(revision) = 32),
    CHECK ((value_kind = 1 AND known_count IS NOT NULL AND known_count >= 0 AND unavailable_reason IS NULL)
        OR (value_kind = 2 AND known_count IS NULL AND unavailable_reason IS NULL)
        OR (value_kind = 3 AND known_count IS NULL AND unavailable_reason IS NOT NULL))
);

CREATE TABLE IF NOT EXISTS backend_index_authority_attempts (
    package TEXT NOT NULL,
    source TEXT NOT NULL,
    branch TEXT NOT NULL,
    environment TEXT NOT NULL,
    plane_kind INTEGER NOT NULL CHECK (plane_kind IN (0, 1)),
    profile TEXT NOT NULL,
    attempt_id BLOB NOT NULL CHECK (length(attempt_id) = 16),
    epoch INTEGER NOT NULL CHECK (epoch > 0),
    attempt_fence BLOB NOT NULL CHECK (length(attempt_fence) = 32),
    input_digest BLOB NOT NULL CHECK (length(input_digest) = 32),
    base_generation INTEGER NOT NULL CHECK (base_generation >= 0),
    base_root BLOB,
    observation_sequence INTEGER NOT NULL CHECK (observation_sequence > 0),
    state INTEGER NOT NULL CHECK (state IN (0, 1)),
    PRIMARY KEY (package, source, branch, environment, plane_kind, profile, attempt_id),
    UNIQUE (package, source, branch, environment, plane_kind, profile, epoch),
    CHECK ((plane_kind = 0 AND length(profile) = 0) OR (plane_kind = 1 AND length(profile) > 0)),
    CHECK ((base_generation = 0 AND base_root IS NULL)
        OR (base_generation > 0 AND base_root IS NOT NULL AND length(base_root) = 32))
);

-- Immutable terminal receipts keep cancelled/refused attempts out of the
-- recoverable state-0 table without requiring a rewrite of existing authority
-- tables. The exact input and attempt fences remain available for stale-result
-- proofs after the active row is retired.
CREATE TABLE IF NOT EXISTS backend_index_authority_attempt_terminals (
    package TEXT NOT NULL,
    source TEXT NOT NULL,
    branch TEXT NOT NULL,
    environment TEXT NOT NULL,
    plane_kind INTEGER NOT NULL CHECK (plane_kind IN (0, 1)),
    profile TEXT NOT NULL,
    attempt_id BLOB NOT NULL CHECK (length(attempt_id) = 16),
    epoch INTEGER NOT NULL CHECK (epoch > 0),
    attempt_fence BLOB NOT NULL CHECK (length(attempt_fence) = 32),
    input_digest BLOB NOT NULL CHECK (length(input_digest) = 32),
    base_generation INTEGER NOT NULL CHECK (base_generation >= 0),
    base_root BLOB,
    observation_sequence INTEGER NOT NULL CHECK (observation_sequence > 0),
    terminal_reason INTEGER NOT NULL CHECK (terminal_reason IN (1, 2, 3, 4)),
    PRIMARY KEY (package, source, branch, environment, plane_kind, profile, attempt_id),
    UNIQUE (package, source, branch, environment, plane_kind, profile, epoch),
    CHECK ((plane_kind = 0 AND length(profile) = 0) OR (plane_kind = 1 AND length(profile) > 0)),
    CHECK ((base_generation = 0 AND base_root IS NULL)
        OR (base_generation > 0 AND base_root IS NOT NULL AND length(base_root) = 32))
);

CREATE TRIGGER IF NOT EXISTS backend_index_authority_attempt_terminal_immutable_update
BEFORE UPDATE ON backend_index_authority_attempt_terminals
BEGIN
    SELECT RAISE(ABORT, 'terminal compiler attempt is immutable');
END;

CREATE TRIGGER IF NOT EXISTS backend_index_authority_attempt_terminal_immutable_delete
BEFORE DELETE ON backend_index_authority_attempt_terminals
BEGIN
    SELECT RAISE(ABORT, 'terminal compiler attempt is immutable');
END;

-- A no-result maintenance barrier is distinct from a compiler attempt. It is
-- minted by the same serialized authority lane, receives an epoch above all
-- candidate attempts and earlier barriers, and retires only its exact terminal
-- epoch. A newer active compiler candidate remains eligible to publish.
CREATE TABLE IF NOT EXISTS backend_index_authority_no_result_barriers (
    package TEXT NOT NULL,
    source TEXT NOT NULL,
    branch TEXT NOT NULL,
    environment TEXT NOT NULL,
    plane_kind INTEGER NOT NULL CHECK (plane_kind IN (0, 1)),
    profile TEXT NOT NULL,
    terminal_work_id BLOB NOT NULL CHECK (length(terminal_work_id) = 16),
    terminal_epoch INTEGER NOT NULL CHECK (terminal_epoch > 0),
    terminal_fence BLOB NOT NULL CHECK (length(terminal_fence) = 32),
    barrier_work_id BLOB NOT NULL CHECK (length(barrier_work_id) = 16),
    barrier_epoch INTEGER NOT NULL CHECK (barrier_epoch > terminal_epoch),
    barrier_fence BLOB NOT NULL CHECK (length(barrier_fence) = 32),
    retired_through_epoch INTEGER NOT NULL CHECK (retired_through_epoch >= terminal_epoch),
    PRIMARY KEY (package, source, branch, environment, plane_kind, profile,
                 terminal_epoch, terminal_fence),
    CHECK ((plane_kind = 0 AND length(profile) = 0) OR (plane_kind = 1 AND length(profile) > 0)),
    CHECK (retired_through_epoch < barrier_epoch)
);

CREATE TABLE IF NOT EXISTS backend_index_authority_frontiers (
    package TEXT NOT NULL,
    source TEXT NOT NULL,
    branch TEXT NOT NULL,
    environment TEXT NOT NULL,
    plane_kind INTEGER NOT NULL CHECK (plane_kind IN (0, 1)),
    profile TEXT NOT NULL,
    generation INTEGER NOT NULL CHECK (generation > 0),
    candidate_id BLOB NOT NULL CHECK (length(candidate_id) = 32),
    attempt_id BLOB NOT NULL CHECK (length(attempt_id) = 16),
    attempt_epoch INTEGER NOT NULL CHECK (attempt_epoch > 0),
    attempt_fence BLOB NOT NULL CHECK (length(attempt_fence) = 32),
    input_digest BLOB NOT NULL CHECK (length(input_digest) = 32),
    target_root BLOB NOT NULL CHECK (length(target_root) = 32),
    pack_id BLOB NOT NULL CHECK (length(pack_id) = 32),
    closure_id BLOB NOT NULL CHECK (length(closure_id) = 32),
    semantic_manifest_root BLOB CHECK (semantic_manifest_root IS NULL OR length(semantic_manifest_root) = 32),
    observation_sequence INTEGER NOT NULL CHECK (observation_sequence > 0),
    selection_origin INTEGER NOT NULL CHECK (selection_origin IN (0, 1, 2)),
    PRIMARY KEY (package, source, branch, environment, plane_kind, profile),
    UNIQUE (package, source, branch, environment, plane_kind, profile, generation),
    CHECK (semantic_manifest_root IS NULL OR length(semantic_manifest_root) = 32),
    CHECK ((plane_kind = 0 AND length(profile) = 0) OR (plane_kind = 1 AND length(profile) > 0))
);

CREATE TABLE IF NOT EXISTS backend_index_authority_generation_history (
    package TEXT NOT NULL,
    source TEXT NOT NULL,
    branch TEXT NOT NULL,
    environment TEXT NOT NULL,
    plane_kind INTEGER NOT NULL CHECK (plane_kind IN (0, 1)),
    profile TEXT NOT NULL,
    generation INTEGER NOT NULL CHECK (generation > 0),
    candidate_id BLOB NOT NULL CHECK (length(candidate_id) = 32),
    attempt_id BLOB NOT NULL CHECK (length(attempt_id) = 16),
    attempt_epoch INTEGER NOT NULL CHECK (attempt_epoch > 0),
    attempt_fence BLOB NOT NULL CHECK (length(attempt_fence) = 32),
    input_digest BLOB NOT NULL CHECK (length(input_digest) = 32),
    target_root BLOB NOT NULL CHECK (length(target_root) = 32),
    pack_id BLOB NOT NULL CHECK (length(pack_id) = 32),
    closure_id BLOB NOT NULL CHECK (length(closure_id) = 32),
    semantic_manifest_root BLOB CHECK (semantic_manifest_root IS NULL OR length(semantic_manifest_root) = 32),
    observation_sequence INTEGER NOT NULL CHECK (observation_sequence > 0),
    selection_origin INTEGER NOT NULL CHECK (selection_origin IN (0, 1, 2)),
    PRIMARY KEY (package, source, branch, environment, plane_kind, profile, generation),
    CHECK (semantic_manifest_root IS NULL OR length(semantic_manifest_root) = 32),
    CHECK ((plane_kind = 0 AND length(profile) = 0) OR (plane_kind = 1 AND length(profile) > 0))
);

CREATE TRIGGER IF NOT EXISTS backend_index_authority_generation_history_immutable_update
BEFORE UPDATE ON backend_index_authority_generation_history
BEGIN
    SELECT RAISE(ABORT, 'selected generation history is immutable');
END;

CREATE TRIGGER IF NOT EXISTS backend_index_authority_generation_history_immutable_delete
BEFORE DELETE ON backend_index_authority_generation_history
BEGIN
    SELECT RAISE(ABORT, 'selected generation history is immutable');
END;

CREATE TABLE IF NOT EXISTS backend_index_authority_projection_watermarks (
    package TEXT NOT NULL,
    source TEXT NOT NULL,
    branch TEXT NOT NULL,
    environment TEXT NOT NULL,
    plane_kind INTEGER NOT NULL CHECK (plane_kind IN (0, 1)),
    profile TEXT NOT NULL,
    projector TEXT NOT NULL CHECK (projector IN ('catalog', 'graph', 'lexical')),
    selected_generation INTEGER NOT NULL CHECK (selected_generation > 0),
    selected_root BLOB NOT NULL CHECK (length(selected_root) = 32),
    projected_generation INTEGER,
    projected_root BLOB,
    state INTEGER NOT NULL CHECK (state IN (0, 1)),
    PRIMARY KEY (package, source, branch, environment, plane_kind, profile, projector),
    CHECK ((plane_kind = 0 AND length(profile) = 0) OR (plane_kind = 1 AND length(profile) > 0)),
    CHECK ((state = 0 AND projected_generation IS NULL AND projected_root IS NULL)
        OR (state = 1 AND projected_generation = selected_generation
            AND projected_root = selected_root))
);
";
