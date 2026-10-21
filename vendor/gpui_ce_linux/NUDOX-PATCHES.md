# Nudox patches

This crate is based on `gpui_ce_linux 0.1.0`, pinned in the original lockfile
with checksum `ab947c84c6460f6431c99bd6a8b0954591bfa511c0006515ecc3897e7a903c84`.

Nudox keeps Wayland windows bufferless until their first compositor configure
has been acknowledged. The compositor's initial empty commit still requests
the configure; the draw gate prevents an early GPUI frame from mapping a buffer
before the protocol permits it. The configure-state unit test covers the
initial transition and later configure events.
