# boat API fixtures

Bodies used by the provider tests.

Recorded live from `https://boat.dev/api/v1` on 2026-11-15 with a scoped key (request ids are the real ones):

- `errors/403-api-key-action-forbidden.json`: `POST /sandboxes/{id}/commands` with a key that lacks `exec`.
- `errors/404-not-found.json`: `GET /sandboxes/bx_22222222`.
- `errors/401-unauthorized.json`: `GET /limits` with an invalid key.
- `errors/400-invalid-query.json`: `GET /sandboxes?bogus=1`.
- `errors/400-invalid-type.json`: `POST /sandboxes` with `{"type":"bogus"}`.
- `limits.json`: `GET /limits` on a $20 plan account.

Written from the documented shapes (https://docs.boat.dev/api/v1.md "Error model", https://docs.boat.dev/limits.md "Errors and what to do", and the OpenAPI `ErrorEnvelope`), because producing them live would mean exhausting a limit or waiting for an outage. Their request ids start with `req_doc_`:

- `errors/429-rate-limited-hour.json` (fields in `error.details`) and `errors/429-rate-limited-top-level.json` (fields at the top level): the docs name `window`, `limit`, `cause`, `nextStartAt` and `retryAfterSeconds` without fixing their position, so both are covered.
- `errors/429-ip-rate-limit.json`, `errors/429-limit-reached.json`, `errors/503-out-of-capacity.json`, `errors/409-snapshot-data-expired.json`, `errors/402-billing-required.json`, `errors/502-sandbox-direct-failed-restoring.json` (the body in the docs, verbatim).
- `errors/409-unknown-code.json`: an invented code, to prove unknown codes stay distinguishable.
