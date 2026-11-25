// Tests for the website's functions (functions/api/*.js), which run on Cloudflare Pages with no build step.
// Node has no Pages runtime, so this loads each file as a module and checks its pure parts:
//   node scripts/check-functions.mjs
import { copyFileSync, mkdtempSync, readdirSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import assert from 'node:assert/strict';

const dir = mkdtempSync(join(tmpdir(), 'plonix-functions-'));
const load = async (name) => {
  const to = join(dir, `${name}.mjs`);
  copyFileSync(new URL(`../functions/api/${name}.js`, import.meta.url), to);
  return import(to);
};
const usage = await load('usage');
const stats = await load('stats');
const hit = await load('hit');

// usage.js keeps only known names, whole numbers and allowed values.
{
  const base = { install_id: 'a'.repeat(32), version: '0.2.0', os: 'macos', os_version: '15.1.2', arch: 'aarch64', counts: { bench_send: 3, nope: 4, scan_run: 1.5 } };
  const v1 = usage.clean({ schema: 1, ...base });
  assert.deepEqual(v1.counts, { bench_send: 3 });
  assert.equal(v1.osVersion, '15.1');
  assert.equal(v1.extra, null);
  const v2 = usage.clean({
    schema: 2,
    ...base,
    minutes: { traffic: 30, '../etc': 5, bench: 99999 },
    filters: { host: 2, '-status': 1, 'host:secret.example': 1 },
    profile: 'my own words',
    look: { style: 'studio', theme: 'neon' },
    projects: '2-5',
    sizes: { requests: { '1k-10k': 1, 4321: 1 }, hosts: { '2-5': 1 } },
    rejected: { 'google-analytics.com': 2, 'secret-target.internal': 1, other: 3 },
    url: 'https://secret.example/',
  });
  assert.deepEqual(v2.extra.minutes, { traffic: 30, bench: 1440 });
  assert.deepEqual(v2.extra.filters, { host: 2, '-status': 1 });
  assert.equal(v2.extra.profile, 'none');
  assert.deepEqual(v2.extra.look, { style: 'studio' });
  assert.deepEqual(v2.extra.sizes.requests, { '1k-10k': 1 });
  assert.deepEqual(v2.extra.rejected, { 'google-analytics.com': 2, other: 3 });
  assert.ok(!JSON.stringify(v2).includes('secret'));
  assert.equal(usage.clean({ schema: 3, ...base }), null);
  // Nothing a client sends gets through as free text.
  for (const version of ['<script>alert(1)</script>', '0.1.4"; DROP TABLE usage_reports;--', '1.2.3\n', '1.2', ' 1.2.3', '1.2.3-' + 'x'.repeat(30), ['1.2.3'], { toString: () => '1.2.3' }]) {
    assert.equal(usage.clean({ schema: 2, ...base, version }), null, JSON.stringify(version));
  }
  const odd = usage.clean({ schema: 2, ...base, os: '<img src=x onerror=alert(1)>', arch: 'x86_64\u0000', os_version: '15.1<b>', counts: { __proto__: 5, constructor: 1, bench_send: 2 }, profile: { toString: () => 'bug-hunter' }, look: { style: ['studio'] } });
  assert.equal(odd.os, 'other');
  assert.equal(odd.arch, 'other');
  assert.equal(odd.osVersion, '');
  assert.deepEqual(odd.counts, { bench_send: 2 });
  assert.equal(odd.extra.profile, 'none');
  assert.deepEqual(odd.extra.look, {});
  assert.equal(usage.clean({ schema: 2, ...base, os_version: '10.0.22631.4317' }).osVersion, '10.0');
  assert.equal(usage.clean({ schema: 2, ...base, version: '0.0.0-test' }).version, '0.0.0-test');
  assert.equal(usage.clean({ schema: 2, ...base, install_id: 'x' }), null);
}

// stats.js never shows a group smaller than K installs.
{
  assert.deepEqual(stats.fold(new Map([['a', new Set([1, 2, 3, 4, 5])], ['b', new Set([6])], ['c', new Set([7, 8])]]), 5), [{ id: 'a', installs: 5 }]);
  assert.deepEqual(stats.fold(new Map([['a', new Set([1, 2, 3, 4, 5])], ['b', new Set([6, 7, 8])], ['c', new Set([8, 9, 10])]]), 5), [
    { id: 'a', installs: 5 },
    { id: 'other', installs: 5 },
  ]);
  const today = '2026-10-10';
  const row = (i, day, extra) => ({ day, install_id: String(i), version: '0.2.0', os: 'macos', os_version: '15.1', arch: 'aarch64', counts: '{"bench_send":2}', extra: JSON.stringify(extra) });
  const few = stats.summarize([row(1, today, {}), row(2, today, {})], { ever: 2, new30: 2 }, today);
  assert.equal(few.ready, false);
  assert.equal(few.installs.active_30d, null);
  assert.equal(few.features, undefined);
  const rows = [];
  for (let i = 0; i < 6; i++) rows.push(row(i, today, { minutes: { traffic: 10 }, profile: i < 5 ? 'bug-hunter' : 'red-teamer', rejected: { 'sentry.io': 1 } }));
  rows.push(row(99, '2026-08-01', {}));
  const s = stats.summarize(rows, { ever: 7, new30: 6 }, today);
  assert.equal(s.ready, true);
  assert.equal(s.installs.active_30d, 6);
  assert.deepEqual(s.profiles, [{ id: 'bug-hunter', installs: 5 }]);
  assert.deepEqual(s.features, [{ id: 'bench_send', installs: 6, uses: 12 }]);
  assert.equal(s.time.hours, 1);
  assert.deepEqual(s.rejected, [{ id: 'sentry.io', installs: 6 }]);
  assert.equal(s.daily.length, 30);
  assert.equal(s.daily[29].installs, 6);
  assert.equal(s.daily[0].installs, null);
  // A bad name in a row never reaches the page: it is counted as "other" at most.
  const bad = [];
  for (let i = 0; i < 6; i++) bad.push({ ...row(i, today, {}), version: '<b>x</b>' });
  assert.ok(!JSON.stringify(stats.summarize(bad, { ever: 6, new30: 6 }, today)).includes('<'));
  assert.ok(stats.safeId('-host') && stats.safeId('macos 15') && stats.safeId('0.1.4-beta.1') && !stats.safeId('a"b') && !stats.safeId(''));
  assert.deepEqual(stats.downloads([{ tag_name: '<img>', assets: [{ name: 'a.dmg', download_count: '9' }] }]).releases, []);
  const dl = stats.downloads([{ tag_name: 'v0.1.4', assets: [{ name: 'Plonix-macOS.dmg', download_count: 3 }, { name: 'Plonix-Windows-setup.exe', download_count: 2 }, { name: 'Plonix-macOS.app.tar.gz', download_count: 9 }, { name: 'latest.json', download_count: 50 }] }]);
  assert.deepEqual([dl.total, dl.macos, dl.windows, dl.updates], [5, 3, 2, 9]);
}

// hit.js counts pages and sources by fixed names only, and keeps nothing about the visitor.
{
  const docs = readdirSync(new URL('../site/docs/', import.meta.url)).filter((f) => f.endsWith('.html') && f !== 'index.html').map((f) => f.slice(0, -5)).sort();
  assert.deepEqual([...hit.DOCS].sort(), docs, 'hit.js DOCS must list every page in site/docs/');
  assert.equal(hit.page('/'), 'home');
  assert.equal(hit.page('/index.html'), 'home');
  assert.equal(hit.page('/analytics'), 'analytics');
  assert.equal(hit.page('/docs/'), 'docs');
  assert.equal(hit.page('/docs/install.html'), 'docs/install');
  assert.equal(hit.page('/docs/install'), 'docs/install');
  assert.equal(hit.page('/docs/<script>'), 'other');
  assert.equal(hit.page('/docs/nope'), 'other');
  assert.equal(hit.page({}), 'other');
  assert.equal(hit.source('www.linkedin.com'), 'linkedin');
  assert.equal(hit.source('lnkd.in'), 'linkedin');
  assert.equal(hit.source('old.reddit.com'), 'reddit');
  assert.equal(hit.source('news.ycombinator.com'), 'hackernews');
  assert.equal(hit.source('www.google.co.il'), 'search');
  assert.equal(hit.source('t.co'), 'x');
  assert.equal(hit.source(''), 'direct');
  assert.equal(hit.source('', 'hn'), 'hackernews');
  assert.equal(hit.source('', '<b>'), 'direct');
  assert.equal(hit.source('evil.example'), 'other');
  assert.equal(hit.source('notlinkedin.com'), 'other');
  assert.equal(hit.source('plonix.io'), null);
  assert.equal(hit.source('a<b>.com'), 'other');
  const now = new Date('2026-11-25T15:30:00Z');
  const entry = hit.rows({ kind: 'view', path: '/', ref: 'www.reddit.com', src: '' }, { country: 'IL', ua: 'Mozilla/5.0', now });
  assert.deepEqual(entry, [
    { day: '2026-11-25', hour: 15, page: 'home', source: 'reddit', country: 'IL', kind: 'view' },
    { day: '2026-11-25', hour: 15, page: 'home', source: 'reddit', country: 'IL', kind: 'visit' },
  ]);
  assert.equal(hit.rows({ kind: 'view', path: '/docs/', ref: 'plonix.io' }, { now }).length, 1);
  assert.equal(hit.rows({ kind: 'visit', path: '/' }, { now }), null);
  assert.equal(hit.rows({ kind: 'view', path: '/' }, { ua: 'Googlebot/2.1', now }), null);
  assert.equal(hit.rows({ kind: 'view', path: '/' }, { country: '<x>', now })[0].country, 'XX');
  assert.deepEqual(hit.rows({ kind: 'download_macos', path: '/', ref: '' }, { now }).map((r) => r.kind), ['download_macos']);
  for (const id of hit.SOURCE_NAMES) assert.ok(stats.safeId(id));

  // stats.js adds the hourly totals up per range.
  const rows = [
    ...entry.map((r) => ({ ...r, n: 3 })),
    { day: '2026-11-25', hour: 9, kind: 'view', page: 'docs/install', source: 'internal', country: 'IL', n: 2 },
    { day: '2026-11-25', hour: 9, kind: 'download_windows', page: 'home', source: 'direct', country: 'US', n: 1 },
    { day: '2026-11-20', hour: 9, kind: 'visit', page: 'home', source: 'linkedin', country: 'US', n: 4 },
    { day: '2026-11-20', hour: 9, kind: 'view', page: 'home', source: 'linkedin', country: 'US', n: 4 },
    { day: '2026-10-01', hour: 9, kind: 'view', page: 'home', source: 'linkedin', country: 'US', n: 99 },
    { day: '2026-11-25', hour: 15, kind: 'view', page: '<b>', source: 'x', country: 'US', n: 1 },
  ];
  const site = stats.siteSummary(rows, now);
  assert.equal(site['24h'].visits, 3);
  assert.equal(site['24h'].views, 6);
  assert.equal(site['24h'].downloads.windows, 1);
  assert.equal(site['24h'].series.length, 24);
  assert.equal(site['24h'].series[23].visits, 3);
  assert.deepEqual(site['24h'].sources, [{ id: 'reddit', count: 3 }]);
  assert.equal(site['7d'].visits, 7);
  assert.equal(site['7d'].series.length, 7);
  assert.equal(site['30d'].series.length, 30);
  assert.equal(site['30d'].views, 10);
  assert.deepEqual(site['30d'].sources, [{ id: 'linkedin', count: 4 }, { id: 'reddit', count: 3 }]);
  assert.deepEqual(site['24h'].pages, [{ id: 'home', count: 3 }, { id: 'docs/install', count: 2 }]);
  assert.ok(!JSON.stringify(site).includes('<'));
}

console.log('functions ok');
