# Runvil Gateway

The gateway is the server process that holds every model credential. It serves an OpenAI-compatible HTTP and WebSocket API, routes each request to a cloud provider or a local model, manages the model catalog and local model downloads, and, when built with them, serves web search and speech, plus a browser settings page. It starts serving before any slow model work begins, and on desktop machines it runs from the system tray.

## The API

Every route follows the industry standard it is modeled on where one exists, so a program written against that standard works against the gateway without a special case. Where the gateway needs something a standard lacks, the wire shape is the standard plus the smallest addition that makes the feature possible, named as an extension, documented beside the shape it extends, and opt-in wherever the protocol offers a way to ask. A client never relies on an extension it has not asked for.

`crates/gateway/README.md` states the principle in full, including the extension it depends on most and the fallback rule.

## Building

The config UI is bundled by esbuild, so one `npm ci` per checkout comes first:

```sh
(cd crates/gateway/config-ui/ui && npm ci)
cargo build
cargo build --no-default-features
```

`cargo build` builds the gateway alone, because the workspace's `default-members` names it. Node.js 22 is required for a default build. `--no-default-features` drops the config UI, local models, web search, and speech, so it needs no Node and no npm install.

## Testing

```sh
cargo nextest run --workspace --all-features
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
```

`cargo test -p build-xtask` runs the architecture checks: lint inheritance, the build-script file-size ceiling, the unsafe allowlist, the doctest ban, the `Debug` spelling rule, and the walled admin tier. `cargo xtask tidy` prints the same report on demand.

## Releases

`dist-workspace.toml` drives cargo-dist releases for five targets: `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `x86_64-pc-windows-msvc`, `aarch64-apple-darwin`, and `x86_64-apple-darwin`. A `gateway-v*` tag starts a release.

## License

Boost Software License 1.0. See `LICENSE`.
