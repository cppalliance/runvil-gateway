# Runvil Gateway

The Gateway is a Rust server that holds every model credential and serves an OpenAI-shaped HTTP and WebSocket API. Every route follows the standard it is modeled on where one exists, and every addition to that standard is named as an extension and is opt-in, so a client written against OpenAI works here unchanged and a client written here works against any server speaking the standard. `crates/gateway/README.md` is the full statement.

- `cargo nextest run --workspace --all-features` tests everything. A default build needs one `npm ci` in `crates/gateway/config-ui/ui` first.
- `cargo test -p build-xtask` runs the architecture checks, and `cargo xtask tidy` prints the same report. The checks are the rules; do not restate them here.
- `cargo build` builds the gateway. `--no-default-features` drops the config UI, local models, web search, and speech.
- Rustdoc warnings fail CI: `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features`.
- Per-crate conventions live in each crate's `README.md` and `AGENTS.md`.
