# Gateway

The gateway is the server process that holds every model credential. It serves an OpenAI-compatible API, routes each request to a cloud provider or a local model, manages the model catalog and local model downloads, and, when built with them, serves web search, speech, and a browser settings page. It starts serving before any slow model work begins, and on desktop machines it runs from the system tray. The [gateway guide](https://gh.cppalliance.org/runvil/gateway/) documents its configuration, profiles, and endpoints.

## The committed OpenAPI description

`openapi.json` is an OpenAPI 3.1 document generated from the routes and the types. It is not written by hand, and a hand edit is overwritten at the next run. Derive the file, do not edit it:

```console
BLESS_OPENAPI=1 cargo test -p gateway --lib openapi
```

The command refuses a build that is missing `local`, `web-search`, `config-ui`, or `stt`, because the committed file describes the default build. `cargo test -p gateway --lib openapi` compares the committed file with a fresh generation and fails when they differ, naming the JSON pointers that moved. Edit the routes or the types, then run the command above to rewrite the artifact in the same change, and commit both.
