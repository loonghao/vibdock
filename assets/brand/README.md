# VibDock original brand assets

Supplied by the project owner as `vibdock-icon-assets-v1.zip` on 2026-10-16. The SVGs and primary ICO are unchanged. See ORIGINAL-README.txt for the selected concept, colors, and original export notes.

The light/dark ICO containers pack the supplied PNG bytes without resampling, recoloring, or redrawing them. Each contains 16, 24, 32, 48, 64, 128 and 256 px images. SHA256.json records the shipped asset hashes.

The macOS ICNS container packs the supplied teal 128, 256, 512 and 1024 px PNGs in the modern `ic07`, `ic08`, `ic09` and `ic10` slots, without changing their bytes. `MACOS-PNG-SHA256.json` retains the original source hashes, including the unused 16, 32 and 64 px PNGs; the packaging check verifies every shipped PNG against its original hash. The original SVG/ICO hashes are unchanged.

Small ICNS representations are intentionally omitted. PNG payloads in the legacy `icp4`, `icp5` and `icp6` slots can render as colored noise when macOS uses the file as an application icon, despite the embedded PNGs themselves being valid. AppKit selects and downscales a supplied modern representation for smaller sizes; [Apple documents support for incomplete icon sets](https://developer.apple.com/library/archive/documentation/GraphicsAnimation/Conceptual/HighResolutionOSX/Optimizing/Optimizing.html). This container-only correction preserves the original artwork and adds no image codec or dependency. Packaging tests reject the problematic slots even if their container hash is updated; native acceptance must also inspect the observed application's small icon.

These assets contain no remote resources, fonts, scripts, personal metadata, or other application logos. They are used for VibDock branding at the owner's request. The original pack contains no separate artwork license grant; do not infer a third-party license or endorsement from the software's MIT license.
