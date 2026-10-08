# The community Market

This folder is the community Market: packages written and maintained by their authors, shown in every Plonix next to the Plonix Market with a **Community** badge. `index.json` lists them and `index.json.sig` is its signature, made with the community key (not the Plonix Market key).

The files themselves stay in their authors' repositories, attached to GitHub releases. The index pins each one by SHA-256, so a release file that is replaced later is refused.

## Adding your package

1. Publish it in your own GitHub repository with an open-source license, as a file attached to a release: a `.plonixext` extension (`plonix extensions pack ./my-extension`), a skill (`.md`) or a pack (`.json`). Check it first with `plonix extensions check`, `plonix skills check`, `plonix rules check` or `plonix filters check`.
2. Get its checksum: `shasum -a 256 my-extension.plonixext`.
3. Open a pull request that adds one entry to `packages` in `index.json`:

```json
{
  "name": "graphql-notes",
  "kind": "extension",
  "version": "1.2.0",
  "description": "Notes GraphQL operations and the fields each one asks for.",
  "author": "jsmith",
  "homepage": "https://github.com/jsmith/graphql-notes",
  "url": "https://github.com/jsmith/graphql-notes/releases/download/v1.2.0/graphql-notes.plonixext",
  "sha256": "…64 lowercase hex characters…",
  "about": ["A few short paragraphs for the package's page in the Market."]
}
```

- `name` and `version` must match the file. A name the Plonix Market already uses is not shown.
- `author` is your GitHub name, and `homepage` the repository the release belongs to.
- Bundles, tools and `requires` are not accepted here.
- A new version replaces your entry: new `version`, `url` and `sha256`.

## What happens next

- The **Market lists** workflow downloads the file, checks the checksum and validates it in full.
- A reviewer reads what the package asks to do (its capabilities, for an extension) and checks that the release was built from the repository's source at that tag.
- When the pull request merges, the index is signed and the package shows up in everyone's Market under Community.

Community packages are not reviewed line by line by the Plonix maintainers, and the Market says so. If one turns out to be harmful, the maintainers add it to the block list (`store/blocked.json`) and every Plonix switches it off. See [docs/market.md](../docs/market.md#the-community-market).
