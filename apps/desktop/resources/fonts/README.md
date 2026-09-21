# Nudox capture fonts

These are the four type roles specified by the Nudox design system. They are
bundled into the desktop binary so line breaks, glyph metrics, and visual
hierarchy remain deterministic for screenshot captures. The files are
variable fonts; GPUI chooses the requested weight from the embedded face.

| file | role/family | source | SHA-256 |
| --- | --- | --- | --- |
| `Archivo[wdth,wght].ttf` | display / Archivo | [Google Fonts](https://github.com/google/fonts/tree/main/ofl/archivo) | `0e094a7d3c7c4c25cf1310c4b30014f1dae9332220b1c2c88f4fa996f0b05053` |
| `InstrumentSans[wdth,wght].ttf` | interface / Instrument Sans | [Google Fonts](https://github.com/google/fonts/tree/main/ofl/instrumentsans) | `b24f1812584816958afcf22e22d08e44318c5e51651e25d2438efdde389b33b1` |
| `Newsreader[opsz,wght].ttf` | prose / Newsreader | [Google Fonts](https://github.com/google/fonts/tree/main/ofl/newsreader) | `8a08d13f8a6c0d51be379a60af84f945f65369a67e509ee3c3bdcc421254d7c1` |
| `GeistMono[wght].ttf` | specimen / Geist Mono | [Vercel Geist](https://github.com/vercel/geist-font) | `87c2aff9723544a9adaea19d92e42a33705c9723624801b6e0224c2206a6af0d` |

The adjacent `*-OFL.txt` files are the licenses distributed by each source.
Archivo, Instrument Sans, Newsreader, and Geist Mono are distributed under
the SIL Open Font License, version 1.1. The runtime verifies every hash before
registration; a mismatched asset fails startup instead of silently changing
capture typography or falling back to a host font.
