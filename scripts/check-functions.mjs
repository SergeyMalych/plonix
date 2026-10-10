// Tests for the website's functions (functions/api/*.js), which run on Cloudflare Pages with no build step.
// Node has no Pages runtime, so this loads each file as a module and checks its pure parts:
//   node scripts/check-functions.mjs
import { copyFileSync, mkdtempSync } from 'node:fs';
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
  const dl = stats.downloads([{ tag_name: 'v0.1.4', assets: [{ name: 'Plonix-macOS.dmg', download_count: 3 }, { name: 'Plonix-Windows-setup.exe', download_count: 2 }, { name: 'Plonix-macOS.app.tar.gz', download_count: 9 }, { name: 'latest.json', download_count: 50 }] }]);
  assert.deepEqual([dl.total, dl.macos, dl.windows, dl.updates], [5, 3, 2, 9]);
}

console.log('functions ok');
