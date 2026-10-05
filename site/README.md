# Plonix website

The site at https://plonix.io. A static site with no build step: `index.html`, the images in `assets/`, and the documentation pages in `docs/`.

The pages in `docs/` are generated from the Markdown in the repository's `docs/` folder, and committed. After changing a doc, regenerate them (Node, no dependencies):

```sh
node scripts/build-docs.mjs           # writes site/docs/
node scripts/build-docs.mjs --check   # what CI runs: fails if site/docs/ is out of date
```

Links between docs become links between pages, and links to other files in the repository go to GitHub.

Preview it locally:

```sh
python3 -m http.server -d site 8000   # then open http://localhost:8000
```

It is hosted on Cloudflare Pages, connected to this repository. Every push to `main` publishes it, and pull requests get a preview link. The Pages project uses these settings:

| Setting | Value |
| --- | --- |
| Production branch | `main` |
| Build command | none |
| Build output directory | `site` |
| Custom domain | `plonix.io` |
