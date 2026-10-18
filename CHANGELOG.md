# Changelog

## [0.4.0](https://github.com/loonghao/vibdock/compare/v0.3.0...v0.4.0) (2026-10-18)


### Features

* **cli:** expose per-application shortcut management ([#50](https://github.com/loonghao/vibdock/issues/50)) ([6a25334](https://github.com/loonghao/vibdock/commit/6a2533458ef6d6e25e9614561d8120be825b6909))
* **stt:** add a global hold-to-talk dictate trigger ([#57](https://github.com/loonghao/vibdock/issues/57)) ([1d326ae](https://github.com/loonghao/vibdock/commit/1d326aec521bfed7a41f92d96dd22c9ca2b739fd))
* **stt:** add bounded local dictation and focused-window insertion ([#53](https://github.com/loonghao/vibdock/issues/53)) ([6580c3c](https://github.com/loonghao/vibdock/commit/6580c3c2cc3fc573bcb28ff498c2d034be80f1c5))
* **stt:** add dev-only SenseVoice transcription prototype ([#47](https://github.com/loonghao/vibdock/issues/47)) ([540c61c](https://github.com/loonghao/vibdock/commit/540c61c0fb4f455e96bbca1fb63d636adcbb0540)), closes [#44](https://github.com/loonghao/vibdock/issues/44)
* **stt:** add optional native SenseVoice WAV transcription ([#49](https://github.com/loonghao/vibdock/issues/49)) ([26a31d2](https://github.com/loonghao/vibdock/commit/26a31d2910ab4f5e6ad9f47769998f6eafa032f6))
* **stt:** require a title match before dictation insertion ([#58](https://github.com/loonghao/vibdock/issues/58)) ([f5b9a33](https://github.com/loonghao/vibdock/commit/f5b9a3344e445ad46e15fd8e1ebd9f5140c9b2ab))
* **stt:** trim dictation with local Silero VAD ([#59](https://github.com/loonghao/vibdock/issues/59)) ([e4e3d76](https://github.com/loonghao/vibdock/commit/e4e3d767f70abe30eb14acc3e1e1acb729095793))
* **windows:** add tray microphone and pinned-software quick menu ([#45](https://github.com/loonghao/vibdock/issues/45)) ([6112d1f](https://github.com/loonghao/vibdock/commit/6112d1f37df68ead4774b0f9528ca34d05707fde))
* **windows:** recognize applications and dispatch per-application shortcuts ([#36](https://github.com/loonghao/vibdock/issues/36)) ([6fd196e](https://github.com/loonghao/vibdock/commit/6fd196e987709bac6682e58830efd710a6120cae))


### Bug Fixes

* **windows:** recognise UWP applications behind ApplicationFrameHost ([#51](https://github.com/loonghao/vibdock/issues/51)) ([8ebd320](https://github.com/loonghao/vibdock/commit/8ebd3207fbe567dfff9c219afd061060287e5e68))
* **windows:** report a foreground change during dispatch ([#52](https://github.com/loonghao/vibdock/issues/52)) ([790d4ca](https://github.com/loonghao/vibdock/commit/790d4ca92cbacc5844fc1f8b6d7202680503950e))

## [0.3.0](https://github.com/loonghao/vibdock/compare/v0.2.0...v0.3.0) (2026-10-16)


### Features

* add app microphone bindings and software status indicators ([#33](https://github.com/loonghao/vibdock/issues/33)) ([14b7369](https://github.com/loonghao/vibdock/commit/14b7369299a854bbd4104d08e2aac838db92310d))

## [0.2.0](https://github.com/loonghao/vibdock/compare/v0.1.0...v0.2.0) (2026-10-16)


### Features

* add desktop launcher with atomic service startup ([0e47ffd](https://github.com/loonghao/vibdock/commit/0e47ffd850b264f123b5f97b2e8c482f9d09bc56))
* add explicit Windows application playback session commands ([f68887f](https://github.com/loonghao/vibdock/commit/f68887fc93b210152dee7a9d430e3264aa6587bf))
* add one-use browser authorization sessions ([bb59e05](https://github.com/loonghao/vibdock/commit/bb59e05f46522abf3c27a235e4bd6a4deede06dd))
* add resident service with authenticated loopback API ([118f0e5](https://github.com/loonghao/vibdock/commit/118f0e5f565ced27067cff6fcba842bba0cb4d73))
* add thin Windows tray entry sharing the CLI service core ([44d4fbe](https://github.com/loonghao/vibdock/commit/44d4fbe066d53f9d36b70e38345605db4ac328b5))
* add WeChat Input capability probe ([be20e25](https://github.com/loonghao/vibdock/commit/be20e25dbf59610e711936e32e69a10d36def97a))
* authorize against the local service from the web config page ([a1e3436](https://github.com/loonghao/vibdock/commit/a1e343653e015d0f205e8daf6a52215df3acbfaf))
* declare Codex voice capabilities without unverified claims ([1c96991](https://github.com/loonghao/vibdock/commit/1c96991c62c2500b4fa1184b19d74f1b4e5aa177))
* **desktop:** add branded resident controls and hide-to-tray lifecycle ([24f664c](https://github.com/loonghao/vibdock/commit/24f664cb4336be036ae26ae5240de826984ce164))
* **desktop:** add opt-in non-focus-stealing Windows status indicator ([a51c565](https://github.com/loonghao/vibdock/commit/a51c5650f0546053ec290d31d844ebaa2989c196))
* expose completed hotkey action feedback ([739c6f7](https://github.com/loonghao/vibdock/commit/739c6f7fa104fbe06d5be1d0c179722b8985d719))
* integrate live hotkey status into the configuration page ([56129c7](https://github.com/loonghao/vibdock/commit/56129c73b7df6b6561c97da5c6b6f4c82ac7a45e))
* integrate resident hardware and voice configuration preview ([25c2b98](https://github.com/loonghao/vibdock/commit/25c2b986cdd4386a08480f82072a64357deb8528))
* integrate resident hardware and voice configuration preview ([#25](https://github.com/loonghao/vibdock/issues/25)) ([25c2b98](https://github.com/loonghao/vibdock/commit/25c2b986cdd4386a08480f82072a64357deb8528))
* **macos:** add native resident menu bar and Dock app ([fbd9039](https://github.com/loonghao/vibdock/commit/fbd90390c75ba66bf2750a0afd85bc5c51f0f06b))
* **macos:** persist native application presence and software icons ([3764ce3](https://github.com/loonghao/vibdock/commit/3764ce3b2a9a7709d5da697a58ac1af523894d64))
* persist web binding config and clarify result states ([8869a54](https://github.com/loonghao/vibdock/commit/8869a54bb3e3538f6841c0be28818031367d6503))
* real authenticated shutdown and instance reuse ([c9da22c](https://github.com/loonghao/vibdock/commit/c9da22ce5c1f8a8388cdbd9c4960126ad0b59157))
* register resident hotkeys with rollback on failed edits ([f9d0ee8](https://github.com/loonghao/vibdock/commit/f9d0ee8df7cee23e384bd2d95f50c2b47f575a76))
* share one device executor and expose allowlisted actions ([85f96ae](https://github.com/loonghao/vibdock/commit/85f96ae4b92ecb00a51b37870aab4800d5aec746))
* show resident hotkey registration and action feedback ([bef925b](https://github.com/loonghao/vibdock/commit/bef925b0e8a7273ad2b7c7c4d2fcf3eb92b53e4a))
* **windows:** add explicit exact-session playback controls ([7ded7cd](https://github.com/loonghao/vibdock/commit/7ded7cda7d29d086be3dc79923c829e9ad18f6b0))
* **windows:** persist read-only resident software observations ([060194c](https://github.com/loonghao/vibdock/commit/060194c7aa548f216b7096f5ea840e6c89d6ff3f))


### Bug Fixes

* **app-session:** report unsupported instead of a missing session on non-Windows ([576d1d3](https://github.com/loonghao/vibdock/commit/576d1d3365e887ff749b5ca94b1548b88011eb4a))
* authenticate instance discovery with bounded nonce proofs ([c2c9402](https://github.com/loonghao/vibdock/commit/c2c94028b813b6c219acd8a2c9b80117e5fc07ad))
* bind privileged controls to the proved connection ([07c3996](https://github.com/loonghao/vibdock/commit/07c39967d7396e37f46cfacf84d9f96134bf395a))
* bound HTTP request and response time on the loopback service ([57d9fad](https://github.com/loonghao/vibdock/commit/57d9fadc85f8827de3ec58df2421d05e2617e99f))
* bound status polling through response-body reads ([b4fdf91](https://github.com/loonghao/vibdock/commit/b4fdf91c8dcbb176a8a1e7eb47778ddefdd031d6))
* **codex:** scope EndVoice claim to the source it cites ([9e7e220](https://github.com/loonghao/vibdock/commit/9e7e220def31f379c2351c6bb26bdd9cd11e9a71))
* consume the launcher bootstrap code from #bootstrap ([b2eaee3](https://github.com/loonghao/vibdock/commit/b2eaee3aa52648ab1bff7c3aa671bea2a99ad317))
* correct a stale doc comment on skipped_session_warnings ([8dc6da6](https://github.com/loonghao/vibdock/commit/8dc6da650c2365e5943bdd0869ef7d3555ccb33f))
* credit setup steps to user actions and confirm draft loss ([fb4f732](https://github.com/loonghao/vibdock/commit/fb4f732656d8a7a608bb4ee1b79c7eaf2ca2d360))
* decode config with real JSON and persist before success ([fb6bd57](https://github.com/loonghao/vibdock/commit/fb6bd57d30bf44e7cf3098d1e1ed3ac48d87876f))
* **desktop:** harden keyboard handoff and candidate evidence ([f07318f](https://github.com/loonghao/vibdock/commit/f07318fe5696c1282f8a13362292e8fcb32463d6))
* **desktop:** honour the work-area origin and bind feedback to its own target ([7581d36](https://github.com/loonghao/vibdock/commit/7581d362336f7eaaad8af86de22838dac9d59539))
* **desktop:** make the status overlay reachable, wakeable, and bounded ([bab5302](https://github.com/loonghao/vibdock/commit/bab530288a8b92aee1ab1d8f43ac71161d82fa59))
* **desktop:** place the indicator in the work area, not the full screen ([6ea8b0d](https://github.com/loonghao/vibdock/commit/6ea8b0d28d87ad83d2bec76b29f88f447e477d29))
* **desktop:** verify native observer and browser initialization ([6ad73af](https://github.com/loonghao/vibdock/commit/6ad73af2f7daff24c08b93beb7cd9bfcc39390cd))
* display the actual hotkey registration failure ([195dc57](https://github.com/loonghao/vibdock/commit/195dc576c0cda013d63508a4edc541016f4c18b0))
* distinguish current registration from past action results ([b938e20](https://github.com/loonghao/vibdock/commit/b938e206c2b28c3fb05a84420897f9461ac59a74))
* distinguish unaddressable sessions from read failures in app-list ([ff7cc4d](https://github.com/loonghao/vibdock/commit/ff7cc4df678ab1d494f5ec553393d2da702dffa2))
* enforce a total deadline across socket reads ([4878049](https://github.com/loonghao/vibdock/commit/48780493e9bc81fb0e55639ea3b492620446ba7b))
* give each web test call its own private state directory ([df14afe](https://github.com/loonghao/vibdock/commit/df14afec71442d08d51e237094ef437e4150b38d))
* give the audio session readback and toggle fixes real regression tests ([4340124](https://github.com/loonghao/vibdock/commit/43401240087f85cf76c8eb270e1efd3f9b1a8a84))
* import Read for the non-Windows entropy path ([66ed1a3](https://github.com/loonghao/vibdock/commit/66ed1a39238550aff86664e13917b09f9dcd7801))
* keep response handoffs alive during slow routing ([e85bd20](https://github.com/loonghao/vibdock/commit/e85bd20b90fd79b026c3d4437c4613d212c935a3))
* **macos:** bind application selections to kernel launch identity ([23ae814](https://github.com/loonghao/vibdock/commit/23ae81433c5dabb470e18db0a5c679358bf82bb6))
* **macos:** encode retained Dock menu return correctly ([4235783](https://github.com/loonghao/vibdock/commit/423578360d3f44f93e1382857f5d415419c212c5))
* **macos:** keep capability caption within native panel bounds ([2de3a8d](https://github.com/loonghao/vibdock/commit/2de3a8df01c9e8dd599000e360532118046119ff))
* **macos:** preserve native logo colors and opaque appearance rendering ([20023ae](https://github.com/loonghao/vibdock/commit/20023ae2845ac2489cd2e7d7b8d1cdb8b009bb69))
* **macos:** satisfy native ownership conversions and current lint checks ([c4879c7](https://github.com/loonghao/vibdock/commit/c4879c78d5e04461bf0aa12d6b902b227855c845))
* **macos:** use checked alpha division for native icon pixels ([dbfd4ef](https://github.com/loonghao/vibdock/commit/dbfd4ef4811af04c06f8cc304d333dfbfaee3acd))
* **macos:** verify Dock policy readback and fail closed on cleanup ([aceafb7](https://github.com/loonghao/vibdock/commit/aceafb76fe04ac6cd07c3d675e248ae8ce6c3c4b))
* map character hotkeys through the Windows keyboard layout ([55c3d92](https://github.com/loonghao/vibdock/commit/55c3d9271be1ae20c0670e269cc1b9b8556f7d53))
* parse letter F and round-trip plus shortcuts ([d9b31c0](https://github.com/loonghao/vibdock/commit/d9b31c0cce1d2509202fb907a0c702a4b1ac64c7))
* preserve split headers and move response writes off the action loop ([48caab0](https://github.com/loonghao/vibdock/commit/48caab0139a292a171ca994c8a9682cb879d784a))
* read bodies on worker threads so the action loop stays responsive ([a1728e1](https://github.com/loonghao/vibdock/commit/a1728e128687f84ea74a1fe8aba8dd118a91edb1))
* **release:** preserve and verify original artwork bytes ([bcedda9](https://github.com/loonghao/vibdock/commit/bcedda9f6102390234d37fc94c8856b54b3a56fd))
* report audio session write outcomes without misleading callers ([759925d](https://github.com/loonghao/vibdock/commit/759925d9f320d18f16d47e7c3103adf776c0eaf9))
* report platform audio capabilities accurately ([8272ea8](https://github.com/loonghao/vibdock/commit/8272ea8bc59dce9ce4eec6164d1186f5d0d4ab44))
* report undocumented Codex capabilities as unknown, not unsupported ([68322a7](https://github.com/loonghao/vibdock/commit/68322a7150355959ac7377e98ed218427ec8f38c))
* report unsupported platform audio instead of an empty device list ([0ae911c](https://github.com/loonghao/vibdock/commit/0ae911c118209095f6dcb0ef730f8282da45428e))
* require reauthorization after browser session expiry ([847d17b](https://github.com/loonghao/vibdock/commit/847d17b7e07db6ea35fc1601b550d6ae4b283985))
* require the bound port for browser origins ([0ddfdf1](https://github.com/loonghao/vibdock/commit/0ddfdf10332e46807aa8d5744fc4a045082205f7))
* restore exact port validation for browser origins ([42f3ff6](https://github.com/loonghao/vibdock/commit/42f3ff642d0754d99f3229f1ff202831a5f79bc7))
* restore exact port validation for browser origins ([72efd2a](https://github.com/loonghao/vibdock/commit/72efd2aaa9f29836a030f0beb3c5ae6e78fccf7a))
* return hotkey activation errors separately from persistence failures ([c49174b](https://github.com/loonghao/vibdock/commit/c49174bf982158f684d9643aa5e3e733fe159266))
* scope service port checks to its IPv4 listener ([2a1cf65](https://github.com/loonghao/vibdock/commit/2a1cf65d3bf889865847346ed749aa4b73d97f17))
* separate binding status from historical action feedback ([7208ba1](https://github.com/loonghao/vibdock/commit/7208ba136711af5723bd35406ae9a79945599ad8))
* strengthen service identity, persistence, and input handling ([bb3a2be](https://github.com/loonghao/vibdock/commit/bb3a2be85a2aa870c6dce09a252fb88d45aa2ae5))
* **ui:** distinguish service bindings from browser drafts ([64575bf](https://github.com/loonghao/vibdock/commit/64575bffcd714d09e745f94d152e357a21ae0b50))
* **wechat:** keep host snapshot out of runtime capability output ([2bb1b80](https://github.com/loonghao/vibdock/commit/2bb1b80ee130c0854f0ef1361557be0b723d2b47))
* **windows:** align native styles and current lint checks ([1b57983](https://github.com/loonghao/vibdock/commit/1b57983a422fc87d9571cf08338d135219594e6e))
* **windows:** keep delayed playback execution outcome unconfirmed ([e976cdd](https://github.com/loonghao/vibdock/commit/e976cdd9d9fb737ef327600b3b54d0bbca5a9059))
* **windows:** keep long playback results readable and scrollable ([2e9694c](https://github.com/loonghao/vibdock/commit/2e9694c3feee10b648d6911bf9891d0cdd9fc255))

## 0.1.0 (2026-10-14)


### Features

* bootstrap VibDock hardware and voice workflows ([9f65f20](https://github.com/loonghao/vibdock/commit/9f65f20fc987bc4192650424de6e95c2dab3e65e))


### Bug Fixes

* validate preview arguments and clarify launch design ([9e89b94](https://github.com/loonghao/vibdock/commit/9e89b94e8fd55f5adbb9e9404f4f755fbedaea4a))
