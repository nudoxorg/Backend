# Wave B4 immutable-index raw source-isolated sidecar attempts

## Common custody facts

```text
source root: /private/tmp/b4-sidecar-source.7Ymsv3
source aggregate SHA-256 before: 13e7bf55ad0ca948e2cf5867f72192d03f58b0c2ac675bc2edc7221477cfd03e
source aggregate SHA-256 after:  13e7bf55ad0ca948e2cf5867f72192d03f58b0c2ac675bc2edc7221477cfd03e
packet SHA-256: ded9b7ce69a4307fe593448194957ae2cf3b41cc5384abef1826a650e81b19d1
effective requested sandbox: workspace-write
explicit writable roots override: []
implicit TMPDIR writes excluded: true
implicit /tmp writes excluded: true
```

## Attempt 1 raw terminal lines

```text
BUILD_ROOT=/private/tmp/b4-sidecar-build.RWdsAt
SOURCE_ROOT=/private/tmp/b4-sidecar-source.7Ymsv3
SOURCE_BEFORE=13e7bf55ad0ca948e2cf5867f72192d03f58b0c2ac675bc2edc7221477cfd03e
{"type":"thread.started","thread_id":"01a0514e-3d25-7201-836c-33473be48957"}
{"type":"turn.started"}
{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"I’m locating the registered project configuration and packet metadata, then I’ll dispatch the isolated reviewer with the exact runtime constraints and wait for its raw findings."}}
ERROR codex_core::tools::router: error=Unknown model `gpt-5.6-luna` for spawn_agent. Available models: gpt-5.6-sol, gpt-5.6-terra
{"type":"item.started","item":{"id":"item_1","type":"collab_tool_call","tool":"wait","sender_thread_id":"01a0514e-3d25-7201-836c-33473be48957","receiver_thread_ids":[],"prompt":null,"agents_states":{},"status":"in_progress"}}
```

## Attempt 2 raw terminal lines

```text
ALT_BUILD_ROOT=/private/tmp/b4-sidecar-alt-build.scm7ZJ
SOURCE_ROOT=/private/tmp/b4-sidecar-source.7Ymsv3
PACKET_SHA=ded9b7ce69a4307fe593448194957ae2cf3b41cc5384abef1826a650e81b19d1
{"type":"thread.started","thread_id":"01a0514f-3c9c-76d1-bd36-c32a380a2768"}
{"type":"turn.started"}
{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"I’ll dispatch the registered reviewer against only the supplied read-only snapshot and packet, then return its unmodified findings in a JSON receipt."}}
ERROR codex_core::tools::router: error=Unknown model `gpt-5.6-luna` for spawn_agent. Available models: gpt-5.6-sol, gpt-5.6-terra
{"type":"item.started","item":{"id":"item_1","type":"collab_tool_call","tool":"wait","sender_thread_id":"01a0514f-3c9c-76d1-bd36-c32a380a2768","receiver_thread_ids":[],"prompt":null,"agents_states":{},"status":"in_progress"}}
```

## Attempt 3 and requested continuation raw terminal lines

```text
TERRA_BUILD_ROOT=/private/tmp/b4-sidecar-terra-build.uqj9ok
SOURCE_ROOT=/private/tmp/b4-sidecar-source.7Ymsv3
PACKET_SHA=ded9b7ce69a4307fe593448194957ae2cf3b41cc5384abef1826a650e81b19d1
{"type":"thread.started","thread_id":"01a05150-31ac-7353-bef9-213f969a5443"}
{"type":"turn.started"}
{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"I’ll act purely as the dispatcher: validate the registered reviewer configuration and packet digest, launch the source-isolated reviewer, then return its findings verbatim."}}
{"type":"item.started","item":{"id":"item_1","type":"collab_tool_call","tool":"wait","sender_thread_id":"01a05150-31ac-7353-bef9-213f969a5443","receiver_thread_ids":[],"prompt":null,"agents_states":{},"status":"in_progress"}}

$ codex exec resume 01a05150-31ac-7353-bef9-213f969a5443 ...
Error: thread/resume: thread/resume failed: no rollout found for thread id 01a05150-31ac-7353-bef9-213f969a5443 (code -32600)
```

The terminal also emitted repeated non-semantic rollout-cache warnings and a Rust environment
`unwrap()` panic containing the malformed value `"â\\x88\\x99"`. No reviewer child task ID, child
result, effective reviewer sandbox event, or findings was emitted in any attempt.
