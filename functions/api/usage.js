// POST https://plonix.io/api/usage: one anonymous usage report from a Plonix install.
//
// Plonix sends at most one report a day (crates/plonix-core/src/usage.rs; what it holds is in docs/privacy.md).
// This keeps only the fields below, with the shapes below, and drops everything else. The client's IP address
// and headers are not stored. Reports are written to the D1 database bound as USAGE (schema in
// migrations/0001_usage.sql); without that binding the report is accepted and thrown away.

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
]);

const MAX_BYTES = 4096;
const MAX_COUNT = 1000000;
const done = () => new Response(null, { status: 204 });
const bad = () => new Response(null, { status: 400 });

const text = (v, re) => (typeof v === 'string' && re.test(v) ? v : null);

/** The report with only the expected fields, or null when it is not one. */
function clean(r) {
  if (!r || typeof r !== 'object' || Array.isArray(r) || r.schema !== 1) return null;
  const install = text(r.install_id, /^[0-9a-f]{32}$/);
  const version = text(r.version, /^[0-9A-Za-z.+-]{1,32}$/);
  if (!install || !version) return null;
  if (!r.counts || typeof r.counts !== 'object' || Array.isArray(r.counts)) return null;
  const counts = {};
  for (const [k, n] of Object.entries(r.counts)) {
    if (EVENTS.has(k) && Number.isInteger(n) && n > 0) counts[k] = Math.min(n, MAX_COUNT);
  }
  return {
    install,
    version,
    os: text(r.os, /^[a-z0-9_]{1,16}$/) || 'unknown',
    osVersion: text(r.os_version, /^[0-9A-Za-z.]{0,24}$/) || '',
    arch: text(r.arch, /^[a-z0-9_]{1,16}$/) || 'unknown',
    counts,
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
      'INSERT OR IGNORE INTO usage_reports (day, received_at, install_id, version, os, os_version, arch, counts) VALUES (?, ?, ?, ?, ?, ?, ?, ?)',
    )
      .bind(now.toISOString().slice(0, 10), Math.floor(now.getTime() / 1000), report.install, report.version, report.os, report.osVersion, report.arch, JSON.stringify(report.counts))
      .run();
  } catch (_) {
    // Never make Plonix retry or wait because storage failed.
  }
  return done();
}

export function onRequest() {
  return new Response(null, { status: 405, headers: { Allow: 'POST' } });
}
