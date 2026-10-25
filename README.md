# ArchiDistantVoxy

Server → client LOD (Level Of Detail) streaming plugin for the [Pumpkin](https://github.com/Pumpkin-MC/Pumpkin) Minecraft server (Rust).

- Phase 1 target: **Distant Horizons 3.3.x** clients (network protocol no. 16).
- Phase 2 target (optional): **Voxy + LOD Server Support (LSS)** clients.

The full specification (scope, requirements, architecture, milestones, acceptance criteria, risks, decisions) lives in [SPEC.md](SPEC.md).

## Workspace layout

| Crate | Description |
|---|---|
| `crates/lod-core` | Pure-Rust DH protocol 16 codec, LOD section (DTO) builder and bandwidth throttling. Fully unit-tested, no Pumpkin dependency. |
| `crates/archidistantvoxy` | Native Pumpkin plugin (`.so`/`.dll`/`.dylib`): handshake, session handling, custom-payload dispatch on `distant_horizons:msg`. |

## Pinned Pumpkin version

`0.2.1+26.3-26.51` (see `crates/archidistantvoxy/Cargo.toml`). Pumpkin has no stable API yet; every release of this project is built and validated against this exact tag (ENF-4).

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

- **M0 (spike)**: handshake messages (init/config/level-init/session-config/reject/close) implemented and round-trip tested in `lod-core`; plugin wires `PlayerCustomPayloadEvent` to the DH channel.
- **Next (M1)**: cache (SQLite), LodBuilder pipeline from `pumpkin-world` terrain, throttled transfer of LOD sections (`transfer_fragment`/`transfer_response` encoders are ready in `lod-core`).

## License

GPL-3.0-or-later (see [LICENSE](LICENSE)). The LodBuilder and protocol logic are ported from [DH Support](https://gitlab.com/distant-horizons-team/distant-horizons-server-plugin) (GPLv3+), which makes this project GPLv3 as a derivative work.
