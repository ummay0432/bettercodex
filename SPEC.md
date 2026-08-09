# bettercodex binary distribution

## Supported binaries

bettercodex will publish three 64-bit release binaries:

| System | Architecture | Rust target | Supported environments |
| --- | --- | --- | --- |
| Windows | x86-64 | `x86_64-pc-windows-msvc` | 64-bit Windows |
| macOS | ARM64 | `aarch64-apple-darwin` | Apple silicon Macs |
| Linux | x86-64 | `x86_64-unknown-linux-gnu` | 64-bit Ubuntu and Debian |

Ubuntu and Debian share one Linux binary. They are separate supported and tested
environments, but not separate release targets.

## Build and publication

GitHub Actions will build the three binaries and publish them as downloadable
release assets. The Linux binary must be tested on both Ubuntu and Debian.

## Installation and updates

bettercodex will be refactored so normal installations and updates download the
matching prebuilt binary. Users must not need Rust, Cargo, platform build tools,
or a local compilation step. Source compilation remains a developer workflow,
not the default installation or update path.
