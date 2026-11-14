[![License](https://img.shields.io/static/v1?label=license&message=BSL-1.0&color=blue)](LICENSE)
[![CI](https://img.shields.io/static/v1?label=CI&message=GitHub%20Actions&color=blue)](https://github.com/cppalliance/runvil-gateway/actions)
[![Rust](https://img.shields.io/static/v1?label=rust&message=1.99&color=blue)](rust-toolchain.toml)

# Runvil Gateway

One server that speaks the API other programs already call, and runs the models itself.

Runvil Gateway is one process that serves an OpenAI-shaped API for chat, embeddings, reranking, speech, and live transcription, and answers each request from a model on your own machine or from a cloud provider you configured. Because the routes follow standards existing clients already speak, an existing OpenAI client points at the Gateway and works, with no adapter and no second client library. Where a feature needs something a standard lacks, the Gateway adds the smallest opt-in extension that makes it possible, and leaves the standard path untouched for clients that do not ask. The full rule is in [`crates/gateway/README.md`](crates/gateway/README.md).

## Why it exists

- One API in front of both your own models and cloud ones. You change a model name in a request, not the client code or the URL.
- The same shape other tools already speak. Chat, embeddings, and models follow OpenAI's API, rerank follows the shape Jina and Cohere publish, and the realtime stream follows OpenAI's transcription protocol.
- Live transcription that revises itself. The realtime route can send hypothesis snapshots as it re-listens, behind an opt-in token, so a client never has to guess.
- A settings page in the browser, so a running Gateway can be configured without editing TOML by hand.

## What it serves

| Route | Standard it follows |
|---|---|
| `POST /v1/chat/completions` | OpenAI chat completions, including streaming |
| `POST /v1/embeddings` | OpenAI embeddings |
| `GET /v1/models` | OpenAI model listing |
| `POST /v1/audio/speech` | OpenAI speech synthesis |
| `POST /v1/audio/transcriptions` | OpenAI audio transcriptions |
| `GET /v1/realtime` with `intent=transcription` | OpenAI Realtime transcription protocol, over a WebSocket |
| `POST /v1/rerank` | The Jina and Cohere rerank shape, which vLLM also serves |
| `POST /v1/tools/web_search` | A Gateway extension, backed by Brave Search |
| `GET /v1/audio/voices` | A Gateway extension, the voice catalog |
| `/v1/cache` | A Gateway extension, a local blob cache that downloads and keeps files by sha256 |
| `/config/`, `/admin/*`, `/health` | The Gateway's own settings page, admin surface, and liveness probe ([a config save names a revision](crates/gateway/README.md#saving-the-configuration)) |

Requests are answered by a local model through a managed `llama-server` child, or forwarded to a cloud provider you configured. Chat, embeddings, and reranking work either way. Transcription runs on the Gateway's own speech pipeline, which decodes with whisper.cpp, and speech synthesis forwards to a speech endpoint you configure.

Every route except `/health` is authenticated by default. A loopback caller is trusted without a key unless you turn that off with `trust_loopback`, and the admin routes that read secrets or launch processes are reachable only from the same machine.

## Install

There are no published releases yet, so there is nothing to download or install from a URL today. Build from source instead.

You need Rust, and Node.js 22 for the settings page bundle. The toolchain is pinned in [`rust-toolchain.toml`](rust-toolchain.toml) and rustup installs it on the first cargo call.

```sh
git clone https://github.com/cppalliance/runvil-gateway
cd runvil-gateway
npm ci --prefix crates/gateway/config-ui/ui
cargo build --release
```

`cargo build` builds the Gateway on its own, because the workspace's default members name it. The binary lands at `target/release/runvil-gateway`, or `target/release/runvil-gateway.exe` on Windows.

The `npm ci` step is only needed because the settings page is compiled into the binary. A build without it drops the settings page, local models, web search, and speech:

```sh
cargo build --release --no-default-features
```

To run it, either point it at a config you already have, or let it write one on first launch:

```sh
cargo run --release -- --print-url
```

With no config anywhere, the Gateway generates one in `~/.runvil/gateway.toml`, binds loopback on a free port, and prints the settings URL, which carries the generated bearer key. If you already have a config, `--config PATH` names it instead. Everything else lives in that file, and the settings page edits it without you opening it: profiles, model endpoints, GPU pools, and the speech models.

On a desktop it stays in the system tray, and the tray opens the settings page. `--print-url` serves headless and prints the URL, which suits a server or CI. `runvil-gateway diagnostics` prints a JSON report of the state directory, config, logs, and discovery file without serving.

## Documentation

The Gateway guide, covering installation, configuration, profiles, local models, speech, and the settings page, lives in the main Runvil documentation at [runvil.org](https://runvil.org).

## Related projects

- [Runvil](https://github.com/cppalliance/runvil) is the product family this Gateway belongs to. It holds the desktop Workshop app and the prompt engine that talk to a Gateway.
- [runvil.org](https://runvil.org) is the website and the documentation.

## License

Runvil Gateway is distributed under the [Boost Software License 1.0](LICENSE), copyright The C++ Alliance, Inc.
