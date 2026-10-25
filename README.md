# ArchiDistantVoxy

Server → client LOD (Level Of Detail) streaming plugin for the [Pumpkin](https://github.com/Pumpkin-MC/Pumpkin) Minecraft server (Rust).

- Phase 1 target: **Distant Horizons 3.3.x** clients (network protocol no. 16).
- Phase 2 target (optional): **Voxy + LOD Server Support (LSS)** clients.

The full specification (scope, requirements, architecture, milestones, acceptance criteria, risks, decisions) lives in [SPEC.md](SPEC.md).

## Workspace layout

| Crate | Description |
|---|---|
| `crates/lod-core` | Pure-Rust DH protocol 16 codec, LOD section (DTO) builder, two-level LOD cache (RAM LRU + persistent `BlobStore`, SQLite via the `sqlite` feature), transfer queue and bandwidth throttling. Fully unit-tested, no Pumpkin dependency. |
| `crates/archidistantvoxy` | Native Pumpkin plugin (`.so`/`.dll`/`.dylib`): handshake, session handling, custom-payload dispatch on `distant_horizons:msg`, per-player throttled transfers, persistent LOD cache at `plugins/data/archidistantvoxy/lod-cache.db`. |

## Pinned Pumpkin version

`0.2.1+26.3-26.51` (see `crates/archidistantvoxy/Cargo.toml`). Pumpkin has no stable API yet; every release of this project is built and validated against this exact tag (ENF-4).

## Testing (receiving real LODs on a DH client)

The release workflow (`.github/workflows/release.yml`) builds the plugin on every push to `main` and uploads the artifacts:

1. **Get the plugin binary** — on GitHub, *Actions → Release artifacts → latest successful run → Artifacts*: `archidistantvoxy-linux-x86_64` (`libarchidistantvoxy.so`), `archidistantvoxy-windows-x86_64` (`archidistantvoxy.dll`) or `archidistantvoxy-macos-arm64` (`libarchidistantvoxy.dylib`, Apple Silicon M1–M5).
2. **Get Pumpkin** `0.2.1+26.3-26.51` (from [Pumpkin's releases](https://github.com/Pumpkin-MC/Pumpkin/releases), tag matching the pin) and run it once so the configs are generated, then stop it.
3. **Install the plugin**: drop the `.so`/`.dll` into the server's `plugins/` folder. Native plugins load by extension; the plugins system is enabled by default (`[plugins] enabled = true` in the advanced configuration). On Windows the DLL cannot be hot-unloaded, prefer a clean stop/start.
4. **Client setup**: vanilla Minecraft (26.3–26.51 supported by the pin) + **Distant Horizons 3.3.4** only. DH is the only tested path; Voxy answers its handshake but gets `NOT_GENERATED` for now.
5. **Join the server** and look at the horizon: DH should display distant terrain you never explored, filled progressively under the per-player throttle (defaults 500 KB/s for 2 minutes, then 200 KB/s).
6. **Check the logs**: `archidistantvoxy` target lines show the session init, config, requests and generations; `/lod status` in-game reports the cache hit-rate and queues; `/lod pregen 64` pre-fills around spawn.
7. **Restart the server** and confirm in the log that sections come from the persistent cache (`plugins/data/archidistantvoxy/lod-cache.db`) without regeneration.

Known first-run caveats: a section's first request pays the generation cost (~seconds per 64×64 section, off the tick); terrain divergence from vanilla rendering is expected to be minor but is exactly what this recette validates (spec acceptance criterion 3).

## Building

```bash
cargo test -p lod-core        # protocol + throttling unit tests
cargo build -p archidistantvoxy --release   # produces the native plugin
```

Notes:

- The plugin depends on `pumpkin-core` **renamed to `pumpkin`**: the `#[plugin_impl]` macro emits `pumpkin::plugin::*` paths.
- `pumpkin-data` is a ~88 MB generated crate; building the plugin needs a machine with ≥ 8 GB RAM (or CI). Disable debug info (`CARGO_PROFILE_DEV_DEBUG=0`) to reduce rustc memory usage.
- The built `libarchidistantvoxy.so` goes into the server's `plugins/` directory (native loader, enabled in Pumpkin's plugin config).

## Status

- **M0 (spike)**: done — handshake messages (init/config/level-init/session-config/reject/close) round-trip tested in `lod-core`; plugin wires `PlayerCustomPayloadEvent` to the DH channel.
- **M1 (MVP)**: done — persistent SQLite LOD cache (survives restarts, per-dimension), two-level RAM LRU cache, per-player throttled fragmented transfers, cancel support, update-only requests answered by timestamp comparison, clean refusal of unsupported protocols. LOD requests are served from cache; misses are rejected with a typed message until M2 wires terrain generation.
- **M2 (on-demand generation)**: in progress — `lod-core` gained the incremental terrain capture (column/segment builder with sample budgets, port of the pumpkin-plugins capture); the plugin generates distant sections on cache miss via `pumpkin-world`'s `generate_single_chunk` (Features stage, outside the loaded area, deterministic from the seed), samples block states, biomes and surface heightmaps into DH materials, and serves the result through the throttled transfer queue. Generation runs on the Tokio blocking pool (ENF-2).
- **M3 (operations)**: in progress — `/lod status` (cache size, RAM usage, hit-rate, sessions, queued transfers, pending generations — EF-8), `/lod pregen <radius>` (bounded to 1024 chunks, spawn-centered, skips cached sections — EF-6), `/lod resend <player>` (resend flag consumed on the next requests), `/lod purge-cache` (wipes both cache layers via the new BlobStore::clear), permissions `lod.admin` / `lod.pregen` (EF-10).
- **M3 invalidation & config**: done — block place/break events invalidate the covering LOD section per a configurable policy (`immediate`/`periodic`/`disabled`, EF-7); TOML config at `plugins/data/archidistantvoxy/config.toml` (bandwidth budgets, login-sync duration, generation threads and sample budget, invalidation policy — EF-5/ENF-2/EF-7), parsed with serde and falling back to defaults.
- **M3 complete**: the `periodic` invalidation policy flushes its dirty set every 30 seconds from a background task spawned in `on_load`.
- **M4 (LSS/Voxy, minimal)**: done — the LSS wire protocol v20 is ported in `lod-core::lss` (channels, VarInt primitives, handshake, session config with the v16 legacy echo, batch chunk requests/responses, and the full v20 voxel-column encoder: identity dictionary, palettes, packed longs, light layers); the plugin routes `lss:*` channels on the same custom-payload event, completes the v20 handshake and answers batch requests. **License gate resolved: LSS is MIT** (© 2026 VoX), compatible with this project's GPL-3.0; the port credits the source. **Documented gap**: batch requests currently answer `NOT_GENERATED` per position — the LSS column builder needs pumpkin-world per-chunk sections (16×16 columns) while the DH cache stores 64×64 sections; wiring per-chunk column encoding is the remaining M4+ work, as is Voxy real-client validation.

## License

GPL-3.0-or-later (see [LICENSE](LICENSE)). The LodBuilder and protocol logic are ported from [DH Support](https://gitlab.com/distant-horizons-team/distant-horizons-server-plugin) (GPLv3+), which makes this project GPLv3 as a derivative work.
