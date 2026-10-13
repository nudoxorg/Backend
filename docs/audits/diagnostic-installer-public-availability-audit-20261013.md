# Public installer and checkpoint readback — 2026-10-13

Anonymous HTTPS readback was performed on October 13, 2026, beginning at 01:41 UTC. The local source checkout at the time was `canonical` commit `8b27d833f71532a73a4d8b660661227023e9892b`, tree `568249160308d91bb4a4d97ffb1c46e57211b75b`. This note records public artifact availability; it does not claim that those artifacts contain or validate that source checkout.

## Forgejo R2 channel

| Request | Result | Returned bytes / digest |
| --- | --- | --- |
| [R2 shell launcher](https://dev.nudox.org/git/philocalyst/Backend/releases/download/nudox-diagnostic-installer-20261009-r2/install-shell-d837c1fc8130285f.sh), GET | HTTP 200 | 5,845 bytes; SHA-256 `d837c1fc8130285fe60d1dc44ef96f93fa2177f18bdb2c1db031eab7fc28a8ac` |
| [R2 Python bootstrap](https://dev.nudox.org/git/philocalyst/Backend/releases/download/nudox-diagnostic-installer-20261009-r2/install.py), GET | HTTP 200 | 13,426 bytes; SHA-256 `36f0ea62e3b49cadebcef0f89e735639530c0ba1686e62d4dcbfea96294174f5` |
| [Preview channel](https://dev.nudox.org/git/philocalyst/Backend/releases/download/nudox-diagnostic-installer-20261009-r2/preview-channel.json), GET | HTTP 200 | 3,222 bytes; SHA-256 `f7c24e4628ac88f6252063b1e54f9a96054993ea90964dcdc555809888cf4fef`; identical to the checked-in channel file |
| [Linux x86_64 installer](https://dev.nudox.org/git/philocalyst/Backend/releases/download/nudox-diagnostic-installer-20261009-r2/install-linux-x64-e11fcc271fc3a8fe.py), GET | HTTP 200 | 49,954 bytes; SHA-256 `e11fcc271fc3a8feb676b9f8c383bbf5bdc215578ffc399774819184908ed071` |
| [Linux C001 manifest](https://dev.nudox.org/git/philocalyst/Backend/releases/download/checkpoint-20261008-c0016d4f4f-linux-x64/release-manifest-linux-x64.json), GET | HTTP 200 | 698 bytes; SHA-256 `384ab3802efd256752f39fd8e3d79ace7af9e3ccf72797c71577327cb2535b24` |
| [Linux C001 archive](https://dev.nudox.org/git/philocalyst/Backend/releases/download/checkpoint-20261008-c0016d4f4f-linux-x64/nudox-linux-x86_64-c0016d4f4f.tar.gz), HEAD | HTTP 200 | Content-Length 133,556,631; archive SHA-256 advertised by the manifest: `b394e10203d02898b20140f0f9f9869aa8412e6e288fb8d8e25ac01469df7a05` |
| [Linux C001 `.sha256` sidecar](https://dev.nudox.org/git/philocalyst/Backend/releases/download/checkpoint-20261008-c0016d4f4f-linux-x64/nudox-linux-x86_64-c0016d4f4f.tar.gz.sha256), GET | HTTP 404 | 10-byte response; SHA-256 `7515bf959b73b956ceb967351c7e299cbb3668a53d35f9c770eb72e00d93ced6` |
| [macOS arm64 installer](https://dev.nudox.org/git/philocalyst/Backend/releases/download/checkpoint-20261006-a659d5d181-macos-arm64/install-macos-arm64-82f71f98a05a2b13.py), GET | HTTP 200 | 22,094 bytes; SHA-256 `82f71f98a05a2b13e05afd7185e35c5f8d252b9973ab4ce6d32ab22688794f25` |
| [macOS A659 archive](https://dev.nudox.org/git/philocalyst/Backend/releases/download/checkpoint-20261006-a659d5d181-macos-arm64/nudox-macos-arm64-a659d5d181.tar.gz), HEAD | HTTP 200 | Content-Length 70,309,994; archive SHA-256 advertised by the channel: `f88b841030699ae277afe697b6c92e98dfbb09a815f40388e3d2fbee0661e110` |

The Linux record is diagnostic preview tag `checkpoint-20261008-c0016d4f4f-linux-x64`, version `0.2.0`, source `c0016d4f4fc9b0f0a452c7c9556954e2287948b3`, source tree `8e4303f3f26690f137c8ca3701878f4f170d79b2`, and minimum glibc `2.39`. The stated validation boundary is Ubuntu 24.04+ with glibc 2.39+; the channel itself lists a runtime timeout and a NixOS loader refusal. The macOS record is diagnostic preview tag `checkpoint-20261006-a659d5d181-macos-arm64`, source `a659d5d1819078b3c0046937191a9b54a76b1dfb`; the installed manifest records version `0.0.0`. The channel reports `production_ready: false` and lists only `linux-x64` and `macos-arm64`.

## GitHub availability

| Request | Result | Returned bytes / digest |
| --- | --- | --- |
| [`releases/latest` API](https://api.github.com/repos/nudoxorg/Backend/releases/latest), GET | HTTP 200 | 21,721 bytes; SHA-256 `52a9fe6e793fa9aac240b8783e23d4b0ffd1e1e0883e0c3bf114c46de6f6c686` |
| [`v0.1.0/install-linux-x64.py`](https://github.com/nudoxorg/Backend/releases/download/v0.1.0/install-linux-x64.py), GET | HTTP 404 | 264,702-byte HTML response; SHA-256 `998c5362908977f822f8fa08720c2d4e6ca10516e6275f72e311763d313d0648` |
| [C001 release-tag API](https://api.github.com/repos/nudoxorg/Backend/releases/tags/checkpoint-20261008-c0016d4f4f-linux-x64), GET | HTTP 404 | 133-byte response; SHA-256 `010563d2cfb8f1e87f700936ce59ee3a33364da46e3db41b0049227918ece74d` |
| C001 [`release-manifest-linux-x64.json`](https://github.com/nudoxorg/Backend/releases/download/checkpoint-20261008-c0016d4f4f-linux-x64/release-manifest-linux-x64.json), GET | HTTP 404 | 21-byte response; SHA-256 `e28bf7d9fcc22cdfde0c7c8f31e3648a4847e7bda9cb69f309f24257eee3dd41` |
| C001 [`nudox-linux-x86_64-c0016d4f4f.tar.gz`](https://github.com/nudoxorg/Backend/releases/download/checkpoint-20261008-c0016d4f4f-linux-x64/nudox-linux-x86_64-c0016d4f4f.tar.gz), HEAD | HTTP 404 | No response body |

The latest GitHub release is `v0.1.0`, published August 19, 2026. Its twelve assets are `lindsey-linux-arm64.tar.gz[.sha256]`, `lindsey-linux-x64.tar.gz[.sha256]`, `lindsey-macos-arm64.dmg[.sha256]`, `lindsey-macos-arm64.tar.gz[.sha256]`, `lindsey-windows-x64.zip[.sha256]`, `SHA256SUMS.txt`, and `src.zip`; it is not the Nudox C001 installer/checkpoint set. These results refine the earlier blanket “GitHub delivery unavailable” wording: GitHub has a published release, while the expected Nudox installer and selected C001 assets are absent at the tested URLs.

## Validation boundary

The exact R2 bootstrap bytes were parsed and compiled with CPython 3.12.14; `install.py --help` exited 0, and `/bin/sh -n` passed on the exact saved shell-launcher bytes. The channel SHA matched `tools/package/forgejo-preview-channel.json`. These were read-only delivery and help checks. No installation, build, indexing, owner launch, or product acceptance was performed in this readback. The R2 artifacts identify the C001 and A659 source checkpoints above; neither is a build of the local `canonical` commit recorded at the top of this note. The earlier Mac repeat installation and binary hashes remain in [the October 12 Mac repeat receipt](diagnostic-installer-mac-repeat-evidence-20261012.json).
