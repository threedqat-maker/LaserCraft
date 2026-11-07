# LaserCraft

LaserCraft is open-source desktop software for designing laser jobs and preparing them for engraving and cutting. It began as a fork of [PhotonForge](https://github.com/babisque/PhotonForge) and is now being developed in its own direction, with a LightBurn-inspired design workflow and an initial focus on GRBL-based machines.

The project is experimental and under active development. Some tools are still being built on feature branches, and hardware support is evolving. Check the current branch and documentation before relying on a feature.

## Current project

The codebase is a Rust workspace. Its current foundations include:

- SVG import and vector geometry in millimeters
- A layered design workspace with machine profiles and cut settings
- Vector toolpath planning, G-code preview, and G-code export
- Early GRBL connection and machine-status support
- Ongoing work on drawing, node editing, text, and shape modifiers

LaserCraft retains the original `pf-*` crate names while the fork evolves. Changes from PhotonForge are not applied automatically; upstream updates are incorporated deliberately.

## Build and run

Requires Rust stable (1.99 or newer).

```sh
cargo run -p pf-app                                    # run the app
cargo test --workspace                                 # run the tests
cargo clippy --workspace --all-targets -- -D warnings  # lint
cargo fmt --all                                        # format
```

On Linux, building the GUI needs the X11/Wayland and fontconfig development headers. On Debian/Ubuntu:

```sh
sudo apt-get install libxkbcommon-dev libxcb-render0-dev libxcb-shape0-dev \
  libxcb-xfixes0-dev libwayland-dev libfontconfig1-dev
```

## Workspace crates

| Crate | Purpose |
|---|---|
| `pf-core` | Geometry, shapes, layers, units, and document model |
| `pf-io` | SVG and G-code file import/export |
| `pf-gcode` | Toolpath planning and G-code generation |
| `pf-device` | GRBL communication and machine state |
| `pf-app` | Desktop GUI |

## Development direction

The goal is to grow LaserCraft into a capable laser design and control application. Planned work includes expanding vector editing and text tools, improving machine connectivity and job controls, and refining the workflow around real hardware. Features that can move a machine or fire a laser require careful validation before being treated as ready for use.

## License

LaserCraft is licensed under the GNU General Public License v3.0 or later. See [`LICENSE`](LICENSE).
