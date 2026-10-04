# Plonix website

The site at https://plonix.io. A static page with no build step: `index.html` and the images in `assets/`.

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
