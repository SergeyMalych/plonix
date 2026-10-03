# Plonix community store

This folder is the default Plonix store: `index.json` lists community packages, and `packs/` holds the rule packs it points to. `plonix store` reads it from this repository's `main` branch.

To add or update a pack:

1. Add or edit `packs/<name>.json` ([format](../docs/detection-rules.md)).
2. Run `plonix rules check packs/<name>.json`. It must be valid, and it prints the pack's `sha256`.
3. Add or update the pack's entry in `index.json` with that `sha256`, the same `name` and `version` as the pack, and `"url": "packs/<name>.json"`. Bump `version` whenever the pack changes.
4. Run `cargo test`: it fails if any entry in `index.json` doesn't match its file.

Packs named in `crates/plonix-core/src/rulepack.rs` (`BUILTIN`) are also compiled into Plonix, so they work without installing anything.

Everything here is treated as untrusted by Plonix: validated, checksum-verified, never executed. See [docs/extensions.md](../docs/extensions.md#the-trust-boundary).
