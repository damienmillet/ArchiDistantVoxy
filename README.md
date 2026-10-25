# pumpkin-lod

Server → client LOD (Level Of Detail) streaming plugin for the [Pumpkin](https://github.com/Pumpkin-MC/Pumpkin) Minecraft server (Rust).

- Phase 1 target: **Distant Horizons 3.3.x** clients (network protocol no. 16).
- Phase 2 target (optional): **Voxy + LOD Server Support (LSS)** clients.

The full specification (scope, requirements, architecture, milestones, acceptance criteria, risks, decisions) lives in [SPEC.md](SPEC.md).

## License

GPL-3.0-or-later. The LodBuilder and throttling logic are ported from [DH Support](https://gitlab.com/distant-horizons-team/distant-horizons-server-plugin) (GPLv3+), which makes this project GPLv3 as a derivative work. The `LICENSE` file must contain the full GPLv3 text (add it before the first code commit; fetch it from https://www.gnu.org/licenses/gpl-3.0.txt).
