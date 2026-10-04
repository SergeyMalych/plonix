# Contributing to Plonix

Plonix is early, and this is a good time to shape it. Issues and discussions about workflows, pain points and design are as valuable as code. Thanks for helping.

By taking part you agree to follow the [Code of Conduct](CODE_OF_CONDUCT.md). Found a security problem in Plonix itself? Please report it privately as described in [SECURITY.md](SECURITY.md), not in an issue.

## Ways to contribute

- **Report a bug or suggest a feature** with the [issue templates](https://github.com/SergeyMalych/plonix/issues/new/choose).
- **Write a detection rule pack.** No Rust needed: see [Rule packs](#rule-packs) below.
- **Improve the docs.** If something in the README or `docs/` was unclear or wrong, a fix is very welcome.
- **Change the code.** For anything larger than a small fix, open an issue first so we can agree on the approach before you spend time on it.

## Building and testing

Requirements: Rust stable (edition 2024) and a C toolchain. On macOS, `xcode-select --install` is enough. SQLite is bundled.

```sh
git clone https://github.com/SergeyMalych/plonix.git
cd plonix

cargo build --workspace                     # everything, including the app
cargo test --workspace                      # all tests
cargo run -p plonix -- status               # the CLI from source
cargo run -p plonix-app                     # the app window, without bundling
```

On Linux the app crate needs the WebKitGTK development libraries:

```sh
sudo apt-get install -y libwebkit2gtk-4.1-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev librsvg2-dev
```

If you only work on the engine or the CLI, you can skip the app:

```sh
cargo test --workspace --exclude plonix-app
```

To build the bundled `Plonix.app` on a Mac:

```sh
cargo install tauri-cli --version "^2" --locked   # once
cd crates/plonix-app
cargo tauri build --bundles app
```

### Checks that CI runs

Every pull request runs these. Running them before you push saves a round trip:

```sh
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D clippy::correctness -D clippy::suspicious
cargo deny check                                  # cargo install cargo-deny --locked
```

`cargo deny` checks new dependencies for known advisories, licenses Plonix can't ship with, and sources other than crates.io. If you add a dependency with a license that isn't in `deny.toml` yet, say so in the pull request.

The code is not formatted with `rustfmt`. Please don't run `cargo fmt` across the tree or reformat code you aren't otherwise changing; it buries the real change in the diff. Match the style of the code around you.

## Pull requests

- Keep each pull request focused on one change.
- Include tests for engine behavior. Engine tests live next to the code and in `crates/plonix-core/tests/`, CLI tests in `crates/plonix-cli/tests/`.
- Update the README or the relevant page in `docs/` when what users see changes.
- Write commit messages and the pull request description for users: say what changes for them, not how the code moved. "Keep your place in Map: open requests in place" beats "refactor map state".
- Anything that sends requests must go through the engine's scope choke point, and anything agents can reach must stay read-only unless the user opted in. Pull requests that weaken either won't be merged.

There is no Contributor License Agreement and no DCO sign-off. By contributing you agree that your contribution is licensed under the [Apache License 2.0](LICENSE), like the rest of Plonix.

## Rule packs

The easiest way to contribute is a **detection rule pack**: a JSON file that teaches Plonix to recognise a technology. No Rust needed.

1. Write the pack and run `plonix rules check your-pack.json` until it's clean.
2. Try it on real traffic with `plonix rules add your-pack.json` and `plonix tech`.
3. Open a pull request that adds `store/packs/<name>.json` and its entry in `store/index.json`, with the `sha256` that `plonix rules check` prints. `cargo test` checks that every index entry matches its file.

Prefer specific signals over generic ones, and reuse existing technology ids so detections merge. The full format and review checklist are in [docs/detection-rules.md](docs/detection-rules.md#contributing-a-pack). Scan packs (`store/scanpacks/`) follow the same model; see [docs/scanning.md](docs/scanning.md). New checks must be bounded and non-destructive, or clearly labeled as intrusive.

## Use Plonix responsibly

Use Plonix, and test your changes, only against systems you are authorized to test.
