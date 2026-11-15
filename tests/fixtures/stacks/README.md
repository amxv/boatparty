# Stack fixtures

Four small projects, one per built-in stack preset, each with one real
third-party dependency and a test that uses it. `boatparty init` in a copy
of one of them writes a `boatparty.toml` whose `test` command passes on a
sandbox, and the second run installs nothing.

| Fixture | Preset | Dependency | `boatparty cheers test` runs |
| --- | --- | --- | --- |
| `rust` | rust | itoa (crates.io) | `cargo test --locked` |
| `node` | node | ms (npm) | `npm test` |
| `python` | python | attrs (PyPI) | `.venv/bin/python -m pytest` |
| `go` | go | github.com/google/go-cmp | `go test ./...` |

The lockfiles were generated on a boat sandbox. Copy a fixture into its own
Git repository before running boatparty in it.
