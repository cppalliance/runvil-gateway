# gateway-api-discovery

- This is the only implementation inside this repository of gateway-discovery-file discovery, atomic owner-only publication, shutdown removal, stale resolution, launch locking, health probing, and authenticated shutdown, and the one definition of the gateway log path. Consumers here do not reimplement those contracts.
- `tests/fixtures/gateway.json` is the file as readers outside this repository pin it, such as the client in `cppalliance/gateway-client`. The test `the_writer_output_matches_the_contract_fixture` fails when the writer's output stops matching it, so a change to a field of the file is a change to that fixture and to every reader that pins it.
