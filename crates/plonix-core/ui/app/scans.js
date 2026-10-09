// Plonix window: Scans.
// Part of the project window's scripts (see index.html for the load order).
'use strict';

/* ======================================================================
   Scans
   ====================================================================== */

/**
 * Scans is target-first: pick an accepted host, Plonix fingerprints it and
 * suggests only the checks that fit, and the user chooses what to run. Every
 * request a scan or crawl sends goes through the same scope gate as the rest
 * of Plonix, so nothing ever leaves the hosts you accepted. Any issue a scan
 * records is a normal finding, editable on the Findings screen.
 */
const SC = { host: null, hosts: [], suggest: null, suggestErr: null, picks: null, intrusive: false, running: false, report: null, focus: null, eps: null, epsHost: null, crawl: null, crawling: false, crawlStart: '/', crawlBrowser: false, crawlClick: false };

/**
 * Hands one endpoint to Scans, focused: the scan is narrowed to this endpoint
 * (the engine aims injecting checks only here) and the checks that fit the
 * lead are pre-picked. Nothing runs until the researcher presses Run scan.
 */
function scanEndpoint(ex, lead) {
  SC.host = ex.host;
  SC.focus = {
    mode: 'path',
    method: ex.method,
    path: ex.path,
    label: lead ? lead.focus : '',
    // An array of OWASP ids pre-picks the checks that match (an empty array
    // pre-picks none); null, for a plain "scan this endpoint", pre-picks every
    // recommended check, aimed at this endpoint.
    categories: lead ? lead.categories || [] : null,
    title: lead ? lead.title : `${ex.method} ${ex.path}`,
    note: lead ? lead.note : '',
  };
  SC.suggest = null;
  SC.report = null;
  SC.crawl = null;
  SC.running = false;
  leaveTo('scans');
}

const INTRU_LABEL = { passive: 'Passive', safe: 'Safe', active: 'Active', intrusive: 'Intrusive' };
const INTRU_TAG = { passive: 'in', safe: 'in', active: 'upd', intrusive: 'rej' };

function renderScans(main) {
  clear(
    main,
    h(
      'div',
      { class: 'view' },
      h(
        'div',
        { class: 'toolbar' },
        h('h2', { text: 'Scans' }),
        h('span', { class: 'hint', text: 'Pick an in-scope target. Plonix suggests checks from what it fingerprinted and runs only the ones you choose. Every request stays inside your scope.' }),
      ),
      h('div', { id: 'scanprog' }),
      h('div', { class: 'pane' }, h('div', { class: 'stack', id: 'scanbody' }, h('div', { class: 'muted', text: 'Loading…' }))),
    ),
  );
  loadScans();
  showProgramLock('scanprog', 'Scans and crawls are off in this project');
}

/** A note at the top of a screen when the program the project follows bans automated testing. */
async function showProgramLock(id, what) {
  let p;
  try {
    p = (await api('/api/program')).program;
  } catch (_) {
    return;
  }
  const box = document.getElementById(id);
  if (!box || !p || !p.rules.no_automation) return;
  clear(
    box,
    h(
      'div',
      { class: 'proglock' },
      h('span', { text: `${p.name} does not allow automated testing. ${what}; browsing and single Bench requests still work.` }),
      toolOn('programs') ? h('button', { class: 'btn sm', text: 'Program rules', onclick: () => leaveTo('programs') }) : null,
    ),
  );
}

async function loadScans() {
  let hosts;
  try {
    hosts = await api('/api/hosts');
  } catch (e) {
    return toast(e.message, 'err');
  }
  if (S.view !== 'scans') return;
  SC.hosts = hosts.filter((x) => x.scope === 'accepted');
  if (!SC.host || !SC.hosts.some((x) => x.host === SC.host)) {
    SC.host = SC.hosts.length ? SC.hosts.slice().sort((a, b) => b.requests - a.requests)[0].host : null;
    SC.suggest = null;
    SC.report = null;
    SC.crawl = null;
  }
  drawScans();
  if (SC.host && !SC.suggest) loadSuggest();
}

async function loadSuggest() {
  const host = SC.host;
  if (!host) return;
  SC.suggest = null;
  SC.suggestErr = null;
  drawScans();
  let sug;
  try {
    sug = await api('/api/scan/suggest/' + encodeURIComponent(host));
  } catch (e) {
    if (SC.host === host) SC.suggestErr = e.message;
    if (S.view === 'scans') drawScans();
    return;
  }
  if (SC.host !== host || S.view !== 'scans') return;
  SC.suggest = sug;
  // Recommended checks start selected; intrusive ones stay off until opted in.
  // Under a focus, `categories` is an array of OWASP ids to pre-pick (empty
  // picks none — e.g. a shape Plonix has no built-in check for yet), or null
  // for "every recommended check, aimed at this endpoint".
  const cats = SC.focus ? SC.focus.categories : null;
  const fits = (t) => (t.owasp || []).some((o) => (cats || []).some((c) => o === c || o.startsWith(c)));
  // A null/absent `categories` (whole host, group, or a plain endpoint focus)
  // pre-picks every recommended check; an array narrows to the matching ones.
  const chosen = !SC.focus || cats == null ? sug.recommended : sug.recommended.filter(fits);
  SC.picks = new Set(chosen.map((t) => t.id));
  SC.intrusive = false;
  drawScans();
}

function drawScans() {
  const box = $('#scanbody');
  if (!box) return;
  if (!SC.hosts.length) {
    return clear(
      box,
      h(
        'div',
        { class: 'card' },
        h(
          'div',
          { class: 'empty' },
          h('h3', { text: 'No in-scope target yet' }),
          'Scans and crawls only run against hosts you have accepted into scope. Open a target and accept it in ',
          h('button', { class: 'btn sm', text: 'Scope', onclick: () => go('scope') }),
          ' first, then come back here.',
        ),
      ),
    );
  }
  // A path or path-group scope needs the host's discovered endpoints; load
  // them lazily the first time the Scans screen draws under such a scope.
  if (SC.focus && SC.focus.mode !== 'host' && (!SC.eps || SC.epsHost !== SC.host)) ensureScanEndpoints();
  const sel = h(
    'select',
    {
      onchange: () => {
        SC.host = sel.value;
        SC.suggest = null;
        SC.report = null;
        SC.crawl = null;
        SC.focus = null;
        loadSuggest();
      },
    },
    SC.hosts.map((x) => h('option', { value: x.host, text: `${x.host}  (${x.requests} req)`, selected: x.host === SC.host })),
  );
  const targetCard = h(
    'div',
    { class: 'card' },
    h('div', { class: 'sechead' }, h('h3', { text: 'Target' }), h('span', { class: 'shacts' }, askButton({ kind: 'host', host: SC.host }, 'Ask Claude Code to plan a scan for this host'))),
    h('div', { class: 'scanrow' }, h('label', { class: 'muted', text: 'Host' }), sel),
    scanScopeRow(),
    scanScopeNote(),
  );
  clear(box, targetCard, scanSuggestSection(), scanCrawlSection());
}

/** The current scan scope: 'host', 'path' or 'group'. */
function scanScopeMode() {
  return SC.focus ? SC.focus.mode : 'host';
}

/** Make sure the host's discovered endpoints are loaded for the path pickers. */
async function ensureScanEndpoints() {
  if (SC.epsHost === SC.host && SC.eps) return;
  SC.epsHost = SC.host;
  SC.eps = null;
  try {
    SC.eps = await api('/api/hosts/' + encodeURIComponent(SC.host) + '/endpoints');
  } catch (_) {
    SC.eps = [];
  }
  if (S.view === 'scans') drawScans();
}

/** Path prefixes offered for a path-group scan: the distinct first path
 *  segment of the discovered endpoints (e.g. `/api/`, `/admin/`). */
function scanGroupPrefixes() {
  const seen = new Map();
  for (const e of SC.eps || []) {
    const m = (e.path || '').match(/^\/[^/]+\//);
    const pre = m ? m[0] : '/';
    seen.set(pre, (seen.get(pre) || 0) + 1);
  }
  return [...seen.entries()].sort((a, b) => b[1] - a[1]).map(([prefix, count]) => ({ prefix, count }));
}

/** Endpoints under the current path-group prefix. */
function scanGroupEndpoints() {
  const pre = SC.focus && SC.focus.prefix;
  if (!pre) return [];
  return (SC.eps || []).filter((e) => (e.path || '').startsWith(pre));
}

/** Switch the scan scope. Loads endpoints lazily for the path pickers. */
function setScanScope(mode) {
  if (mode === 'host') {
    SC.focus = null;
    if (SC.suggest) {
      SC.picks = new Set(SC.suggest.recommended.map((t) => t.id));
      SC.intrusive = false;
    }
    drawScans();
    return;
  }
  const needEps = !SC.eps || SC.epsHost !== SC.host;
  const go = () => {
    const eps = SC.eps || [];
    if (mode === 'path') {
      const e = eps[0];
      SC.focus = { mode: 'path', method: e ? e.method : 'GET', path: e ? e.path : '/', label: '', categories: null, title: '', note: '' };
    } else {
      const pres = scanGroupPrefixes();
      SC.focus = { mode: 'group', prefix: pres.length ? pres[0].prefix : '/', title: '' };
    }
    drawScans();
  };
  if (needEps) ensureScanEndpoints().then(go);
  else go();
}

/** The scope selector row: whole host, a single path, or a path group. */
function scanScopeRow() {
  const mode = scanScopeMode();
  const seg = (m, label) => h('button', { class: 'segbtn' + (mode === m ? ' on' : ''), text: label, onclick: () => setScanScope(m) });
  const row = h(
    'div',
    { class: 'scanrow' },
    h('label', { class: 'muted', text: 'Scope' }),
    h('div', { class: 'seg scanscope' }, seg('host', 'Whole host'), seg('path', 'A path'), seg('group', 'A path group')),
  );
  if (mode === 'path') row.append(scanPathPicker());
  if (mode === 'group') row.append(scanGroupPicker());
  return row;
}

/** A select of the host's discovered endpoints, for a single-path scan.
 *  The currently focused endpoint is always an option, even if it came from a
 *  lead and is not in the discovered list yet. */
function scanPathPicker() {
  if (!SC.eps) return h('span', { class: 'muted', text: 'Loading paths…' });
  const key = (e) => e.method + ' ' + e.path;
  const cur = SC.focus ? key(SC.focus) : '';
  const opts = (SC.eps || []).slice();
  if (SC.focus && !opts.some((e) => key(e) === cur)) opts.unshift({ method: SC.focus.method, path: SC.focus.path });
  if (!opts.length) return h('span', { class: 'muted', text: 'No endpoints discovered yet — browse or crawl the host first.' });
  const sel = h(
    'select',
    {
      onchange: () => {
        const e = opts.find((x) => key(x) === sel.value);
        if (e) SC.focus = { mode: 'path', method: e.method, path: e.path, label: '', categories: null, title: '', note: '' };
        drawScans();
      },
    },
    opts.map((e) => h('option', { value: key(e), text: key(e), selected: key(e) === cur })),
  );
  return sel;
}

/** A select of path prefixes, for scanning a group of related endpoints. */
function scanGroupPicker() {
  if (!SC.eps) return h('span', { class: 'muted', text: 'Loading paths…' });
  const pres = scanGroupPrefixes();
  if (!pres.length) return h('span', { class: 'muted', text: 'No endpoints discovered yet — browse or crawl the host first.' });
  const cur = SC.focus ? SC.focus.prefix : '';
  const sel = h(
    'select',
    {
      onchange: () => {
        SC.focus = { mode: 'group', prefix: sel.value, title: '' };
        drawScans();
      },
    },
    pres.map((p) => h('option', { value: p.prefix, text: `${p.prefix}  (${p.count})`, selected: p.prefix === cur })),
  );
  const n = scanGroupEndpoints().length;
  return h('span', { class: 'scangrp' }, sel, h('span', { class: 'muted', text: `${n} endpoint${n === 1 ? '' : 's'}` }));
}

/** One line describing what the current scope and any lead mean. */
function scanScopeNote() {
  const f = SC.focus;
  if (!f) return h('p', { class: 'muted', text: 'The scan covers the whole host. Pick a path or a path group to narrow it. Only accepted, in-scope hosts appear here.' });
  if (f.mode === 'group') {
    return h('p', { class: 'muted', text: `Scanning every discovered endpoint under ${f.prefix}. Injecting checks are aimed only there; host-level file probes still cover the host.` });
  }
  return h('p', { class: 'muted focusnote', text: f.note || `The scan is aimed at ${f.method} ${f.path}. Injecting checks hit only this endpoint; host-level file probes still cover the host.` });
}

function scanSuggestSection() {
  if (SC.suggestErr) return h('div', { class: 'card' }, h('div', { class: 'rerr', text: SC.suggestErr }));
  if (!SC.suggest) return h('div', { class: 'card' }, h('div', { class: 'muted', text: 'Fingerprinting ' + SC.host + '…' }));
  const sug = SC.suggest;
  const wrap = h('div', { class: 'scansug' });

  wrap.append(
    h(
      'div',
      { class: 'card' },
      h('div', { class: 'sechead' }, h('h3', { text: `What Plonix sees (${sug.signals.length})` })),
      sug.signals.length
        ? h('div', { class: 'stack scansigs' }, sug.signals.map(scanSignalRow))
        : h('div', { class: 'empty', text: 'No technology signals yet. Browse the target a little more, or crawl it below, so Plonix has traffic to fingerprint.' }),
    ),
  );

  const picks = SC.picks || new Set();
  const runBtn = h('button', { class: 'btn primary', disabled: SC.running || !picks.size, onclick: runScan, title: actingUser() ? `Sent as ${actingUser().name}, the user you act as` : null }, SC.running ? 'Scanning…' : actingUser() ? `Run scan as ${actingUser().name}` : 'Run scan');
  const groups = [];
  if (sug.recommended.length) {
    groups.push(
      h(
        'div',
        { class: 'scangroup' },
        h('div', { class: 'scanglabel' }, h('b', { text: 'Recommended' }), h('span', { class: 'muted', text: ' fit this target, non-intrusive' })),
        sug.recommended.map((t) => scanTacticRow(t, false)),
      ),
    );
  }
  if (sug.optional.length) {
    groups.push(
      h(
        'div',
        { class: 'scangroup' },
        h(
          'div',
          { class: 'scanglabel' },
          h('label', { class: 'intrutoggle' }, h('input', { type: 'checkbox', checked: SC.intrusive, onchange: (e) => toggleIntrusive(e.target.checked) }), h('b', { text: ' Intrusive checks' })),
          h('span', { class: 'muted', text: ' off by default; may be heavier or state-touching' }),
        ),
        sug.optional.map((t) => scanTacticRow(t, true)),
      ),
    );
  }
  const checksCard = h(
    'div',
    { class: 'card' },
    h(
      'div',
      { class: 'sechead' },
      h('h3', { text: 'Checks' }),
      groups.length
        ? h(
            'span',
            { class: 'shacts' },
            h('button', {
              class: 'btn sm',
              text: 'Select recommended',
              onclick: () => {
                SC.picks = new Set(sug.recommended.map((t) => t.id));
                SC.intrusive = false;
                drawScans();
              },
            }),
            h('button', { class: 'btn sm', text: 'Clear', onclick: () => ((SC.picks = new Set()), drawScans()) }),
          )
        : null,
    ),
    groups.length
      ? groups
      : h('div', { class: 'empty', text: 'No checks apply to this target yet. A check unlocks only when a matching technology is detected, so browse or crawl the target first.' }),
    sug.skipped ? h('p', { class: 'muted scanskip', text: `${sug.skipped} check${sug.skipped === 1 ? '' : 's'} in the catalog did not match this target's fingerprint.` }) : null,
    h('div', { class: 'scanrunbar' }, h('span', { class: 'muted', text: `${picks.size} selected` }), runBtn),
  );
  wrap.append(checksCard);
  if (SC.report) wrap.append(scanReportCard());
  return wrap;
}

function scanSignalRow(s) {
  return h(
    'div',
    { class: 'scansig' },
    h('span', { class: 'techchip', text: s.signal }),
    h('span', { class: 'muted scansigev', text: s.evidence }),
    s.exchange_id != null ? h('button', { class: 'linklike', text: 'request #' + s.exchange_id, title: 'Open in the Lens', onclick: () => showExchange(s.exchange_id) }) : null,
  );
}

function scanTacticRow(t, intrusive) {
  const picks = SC.picks || new Set();
  const cb = h('input', { type: 'checkbox', checked: picks.has(t.id), disabled: intrusive && !SC.intrusive, onchange: () => togglePick(t.id, cb.checked) });
  return h(
    'label',
    { class: 'scantactic' + (intrusive ? ' intru' : '') },
    cb,
    h('span', { class: 'sev ' + t.severity, text: t.severity }),
    h('span', { class: 'tt', text: t.title }),
    t.variant ? h('span', { class: 'tag out', text: t.variant }) : null,
    h('span', { class: 'tag ' + (INTRU_TAG[t.intrusiveness] || 'out'), text: INTRU_LABEL[t.intrusiveness] || t.intrusiveness }),
    t.requires && t.requires.length ? h('span', { class: 'muted scanreq', text: 'needs ' + t.requires.join(', ') }) : null,
  );
}

function togglePick(id, on) {
  if (!SC.picks) SC.picks = new Set();
  if (on) SC.picks.add(id);
  else SC.picks.delete(id);
  drawScans();
}

function toggleIntrusive(on) {
  SC.intrusive = on;
  if (!on && SC.suggest && SC.picks) for (const t of SC.suggest.optional) SC.picks.delete(t.id);
  drawScans();
}

async function runScan() {
  if (!SC.host || !SC.picks || !SC.picks.size) return;
  const tactics = [...SC.picks];
  SC.running = true;
  SC.report = null;
  drawScans();
  const body = { host: SC.host, tactics, include_intrusive: SC.intrusive };
  if (toolOn('saved-users') && actingUser()) body.as_user = S.acting;
  if (SC.focus && SC.focus.mode === 'path') {
    body.endpoints = [{ method: SC.focus.method, path: SC.focus.path }];
  } else if (SC.focus && SC.focus.mode === 'group') {
    const eps = scanGroupEndpoints().map((e) => ({ method: e.method, path: e.path }));
    if (eps.length) body.endpoints = eps;
  }
  try {
    SC.report = await api('/api/scan', { method: 'POST', body });
  } catch (e) {
    SC.running = false;
    drawScans();
    return toast(e.code === 'out_of_scope' ? SC.host + ' is not in scope. Accept it in Scope first.' : e.message, 'err');
  }
  SC.running = false;
  const n = SC.report.findings.length;
  toast(n ? `Scan recorded ${n} finding${n === 1 ? '' : 's'}` : 'Scan finished — nothing to report', n ? 'ok' : '');
  refreshFindingsCount();
  drawScans();
}

function scanReportCard() {
  const r = SC.report;
  const findings = r.findings || [];
  return h(
    'div',
    { class: 'card scanreport' },
    h('div', { class: 'sechead' }, h('h3', { text: 'Last scan' }), h('span', { class: 'muted', text: `${r.tactics_run.length} check${r.tactics_run.length === 1 ? '' : 's'} run · ${r.requests_sent} request${r.requests_sent === 1 ? '' : 's'} sent` })),
    findings.length
      ? h(
          'div',
          { class: 'stack' },
          findings.map((f) => h('div', { class: 'scanfinding' }, h('span', { class: 'sev ' + f.severity, text: f.severity }), h('span', { class: 'tt', text: f.title }), h('span', { class: 'fmeta', text: '#' + f.id }))),
          h('button', { class: 'btn sm', text: 'View in Findings', onclick: () => go('findings') }),
        )
      : h('div', { class: 'empty', text: 'No issues found. The requests the scan sent are in Traffic.' }),
    r.notes && r.notes.length ? h('div', { class: 'scannotes' }, r.notes.map((n) => h('p', { class: 'muted', text: n }))) : null,
  );
}

function scanCrawlSection() {
  const start = h('input', { value: SC.crawlStart, spellcheck: 'false', placeholder: '/', oninput: (e) => (SC.crawlStart = e.target.value) });
  const browser = h('input', { type: 'checkbox', checked: SC.crawlBrowser, disabled: SC.crawling, onchange: (e) => ((SC.crawlBrowser = e.target.checked), drawScans()) });
  const click = h('input', { type: 'checkbox', checked: SC.crawlClick, disabled: SC.crawling, onchange: (e) => (SC.crawlClick = e.target.checked) });
  const run = h('button', { class: 'btn', disabled: SC.crawling, text: SC.crawling ? 'Crawling…' : 'Crawl', onclick: () => runCrawl() });
  return h(
    'div',
    { class: 'card' },
    h('div', { class: 'sechead' }, h('h3', { text: 'Crawl' }), h('span', { class: 'muted', text: 'Walk the in-scope site to discover endpoints and forms. Bounded and read-only.' })),
    h(
      'div',
      { class: 'scanrow' },
      h('label', { class: 'muted', text: 'Start path' }),
      start,
      h('label', { class: 'cbrowser' }, browser, 'Use a browser (for JavaScript apps)'),
      SC.crawlBrowser ? h('label', { class: 'cbrowser', title: 'Buttons inside forms, and anything labelled like log out, delete, remove or pay, are never clicked' }, click, 'Click buttons too') : null,
      run,
    ),
    SC.crawlBrowser
      ? h('p', { class: 'muted cbnote', text: 'Pages render in a headless Chrome-family browser routed through Plonix, so every request lands in Traffic. Requests to hosts outside scope are blocked, and forms are never submitted. Takes up to 3 minutes.' })
      : null,
    SC.crawl ? crawlReportCard() : null,
  );
}

async function runCrawl() {
  if (!SC.host) return;
  SC.crawling = true;
  SC.crawl = null;
  drawScans();
  try {
    const browser = !!SC.crawlBrowser;
    SC.crawl = await api('/api/crawl', { method: 'POST', body: { host: SC.host, start: SC.crawlStart || '/', browser, click: browser && !!SC.crawlClick } });
  } catch (e) {
    SC.crawling = false;
    drawScans();
    return toast(e.code === 'out_of_scope' ? SC.host + ' is not in scope.' : e.message, 'err');
  }
  SC.crawling = false;
  drawScans();
  // Fresh traffic can sharpen the fingerprint, so re-suggest.
  loadSuggest();
}

function crawlReportCard() {
  const r = SC.crawl;
  const n = (k, word) => `${k} ${word}${k === 1 ? '' : 's'}`;
  const counts = [n(r.pages_fetched, 'page'), n(r.urls_found, 'URL'), n(r.forms.length, 'form')];
  if (r.clicks) counts.push(n(r.clicks, 'click'));
  if (r.browser) counts.push('rendered in ' + r.browser);
  const blocked = r.blocked_hosts || [];
  const host = r.host || SC.host;
  // A form action may be a path on the crawled host or an absolute URL to
  // another host; show the host so the row is never ambiguous.
  const actionCell = (action) => {
    const a = action || '/';
    if (/^https?:\/\//i.test(a)) return h('td', null, h('code', { text: a }));
    return h('td', null, h('span', { class: 'formhost', text: host }), h('code', { text: a }));
  };
  return h(
    'div',
    { class: 'crawlreport' },
    h('div', { class: 'sechead' }, h('h4', null, 'Crawl result · ', h('code', { text: host })), h('span', { class: 'muted', text: counts.join(' · ') }), h('span', { class: 'shacts' }, h('button', { class: 'btn sm', text: 'View on Map', onclick: () => go('map') }))),
    blocked.length
      ? h('p', { class: 'muted crawlblocked' }, 'Blocked, not in scope: ', blocked.map((b) => h('code', { text: b })))
      : null,
    r.forms.length
      ? h(
          'table',
          { class: 'grid scanforms' },
          h('thead', null, h('tr', null, h('th', { text: 'Method' }), h('th', { text: 'Action' }), h('th', { text: 'Fields' }))),
          h('tbody', null, r.forms.slice(0, 50).map((f) => h('tr', null, h('td', { text: f.method }), actionCell(f.action), h('td', { text: f.fields.join(', ') || '—' })))),
        )
      : null,
    r.notes && r.notes.length ? h('div', { class: 'scannotes' }, r.notes.map((n) => h('p', { class: 'muted', text: n }))) : null,
  );
}

async function refreshFindingsCount() {
  try {
    const l = await api('/api/findings');
    S.findingsCount = l.length;
    updateChrome();
  } catch (_) {}
}
