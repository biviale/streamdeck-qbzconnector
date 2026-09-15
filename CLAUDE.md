# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

QBZ Connector — an OpenDeck / OpenAction plugin, in Rust, driving the QBZ
desktop Qobuz client from a Stream Deck. Linux only. User-facing documentation
is in `README.md`; this file covers what you need to change the code.

## Commands

```bash
./scripts/test.sh                 # fmt --check, clippy -D warnings, unit tests
./scripts/test.sh --live          # the above plus the #[ignore]d tests
./scripts/install.sh --restart    # build, install into OpenDeck, restart it
python3 scripts/make_icons.py     # regenerate icons after changing a glyph

cargo test volume::                        # one module
cargo test --release -- --ignored <name> --nocapture   # one live test
tail -f ~/.local/share/opendeck/logs/plugins/com.biviale.qbzconnector.sdPlugin.log
```

`install.sh` copies into `~/.config/opendeck/plugins/com.biviale.qbzconnector.sdPlugin/`.
OpenDeck reads `manifest.json` only at startup, so **any manifest change needs
`--restart`, and then the affected action's instances removed and re-added in
the OpenDeck UI** — an existing instance keeps the shape it was created with.

Live tests need QBZ running on the session bus. They **move the real volume**
and write to the real keyring (under a probe service name they clean up),
which is why they are `#[ignore]`d rather than part of the default run.

## Architecture

### One hub, many keys

`openaction` runs a single process for every instance of every action.
`src/hub.rs` owns the only D-Bus connection and publishes a `QbzState`
snapshot through a `tokio::sync::watch`. **Actions never touch D-Bus
directly**: they read the snapshot to draw themselves and call helpers to act.

Rendering is centralised, not per-action: when the hub publishes, `main.rs`
calls `actions::refresh_all`, which asks each action module to redraw its
visible instances. So a volume change also re-runs the play/pause and cover
draw paths — which is why `actions::image_changed` exists. A cover is tens of
kilobytes of base64 on the wire, and resending it on every volume tick would
be pure waste. **Any new action that sets an image must go through that
guard** and call `forget_drawn` in `will_disappear`.

QBZ is treated as something that comes and goes: the hub follows
`NameOwnerChanged`, so starting before QBZ or quitting it mid-session settles
into `online: false` rather than an error path. The hub is published even when
the session bus is unreachable, so an action can never panic on a missing
singleton.

### The volume shadow — read `src/volume.rs` before touching volume

QBZ's MPRIS `Volume` property is **write-only in practice**. The setter works;
the getter answers its initial `1.0` forever, because `qbz-qt` never feeds the
level back into its MPRIS state. Everything about the volume design follows
from that single fact: the plugin keeps its own value, seeded from QBZ's
`ui_prefs.json`, adopted when QBZ persists a new one, and re-anchored by every
absolute write.

Two traps already paid for, both covered by tests:

- The file watcher is on the **directory**, because QBZ replaces the file by
  atomic rename — a watch bound to the inode dies after the first save.
- It must ignore `Access` events. Reading the file emits them, and this module
  reads the file whenever it wakes, so reacting to any event made the watcher
  feed itself and drag the level back after every key press.

### Settings live in two caches, for two different reasons

`src/settings.rs` — OpenDeck stores **one object per plugin** and
`setGlobalSettings` replaces it wholesale. The Qobuz identity and the
appearance toggle are written by unrelated code at unrelated times, so every
write must carry the other's current value. Always go through
`settings::update`; never call `set_global_settings` directly.

`actions::remember_settings` / `instance_settings` — `openaction` hands an
instance's settings only to event callbacks, but redraws happen outside any
callback. New actions with settings must cache them in `will_appear` and
`did_receive_settings`, and drop them in `will_disappear`.

### Key images

`scripts/make_icons.py` is the single source for glyph shapes. It writes both
the manifest tiles (`com.biviale.qbzconnector.sdPlugin/icons/`) and the
white-on-transparent masks embedded in the binary (`assets/glyphs/`), so the
two cannot drift. **Do not hand-edit either output.** Needs `rsvg-convert`.

`src/palette.rs` picks the album colour and guarantees it clears 4.5:1
contrast against white, because the glyphs are white. `src/glyphs.rs`
composites mask over colour. `src/render.rs` handles cover art and shares a
byte cache between the key image and the colour scan, so a cover is fetched
once.

### Qobuz API quirks

The plugin holds its own Qobuz session: QBZ encrypts its own token at rest.
Application credentials (`app_id`, signing secrets, OAuth key) are **read from
QBZ's cache** at `~/.cache/qbz/bundle_tokens.json` rather than re-extracted —
bundle scraping is the most breakage-prone logic in the ecosystem, QBZ already
maintains it, and this plugin is useless without QBZ.

Four things that cost a debugging round each, all now under test in
`src/qobuz/`:

| Quirk | Where |
|---|---|
| The OAuth code arrives as `code_autorisation`, not `code`, possibly percent-encoded | `auth::parse_callback` |
| `/oauth/callback` returns the token as `token`; `user_auth_token` is the *next* step's field | `auth::token_from_callback` |
| `favorite/getUserFavorites` signs an **empty** parameter string although it sends `type`, `limit`, `offset` | `favorites::list_sig` |
| The bundle ships several secrets and does not say which is live; try them in order | `favorites::try_secrets` |

**The keyring must be called through `spawn_blocking`.** Its Secret Service
backend starts a runtime of its own, and tokio panics if that happens on a
thread already driving async tasks — which aborted sign-in at its last step.
Use the helpers in `auth::store`, never `keyring::Entry` directly.

## Reference checkouts

`upstream/` holds clones of QBZ, OpenDeck, the OpenAction docs and the
Stream Deck SDK. They are gitignored and **read-only**: consult them, never
modify them. QBZ is MIT; parts of `src/qobuz/` derive from it, credited in
`LICENSES/qbz-MIT.txt`.

When behaviour depends on what QBZ or OpenDeck actually does, **read their
source rather than inferring it** — most of the bugs in this project's history
came from porting a reference incompletely.

## Out of scope, deliberately

Shuffle, repeat, native mute and volume read-back are absent because QBZ does
not expose them: `Shuffle` and `LoopStatus` are hardcoded constants with no-op
setters. Do not add actions for them. The Stream Deck+ encoder support is
written but **has never run on hardware** — say so rather than implying it
works.
