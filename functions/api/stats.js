// GET https://plonix.io/api/stats: the public numbers behind plonix.io/analytics.
//
// Everything here is a total over many installs. A group smaller than K installs is never shown on its own:
// it is folded into "Other", or left out when "Other" would be smaller than K too. Breakdowns appear only once
// at least K installs reported in the window. Raw reports never leave the database.
//
// Reads the D1 database bound as USAGE (migrations/) and the download counts of the GitHub releases. The
// answer is cached for an hour, so the page costs at most one query batch an hour.

export const K = 5;
export const WINDOW_DAYS = 30;
const REPO = 'SergeyMalych/plonix';
const CACHE_S = 3600;

const day = (d) => d.toISOString().slice(0, 10);
const addDays = (d, n) => new Date(d.getTime() + n * 86400000);
const parse = (s) => {
  try {
    const v = JSON.parse(s || '{}');
    return v && typeof v === 'object' && !Array.isArray(v) ? v : {};
  } catch (_) {
    return {};
  }
};

/** Map of name -> Set of installs, as a list of { id, installs } with small groups folded or dropped. */
export function fold(groups, k = K, other = 'other') {
  const out = [];
  const rest = new Set();
  for (const [id, set] of groups) {
    if (set.size >= k) out.push({ id, installs: set.size });
    else for (const i of set) rest.add(i);
  }
  out.sort((a, b) => b.installs - a.installs || String(a.id).localeCompare(String(b.id)));
  if (rest.size >= k) out.push({ id: other, installs: rest.size });
  return out;
}

const add = (map, key, install) => {
  if (!map.has(key)) map.set(key, new Set());
  map.get(key).add(install);
};
const bump = (map, key, n) => map.set(key, (map.get(key) || 0) + n);

/**
 * The public summary of usage reports.
 * rows: { day, install_id, version, os, os_version, arch, counts, extra } from the last WINDOW_DAYS days.
 * totals: { ever, new30 } distinct installs ever, and those whose first report is in the window.
 */
export function summarize(rows, totals, today, k = K) {
  const end = new Date(`${today}T00:00:00Z`);
  const from = day(addDays(end, -(WINDOW_DAYS - 1)));
  const in7 = day(addDays(end, -6));
  rows = rows.filter((r) => r.day >= from && r.day <= today);
  const active = new Set(rows.map((r) => r.install_id));
  const perDay = new Map();
  const week = new Set();
  for (const r of rows) {
    add(perDay, r.day, r.install_id);
    if (r.day >= in7) week.add(r.install_id);
  }
  const daily = [];
  for (let i = WINDOW_DAYS - 1; i >= 0; i--) {
    const d = day(addDays(end, -i));
    const n = perDay.get(d)?.size || 0;
    daily.push({ day: d, installs: n >= k ? n : null });
  }
  const shown = (n) => (n >= k ? n : null);
  const out = {
    k,
    window_days: WINDOW_DAYS,
    installs: { ever: shown(totals.ever || 0), new_30d: shown(totals.new30 || 0), active_7d: shown(week.size), active_30d: shown(active.size) },
    daily,
    ready: active.size >= k,
  };
  if (!out.ready) return out;

  // The latest report of each install in the window describes it.
  const latest = new Map();
  for (const r of rows) {
    const cur = latest.get(r.install_id);
    if (!cur || r.day > cur.day) latest.set(r.install_id, r);
  }
  const versions = new Map(), oses = new Map(), osVersions = new Map(), arches = new Map();
  const profiles = new Map(), styles = new Map(), projectCounts = new Map();
  for (const [id, r] of latest) {
    const x = parse(r.extra);
    add(versions, r.version, id);
    add(oses, r.os, id);
    add(osVersions, `${r.os} ${String(r.os_version || '').split('.')[0] || '?'}`, id);
    add(arches, r.arch, id);
    if (r.extra) {
      add(profiles, typeof x.profile === 'string' ? x.profile : 'none', id);
      if (x.look && typeof x.look.style === 'string') add(styles, x.look.style, id);
      if (typeof x.projects === 'string') add(projectCounts, x.projects, id);
    }
  }

  const featureInstalls = new Map(), featureUses = new Map();
  const screenInstalls = new Map(), screenMinutes = new Map();
  const filterInstalls = new Map(), filterUses = new Map();
  const rejected = new Map(), requests = new Map(), hosts = new Map(), inScope = new Map();
  let minutes = 0;
  const dayMinutes = [];
  for (const r of rows) {
    const id = r.install_id;
    for (const [e, n] of Object.entries(parse(r.counts))) {
      if (!Number.isInteger(n) || n <= 0) continue;
      add(featureInstalls, e, id);
      bump(featureUses, e, n);
    }
    if (!r.extra) continue;
    const x = parse(r.extra);
    let today = 0;
    for (const [s, n] of Object.entries(x.minutes || {})) {
      if (!Number.isInteger(n) || n <= 0) continue;
      add(screenInstalls, s, id);
      bump(screenMinutes, s, n);
      today += n;
    }
    if (today > 0) dayMinutes.push(today);
    minutes += today;
    for (const [f, n] of Object.entries(x.filters || {})) {
      if (!Number.isInteger(n) || n <= 0) continue;
      add(filterInstalls, f, id);
      bump(filterUses, f, n);
    }
    for (const d of Object.keys(x.rejected || {})) add(rejected, d, id);
    for (const b of Object.keys(x.sizes?.requests || {})) add(requests, b, id);
    for (const b of Object.keys(x.sizes?.hosts || {})) add(hosts, b, id);
    for (const b of Object.keys(x.sizes?.in_scope || {})) add(inScope, b, id);
  }
  dayMinutes.sort((a, b) => a - b);
  const timed = new Set(rows.filter((r) => Object.keys(parse(r.extra).minutes || {}).length).map((r) => r.install_id));

  const withUses = (groups, uses) =>
    fold(groups, k, null)
      .filter((g) => g.id !== null)
      .map((g) => ({ ...g, uses: uses.get(g.id) || 0 }));
  out.features = withUses(new Map([...featureInstalls].filter(([e]) => !e.startsWith('screen_'))), featureUses);
  out.time =
    timed.size >= k
      ? {
          hours: Math.round(minutes / 60),
          median_minutes_per_day: dayMinutes[Math.floor(dayMinutes.length / 2)] || 0,
          screens: fold(screenInstalls, k, null)
            .filter((g) => g.id !== null)
            .map((g) => ({ ...g, hours: Math.round((screenMinutes.get(g.id) / 60) * 10) / 10 }))
            .sort((a, b) => b.hours - a.hours),
        }
      : null;
  out.filters = withUses(filterInstalls, filterUses);
  // "other" is any rejected host outside the built-in list; it is a count, never a name.
  out.rejected = fold(rejected, k, null).filter((g) => g.id !== null);
  out.versions = fold(versions, k);
  out.os = fold(oses, k);
  out.os_versions = fold(osVersions, k);
  out.arch = fold(arches, k);
  out.profiles = fold(profiles, k);
  out.styles = fold(styles, k);
  out.projects = fold(projectCounts, k);
  out.sizes = { requests: fold(requests, k), hosts: fold(hosts, k), in_scope: fold(inScope, k) };
  return out;
}

/** Download counts of the release files, by OS and by release. Updates are counted apart. */
export function downloads(releases) {
  const out = { total: 0, macos: 0, windows: 0, updates: 0, releases: [] };
  for (const r of releases || []) {
    if (r.draft) continue;
    let n = 0;
    for (const a of r.assets || []) {
      const c = a.download_count || 0;
      if (/\.app\.tar\.gz$/.test(a.name)) out.updates += c;
      else if (/\.(dmg|zip)$/.test(a.name)) (out.macos += c), (n += c);
      else if (/\.(exe|msi)$/.test(a.name)) (out.windows += c), (n += c);
    }
    out.total += n;
    out.releases.push({ version: String(r.tag_name || '').replace(/^v/, ''), downloads: n, published: String(r.published_at || '').slice(0, 10) });
  }
  return out;
}

async function fetchDownloads() {
  try {
    const res = await fetch(`https://api.github.com/repos/${REPO}/releases?per_page=100`, {
      headers: { 'User-Agent': 'plonix.io', Accept: 'application/vnd.github+json' },
    });
    return res.ok ? downloads(await res.json()) : null;
  } catch (_) {
    return null;
  }
}

async function fromDatabase(db, today) {
  const from = day(addDays(new Date(`${today}T00:00:00Z`), -(WINDOW_DAYS - 1)));
  const [rows, ever, fresh] = await db.batch([
    db.prepare('SELECT day, install_id, version, os, os_version, arch, counts, extra FROM usage_reports WHERE day >= ?').bind(from),
    db.prepare('SELECT COUNT(DISTINCT install_id) AS n FROM usage_reports'),
    db.prepare('SELECT COUNT(*) AS n FROM (SELECT MIN(day) AS first FROM usage_reports GROUP BY install_id) WHERE first >= ?').bind(from),
  ]);
  return summarize(rows.results || [], { ever: ever.results?.[0]?.n || 0, new30: fresh.results?.[0]?.n || 0 }, today);
}

export async function onRequestGet({ request, env, waitUntil }) {
  const cache = typeof caches !== 'undefined' ? caches.default : null;
  const key = new Request(new URL('/api/stats', request.url).toString());
  const hit = cache && (await cache.match(key));
  if (hit) return hit;
  const today = day(new Date());
  let usage = null;
  if (env.USAGE) {
    try {
      usage = await fromDatabase(env.USAGE, today);
    } catch (_) {
      usage = null;
    }
  }
  const body = { generated_at: new Date().toISOString(), collecting: !!env.USAGE, usage, downloads: await fetchDownloads() };
  const res = new Response(JSON.stringify(body), {
    headers: { 'Content-Type': 'application/json', 'Cache-Control': `public, max-age=${CACHE_S}`, 'Access-Control-Allow-Origin': '*' },
  });
  if (cache) waitUntil(cache.put(key, res.clone()));
  return res;
}

export function onRequest() {
  return new Response(null, { status: 405, headers: { Allow: 'GET' } });
}
