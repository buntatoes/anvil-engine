# Anvil Engine

Page pipeline for Anvil Browser. Version 0.22. Author: Buntos.

This is not the window, the network stack, or the profile file. Those stay in `anvil-browser`.

There are no crates.io dependencies.

## Pipeline

| Crate | What it is |
| --- | --- |
| `anvil-dom` | HTML tree and selectors |
| `anvil-style` | CSS cascade and media queries |
| `anvil-layout` | Block, inline, flex, grid, table, and positioned boxes |
| `anvil-paint` | Display list and zoom |
| `anvil-raster` | RGBA framebuffer. Shadows, rounded boxes, and gradients use the software draw device |
| `anvil-script` | Budgeted script runtime and the numeric x86-64 compiler |

`anvil-script` does not open a socket. `fetch()` is performed by the host in `anvil-browser`.

## Build

Clone this repository, `anvil-browser`, and the libraries below as sibling directories.

```text
../anvil-engine
../anvil-browser
../anvil-image
../anvil-json
../anvil-ttf
../anvil-gpu
../anvil-compress
```

`anvil-image` links `anvil-compress`.

Rust 1.85 or newer. A C++20 compiler is optional. If none is found, the C++ files are skipped and the Rust paths are used.

```text
cargo test
```

## License

Proprietary. All rights reserved. See `LICENSE`.

| Repository | License |
| --- | --- |
| This one (`anvil-dom`, `anvil-style`, `anvil-layout`, `anvil-paint`, `anvil-raster`, `anvil-script`) | Proprietary. All rights reserved |
| `anvil-browser` | Apache-2.0. Not this file |
| `anvil-image`, `anvil-json`, `anvil-ttf`, `anvil-gpu`, `anvil-compress` | MPL-2.0, in those repositories |

Revisions of these crates that were already published in `anvil-browser` under the Apache License 2.0 stay under that license for those revisions only.
