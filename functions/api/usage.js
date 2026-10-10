// POST https://plonix.io/api/usage: one anonymous usage report from a Plonix install.
//
// Plonix sends at most one report a day (crates/plonix-core/src/usage.rs; what it holds is in docs/privacy.md).
// This keeps only the fields below, with the shapes below, and drops everything else: names must come from the
// fixed lists here, numbers must be whole and capped. The client's IP address and headers are not stored.
// Reports are written to the D1 database bound as USAGE (schema in migrations/); without that binding the report
// is accepted and thrown away. The public totals are served by functions/api/stats.js.

// Feature names Plonix counts. Keep in step with EVENTS in crates/plonix-core/src/usage.rs.
const EVENTS = new Set([
  'app_launched',
  'web_launcher_opened',
  'cli_used',
  'project_opened',
  'demo_opened',
  'capture_started',
  'intercept_used',
  'bench_send',
  'bench_run',
  'scan_run',
  'program_applied',
  'crawl_run',
  'finding_added',
  'report_exported',
  'market_install',
  'ask_claude',
  'agent_launch',
  'mcp_session',
  'callbacks_start',
  'access_check',
  'screen_traffic',
  'screen_bench',
  'screen_scope',
  'screen_map',
  'screen_findings',
  'screen_agents',
  'screen_market',
  'screen_scans',
  'screen_programs',
  'screen_settings',
  'screen_users',
  'screen_access',
  'screen_callbacks',
  'screen_rules',
]);

// Screens whose active minutes are counted. Keep in step with SCREENS in usage.rs.
const SCREENS = new Set(['traffic', 'bench', 'scope', 'map', 'users', 'access', 'callbacks', 'findings', 'agents', 'market', 'scans', 'programs', 'rules', 'settings']);

// Kinds of Traffic search terms (FILTER_KINDS); a leading '-' is allowed.
const FILTER_KINDS = new Set(['host', 'method', 'status', 'path', 'mime', 'scope', 'source', 'ext', 'kind', 'is', 'text']);

// Size ranges (RANGES). Exact numbers are never sent.
const RANGES = new Set(['0', '1', '2-5', '6-20', '21-100', '101-1k', '1k-10k', '10k-100k', '100k+']);

// The kinds of work picked on first launch (store/profiles.json).
const PROFILES = new Set(['bug-hunter', 'red-teamer', 'researcher', 'none']);

// The only domains a report may name: Plonix's built-in exclusion groups and scope noise list (known_domains in
// usage.rs). Every other rejected host arrives as 'other'.
const DOMAINS = new Set([
  'accounts.google.com', 'adnxs.com', 'adservice.google.com', 'adsrvr.org', 'adyen.com',
  'ajax.googleapis.com', 'akamaihd.net', 'amplitude.com', 'analytics.google.com', 'apis.google.com',
  'auth0.com', 'bootstrapcdn.com', 'braintreegateway.com', 'browser-intake-datadoghq.com', 'bugsnag.com',
  'cdn.jsdelivr.net', 'cdnjs.cloudflare.com', 'checkout.com', 'clients2.google.com', 'cloudfront.net',
  'connect.facebook.net', 'criteo.com', 'datadoghq.com', 'doubleclick.net', 'drift.com', 'duosecurity.com',
  'facebook.net', 'fonts.googleapis.com', 'fonts.gstatic.com', 'fullstory.com', 'google-analytics.com',
  'googleadservices.com', 'googlesyndication.com', 'googletagmanager.com', 'gstatic.com', 'heap.io',
  'honeycomb.io', 'hotjar.com', 'hs-scripts.com', 'hubspot.com', 'instagram.com', 'intercom.io',
  'intercomcdn.com', 'js.stripe.com', 'jsdelivr.net', 'klarna.com', 'login.microsoftonline.com',
  'logrocket.com', 'matomo.cloud', 'mixpanel.com', 'mouseflow.com', 'mozilla.net', 'mozilla.org',
  'newrelic.com', 'nr-data.net', 'okta.com', 'onelogin.com', 'optimizationguide-pa.googleapis.com',
  'outbrain.com', 'paypal.com', 'paypalobjects.com', 'pingidentity.com', 'platform.linkedin.com',
  'platform.twitter.com', 'plausible.io', 'pubmatic.com', 'raygun.io', 'retool.com', 'rollbar.com',
  'rubiconproject.com', 'safebrowsing.googleapis.com', 'schema.org', 'scorecardresearch.com', 'segment.com',
  'segment.io', 'sentry.io', 'squareup.com', 'statcounter.com', 'stripe.com', 'taboola.com', 'tryretool.com',
  'unpkg.com', 'update.googleapis.com', 'w3.org', 'youtube.com', 'ytimg.com', 'zdassets.com', 'zendesk.com',
  'other',
]);

const LOOK = { style: ['studio', 'classic'], theme: ['auto', 'light', 'dark'], density: ['dense', 'roomy'] };

const MAX_BYTES = 8192;
const MAX_COUNT = 1000000;
const done = () => new Response(null, { status: 204 });
const bad = () => new Response(null, { status: 400 });

const text = (v, re) => (typeof v === 'string' && re.test(v) ? v : null);

const isObject = (v) => !!v && typeof v === 'object' && !Array.isArray(v);

/** { name: count } with only allowed names and whole, capped counts. */
function tally(v, allowed, max = MAX_COUNT) {
  const out = {};
  if (!isObject(v)) return out;
  for (const [k, n] of Object.entries(v)) {
    if (allowed(k) && Number.isInteger(n) && n > 0) out[k] = Math.min(n, max);
  }
  return out;
}

/** The schema 2 additions, cleaned the same way. */
function extra(r) {
  const look = {};
  if (isObject(r.look)) for (const [k, allowed] of Object.entries(LOOK)) if (allowed.includes(r.look[k])) look[k] = r.look[k];
  const sizes = {};
  if (isObject(r.sizes)) for (const k of ['requests', 'hosts', 'in_scope']) sizes[k] = tally(r.sizes[k], (b) => RANGES.has(b), 1000);
  return {
    minutes: tally(r.minutes, (s) => SCREENS.has(s), 24 * 60),
    filters: tally(r.filters, (f) => FILTER_KINDS.has(f.replace(/^-/, ''))),
    profile: PROFILES.has(r.profile) ? r.profile : 'none',
    look,
    projects: RANGES.has(r.projects) ? r.projects : null,
    sizes,
    rejected: tally(r.rejected, (d) => DOMAINS.has(d), 1000),
  };
}

/** The report with only the expected fields, or null when it is not one. */
export function clean(r) {
  if (!isObject(r) || (r.schema !== 1 && r.schema !== 2)) return null;
  const install = text(r.install_id, /^[0-9a-f]{32}$/);
  const version = text(r.version, /^[0-9A-Za-z.+-]{1,32}$/);
  if (!install || !version) return null;
  if (!isObject(r.counts)) return null;
  const counts = tally(r.counts, (k) => EVENTS.has(k));
  return {
    install,
    version,
    os: text(r.os, /^[a-z0-9_]{1,16}$/) || 'unknown',
    // Major and minor release only.
    osVersion: (text(r.os_version, /^[0-9A-Za-z.]{0,24}$/) || '').split('.').slice(0, 2).join('.'),
    arch: text(r.arch, /^[a-z0-9_]{1,16}$/) || 'unknown',
    counts,
    extra: r.schema === 2 ? extra(r) : null,
  };
}

export async function onRequestPost({ request, env }) {
  const declared = Number(request.headers.get('content-length') || 0);
  if (declared > MAX_BYTES) return new Response(null, { status: 413 });
  let report;
  try {
    const body = await request.text();
    if (body.length > MAX_BYTES) return new Response(null, { status: 413 });
    report = clean(JSON.parse(body));
  } catch (_) {
    return bad();
  }
  if (!report) return bad();
  if (!env.USAGE) return done();
  const now = new Date();
  try {
    // One report per install per day; a second one the same day is ignored.
    await env.USAGE.prepare(
      'INSERT OR IGNORE INTO usage_reports (day, received_at, install_id, version, os, os_version, arch, counts, extra) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)',
    )
      .bind(
        now.toISOString().slice(0, 10),
        Math.floor(now.getTime() / 1000),
        report.install,
        report.version,
        report.os,
        report.osVersion,
        report.arch,
        JSON.stringify(report.counts),
        report.extra ? JSON.stringify(report.extra) : null,
      )
      .run();
  } catch (_) {
    // Never make Plonix retry or wait because storage failed.
  }
  return done();
}

export function onRequest() {
  return new Response(null, { status: 405, headers: { Allow: 'POST' } });
}
