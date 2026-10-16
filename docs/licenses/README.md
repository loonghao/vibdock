# macOS binding notices

The native adapter uses the MIT option in the following published manifests:

| Crate | Version | Published license | Upstream source revision |
| --- | --- | --- | --- |
| objc2 | 0.6.3 | MIT | b4167b582b2f75f9a1be75495c41b765344fd03c |
| objc2-foundation | 0.3.2 | MIT | 7b1abfd750a2cacaea71d6a56ecfb83cb7de560b |
| objc2-app-kit | 0.3.2 | Zlib OR Apache-2.0 OR MIT | 7b1abfd750a2cacaea71d6a56ecfb83cb7de560b |
| objc2-core-foundation | 0.3.2 | Zlib OR Apache-2.0 OR MIT | 7b1abfd750a2cacaea71d6a56ecfb83cb7de560b |
| objc2-encode | 4.1.0 | MIT | 8d214f5477365ffcbcbb7de058c86ed9a518efb7 |
| dispatch2 | 0.3.1 | Zlib OR Apache-2.0 OR MIT | 8852b424193ca41602281b3d7540d7c8ed51e49a |
| libc | 0.2.190 | MIT OR Apache-2.0 | 7b0ab5528dc7f361f3a0b4c06c2cad9971617f75 |
| bitflags | 2.13.2 | MIT OR Apache-2.0 | published crates.io archive |

Versions/checksums are fixed by Cargo.lock. Metadata and source revisions were
checked in the corresponding official crates.io package manifests and
`.cargo_vcs_info.json`, not inferred from a similarly named library.

[objc2-upstream-notice.md](objc2-upstream-notice.md) reproduces the upstream notice
at the framework release revision. That historical notice links to the MIT text;
[objc2-MIT.txt](objc2-MIT.txt) additionally preserves the project's current full
MIT notice from [official upstream](https://github.com/madsmtm/objc2/blob/main/LICENSE-MIT.txt).
[bitflags-MIT.txt](bitflags-MIT.txt) is copied from the exact published 2.13.2
archive. [libc-MIT.txt](libc-MIT.txt) is copied from the exact published 0.2.190
archive. Original VibDock artwork provenance remains in assets/brand and is not
relicensed by these software dependency notices.

The upstream objc2 notice separately discusses derivation from Apple SDKs. This
package does not redistribute Apple SDKs or grant rights to Apple software.
