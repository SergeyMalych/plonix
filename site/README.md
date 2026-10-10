# Plonix website

The site at https://plonix.io. A static site with no build step: `index.html`, the images in `assets/`, and the documentation pages in `docs/`.

The pages in `docs/` are generated from the Markdown in the repository's `docs/` folder, and committed. After changing a doc, regenerate them (Node, no dependencies):

```sh
node scripts/build-docs.mjs           # writes site/docs/
node scripts/build-docs.mjs --check   # what CI runs: fails if site/docs/ is out of date
```

Links between docs become links between pages, and links to other files in the repository go to GitHub.

The look of the site (colors, type, layout rules, components) is written down in [DESIGN.md](DESIGN.md). Read it before adding a section, and update it when the design changes.

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
| Root directory | the repository root (empty) |
| Custom domain | `plonix.io` |

The Pages project also serves two functions from `functions/` at the repository root. `POST /api/usage`, from [`functions/api/usage.js`](../functions/api/usage.js) at the repository root (Pages looks for `functions/` in the root directory, not in the output directory). It receives the anonymous usage reports Plonix sends at most once a day, described in [docs/privacy.md](../docs/privacy.md). It checks each report's shape and size, keeps only the expected fields and the known feature names, and writes one row per install per day to a D1 database. IP addresses and headers are not stored. Until the database is bound, it answers `204` and stores nothing, so the site works either way.

`GET /api/stats`, from [`functions/api/stats.js`](../functions/api/stats.js), feeds the public page [plonix.io/analytics](analytics.html). It reads the last 30 days of reports and the download counts of the GitHub releases, and answers totals only: any group of fewer than five installs is folded into "other" or left out. The answer is cached for an hour. Without the database the page still shows downloads. `node scripts/check-functions.mjs` tests both functions (CI runs it).

There is no `wrangler.toml` on purpose: with one, Pages takes its settings from the file instead of the dashboard. The binding below is set in the dashboard.

## Usage statistics: one-time setup

1. **Create the database.** Cloudflare dashboard › Storage & Databases › D1 › Create database. Name it `plonix-usage`.
   (Or from a terminal: `npx wrangler d1 create plonix-usage`.)
2. **Create the table.** Open the database › Console, paste the contents of [`migrations/0001_usage.sql`](../migrations/0001_usage.sql) and run it, then the same for [`migrations/0002_usage_extra.sql`](../migrations/0002_usage_extra.sql). On a database made before `0002` existed, run only `0002`.
   (Or: `npx wrangler d1 execute plonix-usage --remote --file migrations/0001_usage.sql`, then the same with `0002_usage_extra.sql`.)
3. **Bind it to the site.** Workers & Pages › the plonix.io Pages project › Settings › Bindings › Add › D1 database. Variable name `USAGE`, database `plonix-usage`. Add it for Production. Preview is optional; without it, preview deployments store nothing.
4. **Redeploy.** Bindings take effect on the next deployment: push to `main`, or Deployments › the latest one › Retry deployment.
5. **Check it.** This should print `HTTP/2 204`:

   ```sh
   curl -si https://plonix.io/api/usage -H 'Content-Type: application/json' \
     -d '{"schema":2,"install_id":"00000000000000000000000000000000","version":"0.0.0-test","os":"macos","os_version":"15.1","arch":"aarch64","counts":{"app_launched":1},"minutes":{"traffic":1}}' | head -1
   ```

   The row shows up in the D1 console. Remove it with `DELETE FROM usage_reports WHERE version = '0.0.0-test';`

## Reading the numbers

Run these in the D1 console (or `npx wrangler d1 execute plonix-usage --remote --command "…"`). Each install reports at most once a day, and only after it was used, so a row means "this install was used around this day".

Installs reporting per day, last 30 days:

```sql
SELECT day, COUNT(DISTINCT install_id) AS installs
FROM usage_reports
WHERE day >= date('now', '-30 days')
GROUP BY day ORDER BY day DESC;
```

Active installs in the last 7 and 30 days:

```sql
SELECT
  COUNT(DISTINCT CASE WHEN day >= date('now', '-7 days') THEN install_id END) AS weekly,
  COUNT(DISTINCT install_id) AS monthly
FROM usage_reports
WHERE day >= date('now', '-30 days');
```

New installs per day (the first day each one reported):

```sql
SELECT first_day, COUNT(*) AS new_installs
FROM (SELECT install_id, MIN(day) AS first_day FROM usage_reports GROUP BY install_id)
GROUP BY first_day ORDER BY first_day DESC LIMIT 30;
```

Versions in use (each install's latest report in the last 30 days):

```sql
SELECT version, COUNT(*) AS installs
FROM (SELECT install_id, version, MAX(day) FROM usage_reports WHERE day >= date('now', '-30 days') GROUP BY install_id)
GROUP BY version ORDER BY installs DESC;
```

Operating systems and CPU types:

```sql
SELECT os, os_version, arch, COUNT(DISTINCT install_id) AS installs
FROM usage_reports
WHERE day >= date('now', '-30 days')
GROUP BY os, os_version, arch ORDER BY installs DESC;
```

Feature use in the last 30 days, with how many installs used each feature:

```sql
SELECT f.key AS feature, SUM(f.value) AS uses, COUNT(DISTINCT r.install_id) AS installs
FROM usage_reports AS r, json_each(r.counts) AS f
WHERE r.day >= date('now', '-30 days')
GROUP BY f.key ORDER BY uses DESC;
```

Keep a year of reports and drop the rest:

```sql
DELETE FROM usage_reports WHERE day < date('now', '-365 days');
```
