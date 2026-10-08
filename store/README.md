# The Plonix Market

This folder is the Plonix Market: `index.json` lists every package, `index.json.sig` is its signature, and the folders hold the files it points to:

- `skills/`: agent skills ([format](../docs/market.md#writing-a-skill))
- `packs/`: detection rule packs ([format](../docs/detection-rules.md))
- `filterpacks/`: filter packs ([format](../docs/filters.md))
- `lists/`: payload list packs ([format](../docs/bench.md))
- `platforms/`: bug bounty platform packs ([format](../docs/programs.md#platform-packs)). Packs built into Plonix, such as `hackerone.json`, are listed in the official Market automatically and stay out of `index.json`.
- `extensions/`: extension packages (`.plonixext`) and listings of ones still to come ([extensions](../docs/extensions.md))

Bundles live only in `index.json`: a bundle entry has no `url` or `sha256`, just `requires`.

Plonix reads the index from this repository's `main` branch, and a copy is built into every Plonix (`SNAPSHOT` in `crates/plonix-core/src/market.rs`).

## Adding or updating a package

1. Add or edit the file. Validate it: `plonix skills check`, `plonix rules check` or `plonix filters check`. Each prints the file's `sha256`.
2. Add or update its entry in `index.json` with that `sha256`, the same `name` and `version` as the file, and its relative `url`. Bump `version` whenever the file changes. Names are unique across the whole index.
3. A new file also goes in `SNAPSHOT` in `crates/plonix-core/src/market.rs`.
4. Open a pull request. The **Market lists** workflow (`.github/workflows/market.yml`) checks every package and signs `index.json` with the Plonix Market key, which it keeps as a repository secret, then runs CI again on the signed commit. Merging the pull request is the review: the new signature reaches people only once `main` carries it. Pull requests from forks get the checks but not the key; a maintainer brings the change into a branch of this repository to have it signed.

To sign by hand instead:

```sh
plonix market check store/index.json
plonix market sign store/index.json --key <maintainer key>
```

`blocked.json` is the block list: packages the maintainers have pulled, which Plonix switches off or removes wherever they came from ([docs/market.md](../docs/market.md#the-block-list)). The same workflow signs it.

Everything here is treated as untrusted by Plonix: validated, checksum-verified against the signed index, never executed. See [docs/market.md](../docs/market.md#validated-packages).
