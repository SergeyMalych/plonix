// POST https://plonix.io/api/hit: the website's page counter (site/assets/count.js sends it).
//
// Nothing here can tell visitors apart. There are no cookies and no ids, and the IP address, User-Agent and
// other headers are never stored. A hit only adds one to an hourly total keyed by:
//   kind     view, visit (a view that came from outside the site), download_macos or download_windows
//   page     one of the site's own pages, from a fixed list (anything else is "other")
//   source   where the visit came from, as a fixed name such as linkedin or reddit (any other site is "other")
//   country  the two-letter country Cloudflare reports for the request
// The totals are kept in the D1 database bound as USAGE, in the site_hits table (migrations/0003_site_hits.sql).

export const KINDS = ['view', 'download_macos', 'download_windows'];

export const DOCS = [
  'agents', 'app', 'bench', 'callbacks', 'code-signing', 'crash-reports', 'detection-rules', 'extensions', 'filters', 'findings',
  'getting-started', 'install', 'map', 'market', 'privacy', 'programs', 'projects', 'releasing', 'rules', 'scanning', 'scope',
  'setup', 'traffic', 'updates', 'users',
];

/** A site path as a fixed page name: home, analytics, docs, docs/<name> or other. */
export function page(path) {
  if (typeof path !== 'string' || path.length > 200) return 'other';
  const p = path.replace(/\.html$/, '').replace(/\/index$/, '/').replace(/\/+$/, '') || '/';
  if (p === '/') return 'home';
  if (p === '/analytics') return 'analytics';
  if (p === '/docs') return 'docs';
  const m = /^\/docs\/([a-z-]+)$/.exec(p);
  return m && DOCS.includes(m[1]) ? `docs/${m[1]}` : 'other';
}

// Referring sites by name. A host matches a domain when it is that domain or a subdomain of it.
const SOURCES = [
  ['linkedin', ['linkedin.com', 'lnkd.in']],
  ['reddit', ['reddit.com', 'redd.it']],
  ['hackernews', ['news.ycombinator.com', 'ycombinator.com', 'hn.algolia.com', 'hckrnws.com']],
  ['github', ['github.com', 'github.io']],
  ['x', ['t.co', 'x.com', 'twitter.com']],
  ['facebook', ['facebook.com', 'fb.com', 'messenger.com']],
  ['bluesky', ['bsky.app']],
  ['mastodon', ['mastodon.social', 'infosec.exchange', 'mastodon.online']],
  ['youtube', ['youtube.com', 'youtu.be']],
  ['lobsters', ['lobste.rs']],
  ['producthunt', ['producthunt.com']],
  ['discord', ['discord.com', 'discordapp.com']],
  ['slack', ['slack.com']],
  ['telegram', ['t.me', 'telegram.org']],
  ['whatsapp', ['whatsapp.com']],
  ['ai', ['chatgpt.com', 'openai.com', 'claude.ai', 'perplexity.ai', 'gemini.google.com', 'copilot.microsoft.com']],
  ['search', ['google.com', 'bing.com', 'duckduckgo.com', 'search.brave.com', 'ecosia.org', 'kagi.com', 'yandex.ru', 'yandex.com', 'baidu.com', 'yahoo.com', 'startpage.com', 'qwant.com']],
];
export const SOURCE_NAMES = [...SOURCES.map(([n]) => n), 'direct', 'other'];
// What people put in ?ref= or ?utm_source= links.
const TAGS = { hn: 'hackernews', ycombinator: 'hackernews', li: 'linkedin', twitter: 'x', fb: 'facebook', gh: 'github' };

const matches = (host, domain) => host === domain || host.endsWith('.' + domain);

/** Where a visit came from: a known source name, "direct", "other", or null when it came from this site. */
export function source(ref, tag, self = 'plonix.io') {
  const t = typeof tag === 'string' ? tag.trim().toLowerCase() : '';
  if (t) {
    const named = TAGS[t] || t;
    if (SOURCE_NAMES.includes(named) && named !== 'other' && named !== 'direct') return named;
  }
  const host = typeof ref === 'string' ? ref.trim().toLowerCase().replace(/\.$/, '') : '';
  if (!host) return 'direct';
  if (host.length > 253 || !/^[a-z0-9.-]+$/.test(host)) return 'other';
  if (matches(host, self) || host.endsWith('.plonix.pages.dev')) return null;
  // Google, Yahoo and the rest use many country domains: google.co.il, google.de and so on.
  if (/(^|\.)(google|bing|yahoo|yandex|duckduckgo)\.[a-z.]{2,8}$/.test(host)) return 'search';
  for (const [name, domains] of SOURCES) if (domains.some((d) => matches(host, d))) return name;
  return 'other';
}

const BOT = /bot|crawl|spider|slurp|preview|headless|lighthouse|pagespeed|facebookexternalhit|embedly|curl|wget|python|go-http|java\//i;

/** The rows a hit adds to, or null when it is not counted. */
export function rows(body, { country, ua, now = new Date() } = {}) {
  if (!body || typeof body !== 'object' || Array.isArray(body)) return null;
  if (!KINDS.includes(body.kind)) return null;
  if (typeof ua === 'string' && BOT.test(ua)) return null;
  const src = source(body.ref, body.src);
  const base = {
    day: now.toISOString().slice(0, 10),
    hour: now.getUTCHours(),
    page: page(body.path),
    source: src || 'internal',
    country: typeof country === 'string' && /^[A-Z]{2}$/.test(country) ? country : 'XX',
  };
  if (body.kind !== 'view') return [{ ...base, kind: body.kind }];
  // A view that arrives from outside the site starts a visit.
  return src ? [{ ...base, kind: 'view' }, { ...base, kind: 'visit' }] : [{ ...base, kind: 'view' }];
}

export const SCHEMA = `CREATE TABLE IF NOT EXISTS site_hits (
  day TEXT NOT NULL, hour INTEGER NOT NULL, kind TEXT NOT NULL, page TEXT NOT NULL, source TEXT NOT NULL, country TEXT NOT NULL,
  n INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (day, hour, kind, page, source, country)
)`;
let ready = false;

export async function onRequestPost({ request, env }) {
  const done = new Response(null, { status: 204, headers: { 'Cache-Control': 'no-store' } });
  const url = new URL(request.url);
  const origin = request.headers.get('Origin');
  if (origin && origin !== url.origin) return done;
  if (!env.USAGE) return done;
  let body;
  try {
    const text = await request.text();
    if (text.length > 2000) return done;
    body = JSON.parse(text);
  } catch (_) {
    return done;
  }
  const add = rows(body, { country: request.cf?.country, ua: request.headers.get('User-Agent') || '' });
  if (!add) return done;
  try {
    if (!ready) {
      await env.USAGE.prepare(SCHEMA).run();
      ready = true;
    }
    const sql = 'INSERT INTO site_hits (day, hour, kind, page, source, country, n) VALUES (?, ?, ?, ?, ?, ?, 1) ON CONFLICT (day, hour, kind, page, source, country) DO UPDATE SET n = n + 1';
    await env.USAGE.batch(add.map((r) => env.USAGE.prepare(sql).bind(r.day, r.hour, r.kind, r.page, r.source, r.country)));
  } catch (_) {
    // A lost hit is fine; the page never waits on it.
  }
  return done;
}

export function onRequest() {
  return new Response(null, { status: 405, headers: { Allow: 'POST' } });
}
