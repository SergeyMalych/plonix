# Plonix website

A static page with no build step: `index.html` and the images in `assets/`.

Preview it locally:

```sh
python3 -m http.server -d site 8000   # then open http://localhost:8000
```

Every push to `main` that changes `site/` publishes it to GitHub Pages through `.github/workflows/site.yml`. Pages must be set to deploy from GitHub Actions once, in the repository's Settings › Pages › Source.
