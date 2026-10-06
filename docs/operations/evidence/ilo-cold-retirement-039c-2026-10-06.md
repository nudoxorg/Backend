# Real ILO first-cold-request diagnosis and observer repair

The reproduced Quart first-cold reset comes from the observer declaring a detached daemon stopped before its remaining kernel threads release the endpoint. It is not a demonstrated persisted-store admission failure. The repaired observer waits for kernel exit; the first cold request then succeeds on separate retained Quart and Click states. Other customer resets remain unclassified.

These are actual SSH executions on ILO, root@95.217.56.147, NixOS Linux 6.18.38 x86_64. No new binaries were built. All three immutable binaries match source `039c360d286962a6cf19488fb86caabd1d8c93de`, tree `a3911d62ef9c4ef1f8206c4fc05b8a8c6fde5156`, and runtime manifest SHA256 `a693977a9bcbf2e25c7b7ebb3c820d8a971ac5a190cf25708d608ee69c4c07f8`:

| Program | SHA256 |
| --- | --- |
| locald | 75a57be7880b1946281ec7dae5ee5d177132c4cf9e4dad0a0fefa319b4a1c0bb |
| CLI | 8bed4d798f032218a800e9533fd8be83759303f4117000ec2779a8469c1c6b7d |
| MCP | d6b58696fa516c89aee2c062cf388aae692ad5f56c5f507028e3ddbd2e0e2aeb |

The original Quart state is `runs/fresh-remote-runtime/20261006T112754Z-python-configured-0c54656f` beneath `/root/nudox-corpus-20261006`. The additional Click state is `20261006T113100Z-python-configured-e02a971f`. Every investigation uses its own copied state, private HOME and endpoint; the original project is read only so authenticated absolute source identities remain unchanged. Hash inventories prove original states and source trees stayed unchanged. Two cases ran in parallel with resource admission checks; baseline available memory was about 56.7 GiB and free disk about 55.1 GiB. No original owner was reused or terminated.

## Exact causal boundary

An initial genuinely stopped Quart clone admitted successfully: first CLI health exited 0 after 13.923 seconds under broad syscall tracing, then the second exited 0 after 0.141 seconds. A typing-extensions retained state from source 9f was explicitly tested as an older-state/new-039c-image combination. It refused DTO19 versus DTO20 with daemon startup exit 70 before binding; that is a schema refusal, not a reproduced connection reset or matched runtime acceptance.

The paired Quart control recreates the original observer boundary. After SIGTERM, old PID 1024771 has an empty cmdline and a zombie leader, but still reports five threads and a non-readable pidfd. The first CLI's probe and authenticated stream both connect successfully. `SO_PEERCRED` identifies that exact old PID; request 1 sends the actual DTO20 health frame, then `recvfrom` returns `ECONNRESET`. No new locald is executed in this failed first request. The failed frame is 90 bytes, including its four-byte length prefix; health carries no query root or certificate. The prior observed root is `c02acd20761f78b2ca946453ad2c21c4f1ae23dc491d80e145e1977193e24f25`.

| Retirement condition | First cold CLI health | Explicit second request |
| --- | --- | --- |
| Original cmdline-only observer | Exit 1, connection reset, 0.039 seconds | Exit 0, 4.036 seconds |
| Kernel pidfd exit control | Exit 0, 4.510 seconds | Exit 0, 0.096 seconds |

The kernel control waits until PID 1024770's pidfd becomes readable. The next CLI observes `ECONNREFUSED`, launches exact candidate locald PID 1025203, and completes admission. No retry or fixed readiness sleep substitutes for the first verdict. The product's fault text saying “a fresh connection was opened” does not establish that a new daemon or stream existed; this trace proves the failed query reached the old peer.

## Shared observer change and real retest

The shared observer base is the exact helper from SolFresh commit `fa62f8fd45`, file SHA256 `ce120d95cb3d3e39aea3d858b4f44a8b7c3a6f1399ba241bf0749c7c9823ad94`; its stop condition matches the frozen witness-v2 observer. `retire_owned_process` now opens a Linux pidfd, signals that exact identity with `pidfd_send_signal`, and requires kernel exit through a bounded 20-second poll before recording stopped. A retained Popen child can instead be waited on directly. Unsupported detached proof and timeout fail; cmdline disappearance and zombie state are never accepted as exit proof. This changes the observer, with no product reconnect or source-admission relaxation.

The repaired helper SHA256 is `bd2246da2fa9a61e431adb6bcd17a0e76b2293c9dc5ffa1c33d3546007c1132b`. Two parallel private clones invoked this actual function:

| Retained project | First cold health | Captured status | Warm/cold CLI and MCP |
| --- | --- | --- | --- |
| Quart 0.23.1 | Exit 0, 4.558 seconds | Sequence 9, 3,622 rows, root c02acd20… | Exact root/source/sequence/row equality |
| Click 8.5.0 | Exit 0, 6.989 seconds | Sequence 9, 7,529 rows, root 3853f8d5… | Exact root/source/sequence/row equality |

Both originals remain hash-unchanged and all owned daemons/MCP processes stopped. A final kernel connect probe over all six private endpoints returns only connection refused or absent, independently confirming no owned endpoint accepts a stream. Actual ignoring-SIGTERM processes exercise the bounded failure: Linux pidfd times out in 0.0501 seconds, and a local Mac retained child handle times out in 0.0507 seconds. Neither falsely reports stopped; both private children are then explicitly killed and reaped. The Mac child check is not remote Mac verification. Remote Mac SSH remains unavailable.

The packet `evidence/ilo-cold-retirement-039c-20261006/runtime.tar.gz` contains 123 verified hash-indexed members, exact raw failed trace/frame, original first-fault outputs, process/socket observations, passing syscall slices, raw MCP requests/replies, manifests and frozen harnesses. SHA256: `2f083bcdcfb5b3a1b8f198c0221a3abd05af5e9eed022d8bafa04954f7252ddb`. Complete unsliced traces remain at the recorded remote paths with original hashes. An initial export omitted the derived frame for short traces; validation caught it, and the initial archive remains preserved separately. Packet v2 includes and validates the exact failed frame. State secrets, caches and executable images are excluded.

This closes the demonstrated observer retirement race and its narrow first-cold status gate. Semantic coverage remains unavailable/unconfigured, and no whole-package, stock installer, pristine OS, or GUI acceptance is claimed.
