// Plonix web UI. Plain JavaScript, no build step: every screen reads and
// writes through the engine's local API, exactly like the CLI.
//
// Captured traffic is attacker-controlled, so nothing from the API is ever
// parsed as HTML: the DOM is built with h() and text nodes only.
'use strict';

/* ---------- DOM helpers ---------- */

function h(tag, props, ...kids) {
  const el = document.createElement(tag);
  if (props) {
    for (const [k, v] of Object.entries(props)) {
      if (v == null || v === false) continue;
      if (k === 'class') el.className = v;
      else if (k === 'text') el.textContent = v;
      else if (k === 'value' || k === 'checked' || k === 'disabled' || k === 'selected') el[k] = v;
      else if (k === 'style') Object.assign(el.style, v);
      else if (k.startsWith('on') && typeof v === 'function') el.addEventListener(k.slice(2), v);
      else el.setAttribute(k, v === true ? '' : String(v));
    }
  }
  append(el, kids);
  return el;
}

function append(el, kids) {
  for (const c of kids.flat(Infinity)) {
    if (c == null || c === false) continue;
    el.append(c instanceof Node ? c : String(c));
  }
  return el;
}

function clear(el, ...kids) {
  el.replaceChildren();
  return append(el, kids);
}

const $ = (sel, root = document) => root.querySelector(sel);

/* ---------- formatting ---------- */

const fmtTime = (ms) => new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit', hour12: false });
const fmtDate = (ms) => new Date(ms).toLocaleString([], { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' });
function fmtSize(n) {
  if (n == null || n < 0) return '';
  if (n < 1024) return n + ' B';
  if (n < 1024 * 1024) return (n / 1024).toFixed(n < 10240 ? 1 : 0) + ' KB';
  return (n / 1048576).toFixed(1) + ' MB';
}
const b64len = (s) => (s ? Math.floor((s.length * 3) / 4) - (s.endsWith('==') ? 2 : s.endsWith('=') ? 1 : 0) : 0);
const statusClass = (s) => (s == null ? 'sc sc-x' : 'sc sc-' + String(s)[0]);
const target = (ex) => ex.path + (ex.query ? '?' + ex.query : '');
const shortMime = (m) => (m || '').replace(/^(application|text)\//, '').replace(/^x-/, '');
const header = (headers, name) => {
  const hit = (headers || []).find(([k]) => k.toLowerCase() === name);
  return hit ? hit[1] : null;
};

function store(key, value) {
  try {
    if (value === undefined) return JSON.parse(localStorage.getItem(key));
    if (value === null) localStorage.removeItem(key);
    else localStorage.setItem(key, JSON.stringify(value));
  } catch (_) {
    return null;
  }
}

/* ---------- API ---------- */

class ApiError extends Error {
  constructor(status, code, message) {
    super(message);
    this.status = status;
    this.code = code;
  }
}

const S = {
  token: null,
  railCollapsed: store('plonix.rail') === 'collapsed',
  status: null,
  engineUp: true,
  view: 'traffic',
  scope: { rules: [], suggestions: [] },
};

async function api(path, { method = 'GET', body } = {}) {
  let resp;
  try {
    resp = await fetch(path, {
      method,
      headers: {
        Authorization: 'Bearer ' + S.token,
        'X-Plonix-Client': 'gui',
        ...(body !== undefined ? { 'Content-Type': 'application/json' } : {}),
      },
      body: body !== undefined ? JSON.stringify(body) : undefined,
      cache: 'no-store',
    });
  } catch (e) {
    throw new ApiError(0, 'engine_down', 'The Plonix engine is not reachable. Start it with `plonix ui`.');
  }
  let data = null;
  try {
    data = await resp.json();
  } catch (_) {}
  if (resp.status === 401 && path.startsWith('/api/')) {
    signOut();
    throw new ApiError(401, 'unauthorized', 'Signed out');
  }
  if (!resp.ok) throw new ApiError(resp.status, (data && data.code) || 'error', (data && data.error) || resp.statusText);
  return data;
}

/* ---------- toasts & modal ---------- */

function toast(msg, kind = '') {
  let box = $('.toasts');
  if (!box) document.body.append((box = h('div', { class: 'toasts' })));
  const t = h('div', { class: 'toast ' + kind, text: msg });
  box.append(t);
  setTimeout(() => t.remove(), kind === 'err' ? 6000 : 3200);
}

function closeModal() {
  const m = $('.modal');
  if (m) m.remove();
}

function modal(title, body, actions) {
  closeModal();
  const err = h('span', { class: 'err' });
  const m = h(
    'div',
    { class: 'modal', onmousedown: (e) => e.target === m && closeModal() },
    h('div', { class: 'mcard', role: 'dialog' }, h('h3', { text: title }), h('div', { class: 'mb' }, body), h('div', { class: 'mf' }, err, actions)),
  );
  document.body.append(m);
  const first = m.querySelector('input, textarea, select');
  if (first) first.focus();
  return { el: m, err };
}

/* ---------- session ---------- */

async function boot() {
  applyTheme(store('plonix.theme') || 'auto');
  const hash = location.hash;
  const code = hash.startsWith('#code=') ? hash.slice(6) : null;
  if (code) {
    history.replaceState(null, '', location.pathname + '#/traffic');
    try {
      const r = await fetch('/ui/session', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ code }),
      });
      const data = await r.json().catch(() => ({}));
      if (r.ok && data.token) {
        store('plonix.token', data.token);
      } else if (!store('plonix.token')) {
        return showLock(data.error);
      }
    } catch (_) {
      return showLock('The Plonix engine is not reachable.');
    }
  }
  S.token = store('plonix.token');
  if (!S.token) return showLock();
  try {
    S.status = await api('/api/status');
  } catch (e) {
    if (e.code === 'unauthorized') return;
    return showLock(e.message);
  }
  const v = (location.hash.match(/^#\/(\w+)/) || [])[1];
  if (VIEWS[v]) S.view = v;
  renderShell();
  poll();
}

function signOut(message) {
  store('plonix.token', null);
  S.token = null;
  showLock(message || 'This session has ended.');
}

function showLock(message) {
  clearTimeout(S.pollTimer);
  closeModal();
  clear(
    $('#app'),
    h(
      'div',
      { class: 'lock' },
      h(
        'div',
        { class: 'mcard' },
        h('img', { src: '/ui/icon.svg', alt: '' }),
        h('h1', { text: IN_APP ? 'Reopen Plonix' : 'Open Plonix from your terminal' }),
        message ? h('p', { text: message }) : null,
        IN_APP
          ? h('p', { text: 'Quit Plonix and open it again to reconnect to the capture engine.' })
          : [
              h('p', { text: 'For your safety this page only opens through a one-time link. Run:' }),
              h('pre', { text: 'plonix ui' }),
              h('p', { class: 'muted', text: 'or `plonix open <target>` to start capturing and open this window in one step.' }),
            ],
      ),
    ),
  );
}

/* ---------- theme ---------- */

function applyTheme(t) {
  if (t === 'auto') document.documentElement.removeAttribute('data-theme');
  else document.documentElement.setAttribute('data-theme', t);
  S.theme = t;
}

function cycleTheme() {
  const next = { auto: 'light', light: 'dark', dark: 'auto' }[S.theme] || 'auto';
  applyTheme(next);
  store('plonix.theme', next);
  toast('Theme: ' + next);
}

/* ---------- shell ---------- */

const VIEWS = {
  traffic: { label: 'Traffic', ico: '⇅', render: renderTraffic },
  bench: { label: 'Bench', ico: '⎇', render: renderBench },
  scope: { label: 'Scope', ico: '◉', render: renderScope },
  map: { label: 'Map', ico: '⊞', render: renderMap },
  findings: { label: 'Findings', ico: '⚑', render: renderFindings },
  agents: { label: 'Agents', ico: '✦', render: renderAgents },
};

const IN_APP = !!window.__PLONIX_APP__;

function renderShell() {
  const nav = h('div', { class: 'nav' });
  Object.entries(VIEWS).forEach(([key, v], i) => {
    nav.append(
      h(
        'button',
        { 'data-v': key, title: `${v.label}  (${IN_APP ? '⌘' : ''}${i + 1})`, onclick: () => go(key) },
        h('span', { class: 'ico', text: v.ico }),
        h('span', { class: 'nl', text: v.label }),
        h('span', { class: 'ct', id: 'ct-' + key }),
      ),
    );
  });
  clear(
    $('#app'),
    h(
      'div',
      { class: 'titlebar' },
      h('div', { class: 'brand' }, h('img', { src: '/ui/icon.svg', alt: '' }), 'Plonix', h('span', { class: 'proj', id: 'proj' })),
      h(
        'div',
        { class: 'right' },
        h('div', { class: 'engine', id: 'engine' }, h('span', { class: 'dot' }), h('span', { id: 'enginetxt' })),
        h('button', { class: 'iconbtn', title: 'Theme (auto / light / dark)', onclick: cycleTheme, text: '◐' }),
      ),
    ),
    h(
      'div',
      { class: 'body' },
      h(
        'nav',
        { class: 'rail' + (S.railCollapsed ? ' collapsed' : ''), id: 'rail' },
        h(
          'button',
          { class: 'opentarget', title: `Open a target in the capture browser${IN_APP ? '  (⌘O)' : ''}`, onclick: openTarget },
          h('span', { class: 'ico', text: '+' }),
          h('span', { class: 'nl', text: 'Open target' }),
        ),
        nav,
        h('div', { class: 'railsecs', id: 'railsecs' }),
        h('div', { class: 'spacer' }),
        h(
          'button',
          { class: 'railtoggle', id: 'railtoggle', onclick: toggleSidebar },
          h('span', { class: 'ico', text: '⇤' }),
          h('span', { class: 'nl', text: 'Collapse' }),
        ),
      ),
      h('section', { class: 'main', id: 'main' }),
    ),
    h(
      'div',
      { class: 'footbar' },
      h('div', { class: 'seg' }, 'proxy', h('b', { id: 'f-proxy' })),
      h('div', { class: 'seg' }, 'CA', h('b', { id: 'f-ca' })),
      h('div', { class: 'seg push' }, 'captured', h('b', { id: 'f-cap' })),
      h('div', { class: 'seg' }, 'Plonix', h('b', { id: 'f-ver' })),
    ),
  );
  updateToggle();
  updateChrome();
  loadScope();
  loadFacets();
  loadAgentSettings();
  go(S.view, true);
}

function toggleSidebar() {
  S.railCollapsed = !S.railCollapsed;
  store('plonix.rail', S.railCollapsed ? 'collapsed' : null);
  const rail = $('#rail');
  if (rail) rail.classList.toggle('collapsed', S.railCollapsed);
  updateToggle();
}

function updateToggle() {
  const b = $('#railtoggle');
  if (!b) return;
  b.title = (S.railCollapsed ? 'Show the sidebar' : 'Collapse the sidebar') + (IN_APP ? '  (⌃⌘S)' : '  (\\)');
  b.querySelector('.ico').textContent = S.railCollapsed ? '⇥' : '⇤';
}

/** Sidebar shortcuts: scope decisions waiting on you, and the hosts you are testing. */
function renderRail() {
  const box = $('#railsecs');
  if (!box) return;
  const secs = [];
  const waiting = stillPending(S.scope.suggestions);
  const pending = waiting.slice(0, 4);
  if (pending.length) {
    secs.push(
      h('button', { class: 'navsec navlink', title: 'Review them on the Scope screen', onclick: () => go('scope') }, 'Scope suggestions', h('span', { class: 'qn', text: waiting.length })),
      pending.map((sg) =>
        h(
          'div',
          { class: 'railrow sugg-row', title: `${sg.domain}: seen in ${sg.requests || 0} requests. Accept it into scope, or keep it out.` },
          h('button', { class: 'rl', text: sg.domain, onclick: () => go('scope') }),
          h('button', { class: 'mini ok', text: '✓', title: 'Accept ' + suggestionBase(sg.domain) + ' only', onclick: () => decideDomain('accept', suggestionBase(sg.domain), false) }),
          h('button', { class: 'mini no', text: '✗', title: 'Keep ' + sg.domain + ' out of scope', onclick: () => decideDomain('reject', sg.domain, false) }),
        ),
      ),
    );
  }
  const hosts = ((S.facets && S.facets.hosts) || []).slice(0, 6);
  secs.push(h('div', { class: 'navsec', text: 'In scope' }));
  if (hosts.length) {
    secs.push(
      hosts.map((x) =>
        h(
          'button',
          { class: 'railrow hostlink', title: 'Show traffic for ' + x.value, onclick: () => setQuery('host:' + x.value) },
          h('span', { class: 'rl', text: x.value }),
          h('span', { class: 'ct', text: x.count }),
        ),
      ),
    );
  } else {
    const rules = (S.scope.rules || []).filter((r) => r.decision === 'accepted');
    secs.push(
      rules.length
        ? rules.slice(0, 6).map((r) => h('div', { class: 'railrow muted' }, h('span', { class: 'rl', text: (r.include_subdomains ? '*.' : '') + r.pattern })))
        : h('div', { class: 'railnote', text: 'Open a target to start. Its domain goes into scope, and related domains are suggested as you browse.' }),
    );
  }
  clear(box, secs);
}

/** Opens a target in the capture browser: an isolated browser that routes through Plonix. */
function openTarget() {
  const input = h('input', { placeholder: 'example.com', spellcheck: 'false', autocomplete: 'off', value: store('plonix.lastTarget') || '' });
  const go = async () => {
    const target = input.value.trim();
    if (!target) return input.focus();
    btn.disabled = true;
    m.err.textContent = '';
    const r = await launchTarget(target);
    btn.disabled = false;
    if (r.ok) closeModal();
    else m.err.textContent = r.message;
  };
  input.addEventListener('keydown', (e) => e.key === 'Enter' && go());
  const btn = h('button', { class: 'btn primary', text: 'Open', onclick: go });
  const m = modal(
    'Open a target',
    [
      h('label', null, 'Site or URL', input),
      h('p', { class: 'muted mnote', text: 'Opens a separate browser that captures through Plonix and trusts its certificate. The domain and its subdomains go into scope.' }),
    ],
    [h('button', { class: 'btn', text: 'Cancel', onclick: closeModal }), btn],
  );
  input.select();
}

async function launchTarget(target) {
  try {
    const r = await api('/api/browser/open', { method: 'POST', body: { target } });
    store('plonix.lastTarget', target);
    toast(`Opened ${r.url} in ${r.browser}. Browse the site; requests appear in Traffic.`, 'ok');
    await loadScope();
    if (S.view !== 'traffic') go('traffic');
    return { ok: true };
  } catch (e) {
    return { ok: false, message: e.message };
  }
}

function go(view, force) {
  if (!VIEWS[view]) return;
  if (S.view === view && !force) return;
  S.view = view;
  if (location.hash !== '#/' + view) history.replaceState(null, '', '#/' + view);
  for (const b of document.querySelectorAll('.nav button')) b.classList.toggle('on', b.dataset.v === view);
  const main = $('#main');
  clear(main);
  VIEWS[view].render(main);
}

function updateChrome() {
  const st = S.status;
  if (!st || !$('#engine')) return;
  $('#proj').textContent = '· ' + st.project;
  document.title = 'Plonix · ' + st.project;
  const eng = $('#engine');
  eng.classList.toggle('down', !S.engineUp);
  clear(
    $('#enginetxt'),
    S.engineUp ? 'capturing' : 'engine stopped',
    h('span', { class: 'full', text: S.engineUp ? ' · proxy ' + st.proxy : IN_APP ? ' · reopen Plonix' : ' · run plonix ui' }),
  );
  $('#f-proxy').textContent = st.proxy;
  $('#f-ca').textContent = (st.ca_fingerprint || '').slice(0, 23) + '…';
  $('#f-cap').textContent = st.exchanges;
  $('#f-ver').textContent = st.version;
  $('#ct-traffic').textContent = st.exchanges || '';
  const pending = stillPending(S.scope.suggestions).length;
  const pend = $('#ct-scope');
  pend.textContent = pending || '';
  pend.classList.toggle('hot', pending > 0);
  const rules = S.scope.rules || [];
  $('#ct-scope').title = rules.filter((r) => r.decision === 'accepted').length + ' in scope, ' + pending + ' suggested';
  $('#ct-findings').textContent = S.findingsCount || '';
  $('#ct-bench').textContent = R.tabs.length || '';
}

/* ---------- live updates ---------- */

async function poll() {
  clearTimeout(S.pollTimer);
  try {
    const st = await api('/api/status');
    const prev = S.status || {};
    S.status = st;
    const wasDown = !S.engineUp;
    S.engineUp = true;
    if (wasDown || st.exchanges !== prev.exchanges || st.pending_suggestions !== prev.pending_suggestions || st.scope_rules !== prev.scope_rules) {
      await loadScope();
      loadFacets();
      if (S.view === 'traffic') T.refresh && T.refresh();
      if (S.view === 'scope') renderScopeBody();
      if (S.view === 'map' && st.exchanges !== prev.exchanges) M.dirty = true;
    }
    if (S.view === 'agents' && Date.now() - (S.agentsAt || 0) > 4000) loadAgents();
  } catch (e) {
    if (e.code === 'unauthorized') return;
    S.engineUp = false;
  }
  updateChrome();
  S.pollTimer = setTimeout(poll, S.engineUp ? 1200 : 3000);
}

/** What the captured traffic contains; drives the suggested filters and the sidebar. */
async function loadFacets() {
  if (S.facetsBusy) return;
  S.facetsBusy = true;
  try {
    S.facets = await api('/api/traffic/facets');
  } catch (_) {
  } finally {
    S.facetsBusy = false;
  }
  renderRail();
  renderChips();
}

async function loadScope() {
  try {
    S.scope = await api('/api/scope');
  } catch (_) {}
  updateChrome();
  renderRail();
  if (S.view === 'traffic') renderBanner();
  if (S.view === 'bench') renderScopeHint();
}

/** Same rule as the engine: the most specific matching rule decides. */
function decide(host) {
  host = (host || '').toLowerCase().replace(/^\[|\]$/g, '');
  let best = null;
  let bestSpec = -1;
  for (const r of S.scope.rules || []) {
    const hit = host === r.pattern || (r.include_subdomains && host.endsWith('.' + r.pattern));
    if (!hit) continue;
    const spec = r.pattern.length * 2 + (host === r.pattern && !r.include_subdomains ? 1 : 0);
    if (spec > bestSpec) {
      best = r;
      bestSpec = spec;
    }
  }
  return best ? best.decision : 'unknown';
}

async function decideDomain(action, domain, include_subdomains, quiet) {
  try {
    const rule = await api('/api/scope/' + action, { method: 'POST', body: { domain, include_subdomains: !!include_subdomains } });
    if (!quiet) {
      const label = rule.pattern + (rule.include_subdomains ? ' and subdomains' : '');
      toast(action === 'accept' ? '✓ ' + label + ' is in scope' : '✗ ' + label + ' kept out of scope', action === 'accept' ? 'ok' : '');
    }
    await loadScope();
    return rule;
  } catch (e) {
    toast(e.message, 'err');
    return null;
  }
}

/* ---------- exchange cache & raw rendering ---------- */

const exCache = new Map();
async function getExchange(id) {
  if (exCache.has(id)) return exCache.get(id);
  const ex = await api('/api/traffic/' + id);
  exCache.set(id, ex);
  if (exCache.size > 300) exCache.delete(exCache.keys().next().value);
  return ex;
}

function bodyOf(text, b64, pretty, contentType) {
  if (text == null) {
    const n = b64len(b64);
    return n ? { note: `[binary body, ${fmtSize(n)}]` } : { text: '' };
  }
  if (pretty && /json/.test(contentType || '')) {
    try {
      return { text: JSON.stringify(JSON.parse(text), null, 2) };
    } catch (_) {}
  }
  return { text };
}

function requestText(ex) {
  const lines = [`${ex.method} ${target(ex)} HTTP/1.1`, ...ex.req_headers.map(([k, v]) => `${k}: ${v}`)];
  const b = bodyOf(ex.req_text, ex.req_body, false);
  return { lines, body: b };
}

function responseText(ex, pretty) {
  if (ex.status == null) return { lines: [], body: { note: ex.error ? 'No response: ' + ex.error : 'No response' } };
  const lines = [`HTTP/1.1 ${ex.status}`, ...ex.resp_headers.map(([k, v]) => `${k}: ${v}`)];
  return { lines, body: bodyOf(ex.resp_text, ex.resp_body, pretty, header(ex.resp_headers, 'content-type')) };
}

function asPlain({ lines, body }) {
  return lines.join('\n') + (lines.length ? '\n\n' : '') + (body.text != null ? body.text : body.note || '');
}

function rawPre({ lines, body }) {
  const pre = h('pre', { class: 'raw' });
  lines.forEach((line, i) => {
    if (i === 0) pre.append(h('span', { class: 'hl-h', text: line }));
    else {
      const c = line.indexOf(':');
      pre.append(h('span', { class: 'hl-k', text: line.slice(0, c) }), line.slice(c));
    }
    pre.append('\n');
  });
  if (lines.length) pre.append('\n');
  if (body.note) pre.append(h('span', { class: 'hl-note', text: body.note }));
  else pre.append(body.text);
  return pre;
}

/* ======================================================================
   Traffic
   ====================================================================== */

const T = { text: '', filters: [], items: [], total: 0, sel: null, live: true, maxId: 0, pretty: true, inspH: store('plonix.inspH') };

/* ---------- include / exclude filters ----------
 * The search box holds free text (and accepts the full query syntax). Filters
 * are terms the user switched on: "include" shows only matching traffic,
 * "exclude" hides it. Together they make one query, the same one the CLI and
 * the API take: includes of one field match any value (status:2xx,3xx),
 * excludes are prefixed with "-".
 */
const FILTER_FIELDS = {
  host: 'Host',
  path: 'Path',
  ext: 'Extension',
  method: 'Method',
  status: 'Status',
  mime: 'Content type',
  kind: 'Kind',
  scope: 'Scope',
  source: 'Source',
  text: 'Text',
};
const FIELD_RE = /^(-?)(host|method|status|path|mime|scope|source|ext|kind):(.+)$/i;

const fieldOf = (term) => (FIELD_RE.exec(term) || [])[2]?.toLowerCase() || 'text';
const valueOf = (term) => (fieldOf(term) === 'text' ? term.replace(/^"|"$/g, '') : term.slice(term.indexOf(':') + 1));

/** Splits a typed query into its terms, keeping quoted phrases together. */
function tokens(q) {
  return (q.match(/-?(?:[^\s"]*"[^"]*"?[^\s"]*|[^\s"]+)/g) || []).filter((t) => t && t !== '-');
}

/** Field terms of a typed query as filters (one per comma separated value), and the rest. */
function liftFilters(q) {
  const filters = [];
  const rest = [];
  for (const tok of tokens(q)) {
    const m = FIELD_RE.exec(tok);
    if (!m) {
      rest.push(tok);
      continue;
    }
    const mode = m[1] ? 'exclude' : 'include';
    for (const v of m[3].replace(/^"|"$/g, '').split(',')) if (v.trim()) filters.push({ term: m[2].toLowerCase() + ':' + v.trim(), mode });
  }
  return { filters, text: rest.join(' ') };
}

/** The query the filters and the search box make together. */
function fullQuery() {
  const parts = [];
  for (const mode of ['include', 'exclude']) {
    const groups = new Map();
    for (const f of T.filters.filter((x) => x.mode === mode)) {
      const field = fieldOf(f.term);
      if (field === 'text') {
        parts.push((mode === 'exclude' ? '-' : '') + f.term);
        continue;
      }
      if (!groups.has(field)) groups.set(field, []);
      groups.get(field).push(valueOf(f.term));
    }
    for (const [field, values] of groups) parts.push((mode === 'exclude' ? '-' : '') + field + ':' + values.join(','));
  }
  if (T.text.trim()) parts.push(T.text.trim());
  return parts.join(' ');
}

function filterLabel(term) {
  const field = fieldOf(term);
  const v = valueOf(term);
  const named = {
    'kind:static': 'Static files',
    'scope:in': 'In scope',
    'scope:out': 'Out of scope',
    'source:replay': 'Sent from Bench',
    'source:proxy': 'Captured',
    'status:none': 'No response',
  }[term.toLowerCase()];
  if (named) return { key: '', value: named };
  if (field === 'text') return { key: '', value: '“' + v + '”' };
  if (field === 'ext') return { key: 'ext', value: '.' + v.replace(/^\./, '') };
  if (field === 'mime') return { key: 'type', value: v };
  return { key: field, value: v };
}

/** Adds a filter; the same term in the other mode is switched over. */
function addFilter(term, mode) {
  const i = T.filters.findIndex((f) => f.term.toLowerCase() === term.toLowerCase());
  if (i >= 0) T.filters[i].mode = mode;
  else T.filters.push({ term, mode });
  filtersChanged();
}

function removeFilter(term) {
  T.filters = T.filters.filter((f) => f.term !== term);
  filtersChanged();
}

function flipFilter(term) {
  const f = T.filters.find((x) => x.term === term);
  if (f) f.mode = f.mode === 'include' ? 'exclude' : 'include';
  filtersChanged();
}

function clearFilters() {
  T.filters = [];
  filtersChanged();
}

function filtersChanged() {
  renderChips();
  saveTrafficView();
  if (T.refresh) T.refresh(true);
}

/** Saved with the project in the engine, so filters survive reloads and match in every window. */
function saveTrafficView() {
  clearTimeout(T.saveT);
  T.saveT = setTimeout(() => {
    api('/api/views/traffic', { method: 'PUT', body: { filters: T.filters, text: T.text } }).catch(() => {});
  }, 300);
}

async function loadTrafficView() {
  if (T.viewLoaded) return;
  try {
    const v = await api('/api/views/traffic');
    T.viewLoaded = true;
    if (Array.isArray(v.filters)) {
      T.filters = v.filters.filter((f) => f && typeof f.term === 'string' && (f.mode === 'include' || f.mode === 'exclude'));
      T.text = typeof v.text === 'string' ? v.text : '';
    } else {
      // Searches typed before filters existed: their field terms become filters.
      const old = store('plonix.q');
      if (old) {
        Object.assign(T, liftFilters(old));
        store('plonix.q', null);
        saveTrafficView();
      }
    }
  } catch (_) {
    /* engine unreachable: start without saved filters */
  }
}

/** Moves field terms typed in the search box into filters. */
function liftTyped() {
  const input = $('#q');
  if (!input) return;
  const { filters, text } = liftFilters(input.value);
  if (!filters.length) return;
  for (const f of filters) {
    const i = T.filters.findIndex((x) => x.term.toLowerCase() === f.term.toLowerCase());
    if (i >= 0) T.filters[i].mode = f.mode;
    else T.filters.push(f);
  }
  T.text = text;
  input.value = text;
  filtersChanged();
}

function renderTraffic(main) {
  const input = h('input', {
    id: 'q',
    value: T.text,
    placeholder: 'Search any text, or type a filter: host:example.com -kind:static status:4xx,5xx',
    spellcheck: 'false',
    autocomplete: 'off',
    oninput: () => {
      T.text = input.value;
      clearTimeout(T.qt);
      T.qt = setTimeout(() => {
        saveTrafficView();
        T.refresh(true);
      }, 220);
    },
    onkeydown: (e) => {
      if (e.key === 'Enter') {
        liftTyped();
        T.refresh(true);
      }
      if (e.key === 'Escape') input.blur();
    },
    onblur: () => liftTyped(),
  });
  const liveBtn = h('button', {
    class: 'live iconbtn',
    title: 'Pause or resume live updates',
    onclick: () => {
      T.live = !T.live;
      liveBtn.classList.toggle('paused', !T.live);
      liveLbl.textContent = T.live ? 'Live' : 'Paused';
      if (T.live) T.refresh();
    },
  });
  const liveLbl = h('span', { text: T.live ? 'Live' : 'Paused' });
  append(liveBtn, [h('span', { class: 'dot' }), liveLbl]);
  liveBtn.classList.toggle('paused', !T.live);

  const tbody = h('tbody', { id: 'rows' });
  const table = h(
    'table',
    { class: 'ttable' },
    h(
      'colgroup',
      null,
      h('col', { style: { width: '52px' } }),
      h('col', { style: { width: '66px' } }),
      h('col', { class: 'c-host', style: { width: '22%' } }),
      h('col'),
      h('col', { style: { width: '56px' } }),
      h('col', { style: { width: '92px' } }),
      h('col', { class: 'c-size', style: { width: '72px' } }),
      h('col', { class: 'c-ms', style: { width: '60px' } }),
      h('col', { style: { width: '84px' } }),
    ),
    h(
      'thead',
      null,
      h(
        'tr',
        null,
        h('th', { class: 'num', text: '#' }),
        h('th', { text: 'Method' }),
        h('th', { class: 'c-host', text: 'Host' }),
        h('th', { text: 'Path' }),
        h('th', { text: 'Status' }),
        h('th', { text: 'Type' }),
        h('th', { class: 'num c-size', text: 'Size' }),
        h('th', { class: 'num c-ms', text: 'ms' }),
        h('th', { text: 'Time' }),
      ),
    ),
    tbody,
  );
  const wrap = h('div', { class: 'tablewrap', id: 'tablewrap' }, table);
  append(main, [
    h(
      'div',
      { class: 'view' },
      h(
        'div',
        { class: 'toolbar' },
        h('div', { class: 'search', id: 'searchbox' }, h('span', { class: 'mg', text: '⌕' }), input, h('kbd', { text: '/' })),
        h('span', { class: 'count', id: 'tcount' }),
        liveBtn,
      ),
      h('div', { class: 'filterchips', id: 'chips' }),
      h('div', { class: 'qerr', id: 'qerr', hidden: true }),
      h('div', { id: 'bannerslot' }),
      h('div', { class: 'traffic' }, wrap, h('div', { id: 'inspslot' })),
    ),
  ]);
  renderChips();
  renderBanner();
  T.refresh = refreshTraffic;
  loadTrafficView().then(() => {
    if (S.view !== 'traffic' || !input.isConnected) return;
    input.value = T.text;
    renderChips();
    T.refresh(true);
  });
  if (T.sel) openInspector(T.sel);
}

/**
 * Filters worth one click, built from what was actually captured: only values
 * that occur, and only filters that narrow the list. Each has a natural mode:
 * errors are worth showing alone, static files and third parties worth hiding.
 */
function suggestedFilters(f) {
  const out = [];
  const n = (f && f.sampled) || 0;
  if (!n) return out;
  const count = (list, v) => ((list || []).find((c) => c.value === v) || {}).count || 0;
  const add = (term, label, c, mode, kind, of = n) => c > 0 && c < of && out.push({ term, label, count: c, mode, kind: kind || '' });
  // Static files are noise when hunting.
  const statics = ['image', 'css', 'font', 'javascript'].reduce((a, k) => a + count(f.kinds, k), 0);
  add('kind:static', 'Hide static files', statics, 'exclude', 'neg');
  if (f.in_scope && f.out_of_scope) add('scope:in', 'In scope', f.in_scope, 'include', 'scope');
  add('status:5xx', 'Server errors', count(f.statuses, '5xx'), 'include', 'bad');
  add('status:4xx', 'Client errors', count(f.statuses, '4xx'), 'include', 'warn');
  add('status:none', 'No response', count(f.statuses, 'none'), 'include', 'bad');
  for (const m of f.methods || []) if (!['GET', 'HEAD', 'OPTIONS'].includes(m.value)) add('method:' + m.value, m.value, m.count, 'include', 'method');
  add('mime:json', 'JSON', count(f.kinds, 'json'), 'include');
  add('mime:xml', 'XML', count(f.kinds, 'xml'), 'include');
  for (const p of (f.paths || []).slice(0, 3)) if (p.count >= 2) add('path:' + p.value, p.value + '/…', p.count, 'include', 'path', f.in_scope + f.out_of_scope);
  if ((f.hosts || []).length > 1) for (const x of f.hosts.slice(0, 2)) add('host:' + x.value, x.value, x.count, 'include', 'host');
  // The busiest third-party hosts (analytics, CDNs) are the usual ones to hide.
  for (const x of (f.other_hosts || []).slice(0, 2)) if (x.count >= 3) add('host:' + x.value, 'Hide ' + x.value, x.count, 'exclude', 'host neg');
  add('source:replay', 'Sent from Bench', f.replays, 'include', 'replay');
  return out;
}

function filterChip(f) {
  const { key, value } = filterLabel(f.term);
  const other = f.mode === 'include' ? 'hide it instead' : 'show only it instead';
  return h(
    'span',
    { class: 'fchip ' + f.mode, title: (f.mode === 'include' ? '-' : '') + f.term },
    h(
      'button',
      { class: 'fbody', title: 'Click to ' + other, onclick: () => flipFilter(f.term) },
      h('span', { class: 'fmode', text: f.mode === 'include' ? '+' : '−' }),
      key ? h('span', { class: 'fk', text: key }) : null,
      h('span', { class: 'fv', text: value }),
    ),
    h('button', { class: 'fx', title: 'Remove this filter', 'aria-label': 'Remove filter ' + f.term, text: '×', onclick: () => removeFilter(f.term) }),
  );
}

function renderChips() {
  const box = $('#chips');
  if (!box) return;
  const active = (term) => T.filters.some((f) => f.term.toLowerCase() === term.toLowerCase());
  const sugg = suggestedFilters(S.facets).filter((c) => !active(c.term));
  const inc = T.filters.filter((f) => f.mode === 'include');
  const exc = T.filters.filter((f) => f.mode === 'exclude');
  const q = fullQuery();
  clear(
    box,
    h('button', { class: 'addfilter', id: 'addfilter', title: 'Add an include or exclude filter', onclick: (e) => openFilterBuilder(e.currentTarget) }, '+ Filter'),
    inc.length ? h('span', { class: 'fgroup' }, h('span', { class: 'chipslbl', text: 'Show only' }), inc.map(filterChip)) : null,
    exc.length ? h('span', { class: 'fgroup' }, h('span', { class: 'chipslbl', text: 'Hide' }), exc.map(filterChip)) : null,
    T.filters.length
      ? [
          h('button', { class: 'linkbtn', text: 'Clear', title: 'Remove all filters', onclick: clearFilters }),
          h('button', {
            class: 'linkbtn',
            text: 'Copy query',
            title: 'Copy as a search query for the CLI or the API:\n' + q,
            onclick: () => copyText(fullQuery()),
          }),
        ]
      : null,
    sugg.length ? h('span', { class: 'fsep' }) : null,
    sugg.map((c) =>
      h(
        'button',
        {
          class: 'chip k-' + c.kind.split(' ').join(' k-'),
          title: (c.mode === 'include' ? 'Show only: ' : 'Hide: ') + c.term,
          onclick: () => addFilter(c.term, c.mode),
        },
        h('span', { text: c.label }),
        h('span', { class: 'n', text: c.count }),
      ),
    ),
  );
}

/** Values to offer for a field, from what was captured. */
function fieldValues(field) {
  const f = S.facets || {};
  const vals = (list) => (list || []).map((c) => c.value);
  switch (field) {
    case 'host':
      return [...vals(f.hosts), ...vals(f.other_hosts)];
    case 'path':
      return vals(f.paths);
    case 'method':
      return vals(f.methods);
    case 'status':
      return [...vals(f.statuses).filter((s) => s !== 'other'), '200', '301', '302', '401', '403', '404', '500'];
    case 'mime':
      return vals(f.kinds);
    case 'ext':
      return ['js', 'css', 'png', 'svg', 'woff2', 'map', 'json', 'html', 'php'];
    case 'kind':
      return ['static'];
    case 'scope':
      return ['in', 'out'];
    case 'source':
      return ['proxy', 'replay'];
    default:
      return [];
  }
}

/** A small popover to build one include or exclude filter. */
function openFilterBuilder(anchor, preset = {}) {
  closePopover();
  let mode = preset.mode || 'include';
  const field = h(
    'select',
    { 'aria-label': 'Field' },
    Object.entries(FILTER_FIELDS).map(([k, label]) => h('option', { value: k, text: label })),
  );
  field.value = preset.field || 'host';
  const list = h('datalist', { id: 'fvals' });
  const value = h('input', { list: 'fvals', placeholder: 'value', spellcheck: 'false', autocomplete: 'off', value: preset.value || '' });
  const err = h('div', { class: 'perr', hidden: true });
  const seg = h('div', { class: 'seg' });
  const drawSeg = () =>
    clear(
      seg,
      ['include', 'exclude'].map((m) =>
        h('button', { class: mode === m ? 'on ' + m : m, text: m === 'include' ? 'Show only' : 'Hide', onclick: () => ((mode = m), drawSeg()) }),
      ),
    );
  const fillValues = () => {
    clear(list, fieldValues(field.value).map((v) => h('option', { value: v })));
    value.placeholder = { host: 'example.com or *.cdn.*', path: '/api', ext: 'js', status: '4xx or 404', mime: 'json', text: 'any text' }[field.value] || 'value';
  };
  field.onchange = () => {
    fillValues();
    value.value = field.value === 'kind' ? 'static' : '';
    value.focus();
  };
  const add = async () => {
    const v = value.value.trim();
    if (!v) return value.focus();
    const term = field.value === 'text' ? (/\s/.test(v) ? '"' + v.replace(/"/g, '') + '"' : v) : field.value + ':' + v.replace(/\s+/g, '');
    try {
      await api('/api/traffic?limit=0&q=' + encodeURIComponent(term));
    } catch (e) {
      err.hidden = false;
      err.textContent = e.message;
      return;
    }
    closePopover();
    // "status:4xx,5xx" typed in the box makes one chip per value.
    for (const one of field.value === 'text' ? [term] : v.split(',').filter((x) => x.trim()).map((x) => field.value + ':' + x.trim())) addFilter(one, mode);
  };
  value.addEventListener('keydown', (e) => e.key === 'Enter' && add());
  drawSeg();
  fillValues();
  const pop = h(
    'div',
    { class: 'popover', role: 'dialog', 'aria-label': 'Add filter' },
    seg,
    h('div', { class: 'prow' }, field, value, list),
    err,
    h('div', { class: 'pfoot' }, h('button', { class: 'btn sm', text: 'Cancel', onclick: closePopover }), h('button', { class: 'btn sm primary', text: 'Add filter', onclick: add })),
  );
  showPopover(pop, anchor.getBoundingClientRect());
  value.focus();
}

function showPopover(pop, rect) {
  document.body.append(pop);
  const w = pop.offsetWidth;
  pop.style.left = Math.max(8, Math.min(rect.left, window.innerWidth - w - 8)) + 'px';
  pop.style.top = Math.min(rect.bottom + 6, window.innerHeight - pop.offsetHeight - 8) + 'px';
  setTimeout(() => document.addEventListener('mousedown', T.popOutside = (e) => !pop.contains(e.target) && closePopover()), 0);
  pop.addEventListener('keydown', (e) => e.key === 'Escape' && (e.stopPropagation(), closePopover()));
}

function closePopover() {
  for (const p of document.querySelectorAll('.popover, .ctxmenu')) p.remove();
  if (T.popOutside) document.removeEventListener('mousedown', T.popOutside);
  T.popOutside = null;
}

/** Right-click on a row: show only or hide what it has. */
function rowMenu(e, ex) {
  e.preventDefault();
  closePopover();
  const seg = ((ex.path || '/').match(/^\/[^/?]+/) || [])[0];
  const ext = ((ex.path || '').match(/\.([a-z0-9]{1,6})$/i) || [])[1];
  const cls = ex.status == null ? 'none' : Math.floor(ex.status / 100) + 'xx';
  const kind = ex.mime ? (ex.mime.match(/json|html|javascript|xml|css|image|font/) || [])[0] : null;
  const both = (term, what) => [
    { label: 'Show only ' + what, run: () => addFilter(term, 'include') },
    { label: 'Hide ' + what, run: () => addFilter(term, 'exclude') },
  ];
  const groups = [
    both('host:' + ex.host, ex.host),
    seg && seg !== ex.path ? both('path:' + seg, seg + '/…') : both('path:' + ex.path, ex.path),
    both('status:' + cls, cls === 'none' ? 'no response' : cls + ' responses'),
    kind ? both('mime:' + kind, kind.toUpperCase() + ' responses') : null,
    ext ? both('ext:' + ext.toLowerCase(), '.' + ext.toLowerCase() + ' files') : null,
    both('method:' + ex.method, ex.method + ' requests'),
  ].filter(Boolean);
  const menu = h(
    'div',
    { class: 'ctxmenu', role: 'menu' },
    groups.map((g, i) => [
      i ? h('div', { class: 'msep' }) : null,
      g.map((it) =>
        h('button', {
          role: 'menuitem',
          text: it.label,
          onclick: () => {
            closePopover();
            it.run();
          },
        }),
      ),
    ]),
  );
  showPopover(menu, { left: e.clientX, bottom: e.clientY - 6 });
}

/** Opens Traffic searching for `q`: its field terms replace filters on the same fields, excludes stay. */
async function setQuery(q) {
  await loadTrafficView();
  const { filters, text } = liftFilters(q);
  const fields = new Set(filters.map((f) => fieldOf(f.term)));
  T.filters = T.filters.filter((f) => f.mode === 'exclude' || !fields.has(fieldOf(f.term)));
  for (const f of filters) {
    const i = T.filters.findIndex((x) => x.term.toLowerCase() === f.term.toLowerCase());
    if (i >= 0) T.filters.splice(i, 1);
    T.filters.push(f);
  }
  T.text = text;
  T.sel = null;
  saveTrafficView();
  go('traffic', true);
}

async function refreshTraffic(userAction) {
  if (!userAction && !T.live) return;
  if (S.view !== 'traffic') return;
  const seq = (T.seq = (T.seq || 0) + 1);
  let data;
  try {
    data = await api('/api/traffic?limit=500&q=' + encodeURIComponent(fullQuery()));
  } catch (e) {
    if (seq !== T.seq) return;
    if (e.code === 'bad_query') {
      $('#searchbox').classList.add('bad');
      const qe = $('#qerr');
      qe.hidden = false;
      qe.textContent = e.message;
    }
    return;
  }
  if (seq !== T.seq || S.view !== 'traffic') return;
  $('#searchbox').classList.remove('bad');
  $('#qerr').hidden = true;
  const prevMax = T.maxId;
  T.items = data.items;
  T.total = data.total;
  T.maxId = Math.max(prevMax, ...data.items.map((i) => i.id), 0);
  $('#tcount').textContent = data.total > data.items.length ? `${data.items.length} of ${data.total}` : `${data.total} request${data.total === 1 ? '' : 's'}`;
  drawRows(userAction ? Infinity : prevMax);
}

function emptyTraffic(st) {
  const input = h('input', { placeholder: 'example.com', spellcheck: 'false', autocomplete: 'off', value: store('plonix.lastTarget') || '' });
  const err = h('div', { class: 'qerr' });
  const open = async () => {
    if (!input.value.trim()) return input.focus();
    btn.disabled = true;
    const r = await launchTarget(input.value.trim());
    btn.disabled = false;
    err.textContent = r.ok ? '' : r.message;
  };
  input.addEventListener('keydown', (e) => e.key === 'Enter' && open());
  const btn = h('button', { class: 'btn primary', text: 'Open in capture browser', onclick: open });
  return h(
    'div',
    { class: 'empty' },
    h('h3', { text: 'Start capturing' }),
    'Type the site you are testing. Plonix opens a browser that captures through it, and requests appear here live.',
    h('div', { class: 'starter' }, input, btn),
    err,
    h('div', { class: 'muted fine' }, 'Or point any browser at the proxy ', h('code', { text: st.proxy || '' }), '.'),
  );
}

function drawRows(freshAbove) {
  const tbody = $('#rows');
  if (!tbody) return;
  if (!T.items.length) {
    const st = S.status || {};
    const msg = fullQuery()
      ? h(
          'div',
          { class: 'empty' },
          h('h3', { text: T.filters.length ? 'No traffic matches these filters' : 'No traffic matches this search' }),
          T.filters.length ? h('button', { class: 'btn sm', text: 'Clear filters', onclick: clearFilters }) : 'Try other words.',
        )
      : emptyTraffic(st);
    clear(tbody, h('tr', null, h('td', { colspan: 9, style: { height: 'auto', whiteSpace: 'normal' } }, msg)));
    return;
  }
  const rows = T.items.map((ex) => {
    const tags = [];
    if (ex.source === 'replay') tags.push(h('span', { class: 'tag replay', text: 'sent' }));
    const tr = h(
      'tr',
      {
        'data-id': ex.id,
        class: [ex.id === T.sel ? 'sel' : '', ex.in_scope ? '' : 'out', ex.id > freshAbove ? 'fresh' : ''].join(' ').trim(),
        onclick: () => openInspector(ex.id),
        ondblclick: () => sendToBench(ex.id),
        oncontextmenu: (e) => rowMenu(e, ex),
      },
      h('td', { class: 'num', text: ex.id }),
      h('td', null, h('span', { class: 'meth m-' + ex.method, text: ex.method })),
      h('td', { class: 'host c-host', text: ex.host + (ex.port !== 443 && ex.port !== 80 ? ':' + ex.port : ''), title: ex.host }),
      h('td', { class: 'url', title: target(ex) }, tags, tags.length ? ' ' : '', target(ex)),
      h('td', null, h('span', { class: statusClass(ex.status), text: ex.status == null ? 'ERR' : ex.status })),
      h('td', null, ex.in_scope ? null : h('span', { class: 'tag out', text: 'out' }), ' ', h('span', { class: 'mime', text: shortMime(ex.mime) })),
      h('td', { class: 'num c-size', text: fmtSize(ex.resp_len) }),
      h('td', { class: 'num c-ms', text: ex.duration_ms }),
      h('td', { class: 'num', text: fmtTime(ex.ts) }),
    );
    return tr;
  });
  clear(tbody, rows);
}

function selectRow(delta) {
  if (!T.items.length) return;
  let i = T.items.findIndex((x) => x.id === T.sel);
  i = i < 0 ? 0 : Math.max(0, Math.min(T.items.length - 1, i + delta));
  openInspector(T.items[i].id);
  const tr = document.querySelector(`#rows tr[data-id="${T.items[i].id}"]`);
  if (tr) tr.scrollIntoView({ block: 'nearest' });
}

async function openInspector(id) {
  T.sel = id;
  for (const tr of document.querySelectorAll('#rows tr')) tr.classList.toggle('sel', Number(tr.dataset.id) === id);
  const slot = $('#inspslot');
  if (!slot) return;
  let ex;
  try {
    ex = await getExchange(id);
  } catch (e) {
    toast(e.message, 'err');
    return;
  }
  if (T.sel !== id || !$('#inspslot')) return;
  const insp = h('div', { class: 'inspector', id: 'inspector', 'aria-label': 'Lens' });
  if (T.inspH) insp.style.height = T.inspH + 'px';
  const enc = header(ex.resp_headers, 'content-encoding');
  const isJson = /json/.test(header(ex.resp_headers, 'content-type') || '');
  const respCol = h('div', { class: 'col' });
  const drawResp = () => {
    const seg = isJson
      ? h(
          'span',
          { class: 'seg r' },
          h('button', { class: T.pretty ? 'on' : '', text: 'Pretty', onclick: () => ((T.pretty = true), drawResp()) }),
          h('button', { class: T.pretty ? '' : 'on', text: 'Raw', onclick: () => ((T.pretty = false), drawResp()) }),
        )
      : null;
    clear(
      respCol,
      h('div', { class: 'lbl' }, 'Response', enc ? h('span', { class: 'decodetag', text: 'decoded · ' + enc }) : null, seg),
      h('div', { class: 'spotslot' }),
      rawPre(responseText(ex, T.pretty)),
    );
    if (ex.insights) drawInsights(respCol, ex.insights.filter((i) => i.side === 'response'));
  };
  drawResp();
  const reqCol = h(
    'div',
    { class: 'col' },
    h('div', { class: 'lbl' }, 'Request', h('span', { class: 'r', text: '#' + ex.id + ' · ' + fmtTime(ex.ts) + ' · ' + (ex.initiator || ex.source || 'proxy') })),
    h('div', { class: 'spotslot' }),
    rawPre(requestText(ex)),
  );
  append(insp, [
    h(
      'div',
      { class: 'insp-head' },
      h('span', { class: 'lens', text: 'Lens', title: 'Lens: the selected request and response' }),
      h('span', { class: 'meth m-' + ex.method, text: ex.method }),
      h('span', { class: 'ip', text: ex.url, title: ex.url }),
      h(
        'span',
        { class: 'meta' },
        h('span', { class: statusClass(ex.status), text: ex.status == null ? 'no response' : ex.status }),
        h('span', { text: ex.duration_ms + ' ms' }),
        h('span', { text: fmtSize(b64len(ex.resp_body)) }),
        h('span', { class: 'tag ' + scopeTag(decide(ex.host)), text: scopeLabel(decide(ex.host)) }),
      ),
      h('button', { class: 'btn sm primary', text: 'Send to Bench', title: 'Edit and re-send on the Bench (b, or double-click a row)', onclick: () => sendToBench(id) }),
      h('button', { class: 'btn sm', text: 'New finding', onclick: () => newFinding([id], `${ex.method} ${ex.path}`) }),
      askButton({ kind: 'request', id }),
      h('button', { class: 'iconbtn', text: '✕', title: 'Close (Esc)', onclick: closeInspector }),
    ),
    sideBySide('split', 'lens', reqCol, respCol),
  ]);
  const splitter = h('div', { class: 'splitter', onmousedown: (e) => startResize(e, insp) });
  clear(slot, splitter, insp);
  slot.style.display = 'contents';
  loadInsights(ex).then((list) => {
    if (T.sel !== id || !list) return;
    drawInsights(reqCol, list.filter((i) => i.side === 'request'));
    drawInsights(respCol, list.filter((i) => i.side === 'response'));
  });
}

/* ---------- insights: what stands out in a request or response ---------- */

async function loadInsights(ex) {
  if (ex.insights) return ex.insights;
  try {
    ex.insights = await api('/api/traffic/' + ex.id + '/insights');
  } catch (_) {
    return null;
  }
  return ex.insights;
}

const CAT_TITLE = { secret: 'Exposed secret', decode: 'Decodable value', pii: 'Personal data', info: 'Infrastructure detail' };

/** A row of chips under the Request or Response label, one per thing spotted. */
function drawInsights(col, list) {
  const slot = col.querySelector('.spotslot');
  if (!slot) return;
  if (!list.length) return clear(slot);
  const pre = () => col.querySelector('pre.raw');
  const detail = h('div', { class: 'spotdetail', hidden: true });
  let open = null;
  const chips = list.map((ins) => {
    const chip = h(
      'button',
      {
        class: 'spot c-' + ins.category,
        title: `${CAT_TITLE[ins.category] || ''} · ${ins.location}${ins.count > 1 ? ` · seen ${ins.count} times` : ''}`,
        onclick: () => {
          for (const c of slot.querySelectorAll('.spot')) c.classList.remove('on');
          unmark(pre());
          if (open === ins) {
            open = null;
            detail.hidden = true;
            return;
          }
          open = ins;
          chip.classList.add('on');
          showInsight(detail, ins);
          markIn(pre(), ins.value);
        },
      },
      h('i'),
      h('span', { class: 'sl', text: ins.label }),
      h('span', { class: 'sw', text: shortLocation(ins.location) }),
      ins.count > 1 ? h('span', { class: 'sn', text: '×' + ins.count }) : null,
    );
    return chip;
  });
  clear(slot, h('div', { class: 'spots' }, h('span', { class: 'spotlbl', text: 'Spotted' }), chips), detail);
}

function shortLocation(loc) {
  return loc.replace(/^(header|cookie|query parameter|form field) /, '').replace(/^(request|response) body ?/, '') || 'body';
}

function showInsight(box, ins) {
  box.hidden = false;
  const actions = [];
  if (ins.decoded != null) actions.push(h('button', { class: 'btn sm', text: 'Copy decoded', onclick: () => copyText(ins.decoded) }));
  actions.push(h('button', { class: 'btn sm', text: 'Copy value', onclick: () => copyText(ins.value) }));
  const needle = ins.value.replace(/"/g, '').slice(0, 120);
  if (needle.length >= 4) actions.push(h('button', { class: 'btn sm', text: 'Find in traffic', title: 'Search all captured traffic for this value', onclick: () => setQuery('"' + needle + '"') }));
  clear(
    box,
    h(
      'div',
      { class: 'sdh' },
      h('b', { text: ins.label }),
      h('span', { class: 'muted', text: ' in ' + ins.location + (ins.count > 1 ? ` · seen ${ins.count} times` : '') }),
      h('span', { class: 'sdact' }, actions),
    ),
    ins.notes && ins.notes.length ? h('div', { class: 'sdnotes' }, ins.notes.map((n) => h('span', { class: 'sdnote' + (/^(unsigned|expired|encoded twice|username)/.test(n) ? ' warn' : ''), text: n }))) : null,
    ins.decoded != null ? [h('div', { class: 'sdk', text: 'Decoded' }), h('pre', { class: 'sdv', text: ins.decoded })] : [h('div', { class: 'sdk', text: 'Value' }), h('pre', { class: 'sdv', text: ins.value.length > 600 ? ins.value.slice(0, 600) + '…' : ins.value })],
  );
}

async function copyText(text) {
  try {
    await navigator.clipboard.writeText(text);
    toast('Copied', 'ok');
  } catch (_) {
    toast('Could not copy to the clipboard', 'err');
  }
}

/** Highlights every occurrence of `needle` in a rendered request or response. */
function markIn(pre, needle) {
  if (!pre || !needle || needle.length < 3) return;
  const walker = document.createTreeWalker(pre, NodeFilter.SHOW_TEXT);
  const nodes = [];
  while (walker.nextNode()) nodes.push(walker.currentNode);
  let first = null;
  for (const node of nodes) {
    const text = node.nodeValue;
    let at = text.indexOf(needle);
    if (at < 0) continue;
    const frag = document.createDocumentFragment();
    let from = 0;
    while (at >= 0) {
      frag.append(text.slice(from, at));
      const m = h('mark', { class: 'hit', text: needle });
      first = first || m;
      frag.append(m);
      from = at + needle.length;
      at = text.indexOf(needle, from);
    }
    frag.append(text.slice(from));
    node.replaceWith(frag);
  }
  // Scroll only the pane, never the window around it.
  if (first) pre.scrollTo({ top: Math.max(0, first.offsetTop - pre.clientHeight / 3), behavior: 'smooth' });
}

function unmark(pre) {
  if (!pre) return;
  for (const m of pre.querySelectorAll('mark.hit')) m.replaceWith(m.textContent);
  pre.normalize();
}

/**
 * Request and response side by side, with a divider that drags to resize
 * them. The split is remembered per place (`lens`, `bench`); double-click
 * the divider to even it out again.
 */
function sideBySide(cls, key, left, right) {
  const wrap = h('div', { class: cls + ' sbs' });
  const apply = (pct) => {
    wrap.style.setProperty('--lw', pct + 'fr');
    wrap.style.setProperty('--rw', 100 - pct + 'fr');
  };
  apply(store('plonix.split.' + key) || 50);
  const bar = h('div', {
    class: 'vsplit',
    title: 'Drag to resize · double-click to reset',
    ondblclick: () => {
      apply(50);
      store('plonix.split.' + key, null);
    },
    onmousedown: (e) => {
      e.preventDefault();
      const box = wrap.getBoundingClientRect();
      document.body.classList.add('colresize');
      const move = (ev) => {
        const pct = Math.round(Math.max(15, Math.min(85, ((ev.clientX - box.left) / box.width) * 100)));
        apply(pct);
        store('plonix.split.' + key, pct);
      };
      const up = () => {
        document.body.classList.remove('colresize');
        window.removeEventListener('mousemove', move);
        window.removeEventListener('mouseup', up);
      };
      window.addEventListener('mousemove', move);
      window.addEventListener('mouseup', up);
    },
  });
  return append(wrap, [left, bar, right]);
}

function startResize(e, insp) {
  e.preventDefault();
  const startY = e.clientY;
  const startH = insp.getBoundingClientRect().height;
  const move = (ev) => {
    T.inspH = Math.max(140, Math.min(window.innerHeight - 220, startH - (ev.clientY - startY)));
    insp.style.height = T.inspH + 'px';
    store('plonix.inspH', T.inspH);
  };
  const up = () => {
    window.removeEventListener('mousemove', move);
    window.removeEventListener('mouseup', up);
  };
  window.addEventListener('mousemove', move);
  window.addEventListener('mouseup', up);
}

function closeInspector() {
  T.sel = null;
  const slot = $('#inspslot');
  if (slot) clear(slot);
  for (const tr of document.querySelectorAll('#rows tr.sel')) tr.classList.remove('sel');
}

const scopeTag = (d) => ({ accepted: 'in', rejected: 'rej', unknown: 'out' })[d];
const scopeLabel = (d) => ({ accepted: 'in scope', rejected: 'rejected', unknown: 'not in scope' })[d];

/* ---- adaptive scope banner on the traffic screen ---- */

const EV = {
  shares_session: ['🔑', 'Shares a session'],
  shares_certificate: ['🔒', 'Shares a certificate'],
  redirected_from: ['↪', 'Redirected from'],
  requested_from: ['↗', 'Called from'],
  linked_from: ['🔗', 'Linked from'],
};

/**
 * Pending scope decisions, as one slim bar above Traffic: how many are
 * waiting, the strongest one with its choices, and Accept all / Reject all.
 * Nothing here needs an answer: Skip moves to the next one, Hide tucks the
 * bar away until a new domain is suggested. The count stays in the sidebar.
 */
function renderBanner() {
  const slot = $('#bannerslot');
  if (!slot) return;
  const all = stillPending(S.scope.suggestions);
  const fresh = all.filter((s) => !(T.hidden || []).includes(s.domain));
  if (!all.length || !fresh.length) return clear(slot);
  const queue = all.filter((s) => !(T.skipped || []).includes(s.domain));
  const s = queue[0] || all[0];
  const ev = s.evidence[0];
  const [ico, label] = (ev && EV[ev.kind]) || ['•', ''];
  clear(
    slot,
    h(
      'div',
      { class: 'scopequeue' },
      h('button', { class: 'qcount', title: 'Review every suggestion on the Scope screen', onclick: () => go('scope') }, h('b', { text: all.length }), all.length === 1 ? ' scope decision' : ' scope decisions'),
      h('span', { class: 'qdom', text: s.domain, title: s.domain }),
      ev ? h('span', { class: 'qev', title: s.evidence.map((e) => e.summary).join('\n') }, h('i', { text: ico }), ' ', label, ' ', ev.via) : null,
      h('span', { class: 'qacts' }, scopeButtons(s, 'sm'), all.length > 1 ? h('button', { class: 'btn sm ghost', text: 'Skip', title: 'Decide later; show the next one', onclick: () => ((T.skipped = queue.length > 1 ? [...(T.skipped || []), s.domain] : []), renderBanner()) }) : null),
      h(
        'span',
        { class: 'qall' },
        all.length > 1 ? [h('button', { class: 'btn sm', text: 'Accept all', onclick: () => decideAll('accept') }), h('button', { class: 'btn sm', text: 'Reject all', onclick: () => decideAll('reject') })] : null,
        h('button', { class: 'iconbtn', text: '✕', title: 'Hide until a new domain is suggested (they stay on the Scope screen)', onclick: () => ((T.hidden = all.map((x) => x.domain)), renderBanner()) }),
      ),
    ),
  );
}

/** The host a suggestion is about: `*.example.com` is about example.com. */
const suggestionBase = (domain) => domain.replace(/^\*\./, '');

/**
 * Suggestions still waiting on a decision. The engine already drops a
 * suggestion once a rule covers it, but a rule added elsewhere can land a
 * moment before the next scope refresh, so we also hide anything the current
 * rules already decide. A domain that is part of an existing rule never
 * prompts again.
 */
const stillPending = (sugg) => (sugg || []).filter((s) => decide(suggestionBase(s.domain)) === 'unknown');

/** The three choices for one suggestion: this host only, with subdomains, or reject. */
function scopeButtons(s, size, after) {
  const base = suggestionBase(s.domain);
  const cls = (extra) => 'btn ' + (size || '') + ' ' + (extra || '');
  const act = (action, domain, subs) => async () => (await decideDomain(action, domain, subs)) && after && after();
  return [
    h('button', { class: cls('danger'), text: 'Reject', title: `Keep ${s.domain} out of scope`, onclick: act('reject', s.domain, false) }),
    h('button', { class: cls(), text: '+ subdomains', title: `Accept ${base} and every subdomain (*.${base})`, onclick: act('accept', base, true) }),
    h('button', { class: cls('primary'), text: 'Only ' + base, title: `Accept ${base} only, not its subdomains`, onclick: act('accept', base, false) }),
  ];
}

/** Accepts (each host only) or rejects every pending suggestion, after asking. */
function decideAll(action) {
  const list = stillPending(S.scope.suggestions);
  if (!list.length) return;
  const accept = action === 'accept';
  const run = async () => {
    closeModal();
    let done = 0;
    for (const s of list) {
      const domain = accept ? suggestionBase(s.domain) : s.domain;
      try {
        await api('/api/scope/' + action, { method: 'POST', body: { domain, include_subdomains: false } });
        done++;
      } catch (e) {
        toast(e.message, 'err');
      }
    }
    toast(accept ? `✓ ${done} domain${done === 1 ? '' : 's'} accepted into scope` : `✗ ${done} domain${done === 1 ? '' : 's'} kept out of scope`, accept ? 'ok' : '');
    await loadScope();
    if (S.view === 'scope') renderScopeBody();
  };
  modal(
    accept ? `Accept all ${list.length} suggested domains?` : `Reject all ${list.length} suggested domains?`,
    [
      h('p', { class: 'muted', text: accept ? 'Each host is accepted on its own, without its subdomains. You can change any of them on the Scope screen.' : 'They stay captured, but Bench sends to them are refused. You can change any of them on the Scope screen.' }),
      h('div', { class: 'alllist' }, list.map((s) => h('div', { class: 'mono', text: accept ? suggestionBase(s.domain) : s.domain }))),
    ],
    [h('button', { class: 'btn', text: 'Cancel', onclick: closeModal }), h('button', { class: 'btn ' + (accept ? 'primary' : 'danger'), text: accept ? 'Accept all' : 'Reject all', onclick: run })],
  );
}

function evidenceList(evidence) {
  return h(
    'div',
    { class: 'evlist' },
    evidence.map((e) => {
      const [ico, label] = EV[e.kind] || ['•', e.kind];
      return h(
        'div',
        { class: 'ev' },
        h('span', { class: 'k' }, h('i', { text: ico }), label),
        h('span', { class: 'd' }, e.summary, e.detail ? ' · ' + e.detail : ''),
        h('span', { class: 'w' }, e.count > 1 ? '×' + e.count + ' ' : '', h('button', { class: 'link', text: '#' + e.exchange_id, title: 'Show the request this came from', onclick: () => showExchange(e.exchange_id) })),
      );
    }),
  );
}

function showExchange(id) {
  T.sel = id;
  go('traffic', true);
}

/* ======================================================================
   Bench
   ====================================================================== */

const R = { tabs: [], active: 0, mode: 'response' };
(function loadBench() {
  const saved = store('plonix.bench');
  if (saved && Array.isArray(saved.tabs)) {
    R.tabs = saved.tabs;
    R.active = Math.min(saved.active || 0, Math.max(0, R.tabs.length - 1));
  }
})();
function saveBench() {
  const tabs = R.tabs.map((t) => ({ ...t, error: undefined, picks: [] }));
  store('plonix.bench', { tabs: tabs.slice(-30), active: R.active });
}
const tabNo = () => (R.counter = (R.counter || R.tabs.length) + 1);

function rawFromExchange(ex) {
  const head = ex.req_headers.map(([k, v]) => `${k}: ${v}`).join('\n');
  return head + '\n\n' + (ex.req_text != null ? ex.req_text : '');
}

async function sendToBench(id) {
  let ex;
  try {
    ex = await getExchange(id);
  } catch (e) {
    return toast(e.message, 'err');
  }
  const binary = ex.req_text == null && b64len(ex.req_body) > 0;
  R.tabs.push({
    name: `${ex.method} ${ex.path}`.slice(0, 60),
    from: id,
    method: ex.method,
    url: ex.url,
    raw: rawFromExchange(ex),
    bodyB64: binary ? ex.req_body : null,
    history: [],
    cur: null,
    picks: [],
  });
  R.active = R.tabs.length - 1;
  saveBench();
  go('bench', true);
}

function newBlankTab() {
  R.tabs.push({ name: 'Request ' + tabNo(), method: 'GET', url: 'https://', raw: 'Accept: */*\nUser-Agent: Plonix\n\n', bodyB64: null, history: [], cur: null, picks: [] });
  R.active = R.tabs.length - 1;
  saveBench();
  renderBench($('#main'));
}

/** Splits the editor text into headers and body. */
function parseRaw(raw) {
  const text = raw.replace(/\r\n/g, '\n');
  const cut = text.indexOf('\n\n');
  const head = cut < 0 ? text : text.slice(0, cut);
  const body = cut < 0 ? '' : text.slice(cut + 2);
  const headers = [];
  const bad = [];
  for (const line of head.split('\n')) {
    if (!line.trim()) continue;
    const c = line.indexOf(':');
    if (c <= 0) bad.push(line);
    else headers.push([line.slice(0, c).trim(), line.slice(c + 1).trim()]);
  }
  return { headers, body, bad };
}

function hostOf(url) {
  try {
    return new URL(url).hostname;
  } catch (_) {
    return null;
  }
}

function renderBench(main) {
  if (S.view !== 'bench') return;
  const tab = R.tabs[R.active];
  const tabs = h(
    'div',
    { class: 'rtabs' },
    R.tabs.map((t, i) =>
      h(
        'div',
        { class: 'rtab' + (i === R.active ? ' on' : ''), title: t.url, onclick: () => ((R.active = i), saveBench(), renderBench(main)) },
        h('span', { class: 'nm', text: t.name }),
        h('button', {
          class: 'x',
          text: '✕',
          title: 'Close tab',
          onclick: (e) => {
            e.stopPropagation();
            R.tabs.splice(i, 1);
            R.active = Math.max(0, Math.min(R.active, R.tabs.length - 1));
            saveBench();
            renderBench(main);
            updateChrome();
          },
        }),
      ),
    ),
    h('button', { class: 'iconbtn', text: '+', title: 'New blank request', onclick: newBlankTab }),
  );
  const view = h(
    'div',
    { class: 'view' },
    h('div', { class: 'toolbar' }, h('h2', { text: 'Bench' }), h('span', { class: 'hint', text: 'Each tab is an experiment: edit a request, send it, branch it, compare responses. Sends only reach in-scope hosts.' })),
    tabs,
  );
  clear(main, view);
  updateChrome();
  if (!tab) {
    view.append(
      h(
        'div',
        { class: 'pane' },
        h(
          'div',
          { class: 'empty' },
          h('h3', { text: 'No requests yet' }),
          'Pick a request in Traffic and press ',
          h('b', { text: 'Send to Bench' }),
          ' (or double-click it), or start a ',
          h('button', { class: 'link', text: 'blank request', onclick: newBlankTab }),
          '.',
        ),
      ),
    );
    return;
  }

  const method = h('input', { class: 'method', value: tab.method, spellcheck: 'false', list: 'methods', oninput: () => ((tab.method = method.value.toUpperCase()), saveBench()) });
  const url = h('input', {
    value: tab.url,
    spellcheck: 'false',
    oninput: () => {
      tab.url = url.value;
      saveBench();
      renderScopeHint();
    },
    onkeydown: (e) => e.key === 'Enter' && !e.metaKey && !e.ctrlKey && send(),
  });
  const editor = h('textarea', {
    value: tab.raw,
    spellcheck: 'false',
    oninput: () => ((tab.raw = editor.value), saveBench()),
    onkeydown: (e) => {
      if (e.key === 'Tab') {
        e.preventDefault();
        const s = editor.selectionStart;
        editor.setRangeText('  ', s, editor.selectionEnd, 'end');
        tab.raw = editor.value;
      }
    },
  });
  const sendBtn = h('button', { class: 'btn primary', onclick: () => send() }, 'Send', h('kbd', { text: '⌘↵' }));
  const send = async () => {
    if (sendBtn.disabled) return;
    tab.method = method.value.trim().toUpperCase() || 'GET';
    tab.url = url.value.trim();
    tab.raw = editor.value;
    const { headers, body, bad } = parseRaw(tab.raw);
    if (bad.length) return toast('Not a header line: ' + bad[0] + ' (use "Name: value", then a blank line before the body)', 'err');
    const req = { method: tab.method, url: tab.url, headers };
    if (tab.bodyB64 && !body) req.body_base64 = tab.bodyB64;
    else if (body) req.body = body;
    sendBtn.disabled = true;
    sendBtn.firstChild.textContent = 'Sending…';
    tab.error = null;
    try {
      const ex = await api('/api/send', { method: 'POST', body: req });
      exCache.set(ex.id, ex);
      tab.history.unshift({
        id: ex.id,
        ts: ex.ts,
        status: ex.status,
        ms: ex.duration_ms,
        len: b64len(ex.resp_body),
        error: ex.error,
        req: { method: tab.method, url: tab.url, raw: tab.raw },
      });
      tab.history = tab.history.slice(0, 100);
      tab.cur = ex.id;
    } catch (e) {
      tab.error = { code: e.code, message: e.message, host: hostOf(tab.url) };
      tab.cur = null;
    }
    saveBench();
    renderBench(main);
  };
  R.send = send;

  const binaryNote = tab.bodyB64 ? h('span', { class: 'r', text: `binary body (${fmtSize(b64len(tab.bodyB64))}) is sent unchanged unless you type a body` }) : h('span', { class: 'r', text: 'headers, blank line, body' });
  const respCol = h('div', { class: 'rcol' });
  const body = h(
    'div',
    { class: 'pane' },
    h(
      'div',
      { class: 'rbody' },
      h('datalist', { id: 'methods' }, ['GET', 'POST', 'PUT', 'PATCH', 'DELETE', 'HEAD', 'OPTIONS'].map((m) => h('option', { value: m }))),
      h('div', { class: 'reqbar' }, method, h('div', { class: 'urlwrap' }, url), sendBtn),
      h('div', { id: 'scopehint' }),
      sideBySide('rsplit', 'bench', h('div', { class: 'rcol' }, h('div', { class: 'lbl' }, 'Request', binaryNote), editor), respCol),
      historyPanel(tab, main),
      h('div', { id: 'cmpslot' }),
    ),
  );
  view.append(body);
  renderScopeHint();
  drawBenchResponse(tab, respCol);
  drawCompare(tab);
}

function renderScopeHint() {
  const slot = $('#scopehint');
  const tab = R.tabs[R.active];
  if (!slot || !tab) return;
  const host = hostOf(tab.url);
  if (!host) return clear(slot);
  const d = decide(host);
  if (d === 'accepted') {
    return clear(slot, h('div', { class: 'scopehint ok' }, '✓ ', h('b', { text: host }), ' is in scope: sends go out.'));
  }
  const why = d === 'rejected' ? ' was rejected from scope. The engine refuses to send to it.' : ' is not in scope yet. The engine refuses to send to it until you accept it.';
  clear(
    slot,
    h(
      'div',
      { class: 'scopehint blocked' },
      '⛔ ',
      h('b', { text: host }),
      why,
      h('button', {
        class: 'btn sm',
        text: 'Accept ' + host,
        onclick: async () => {
          if (await decideDomain('accept', host)) renderScopeHint();
        },
      }),
    ),
  );
}

async function drawBenchResponse(tab, col) {
  const label = (extra) => h('div', { class: 'lbl' }, 'Response', extra);
  if (tab.error) {
    const blocked = tab.error.code === 'out_of_scope';
    return clear(
      col,
      label(h('span', { class: 'r sc sc-x', text: blocked ? 'blocked by scope' : 'error' })),
      h('div', { class: 'rerr' }, h('b', { text: blocked ? 'Not sent. ' : 'Failed. ' }), tab.error.message),
    );
  }
  if (tab.cur == null) return clear(col, label(), h('pre', { class: 'raw muted', text: 'Press Send (⌘↵) to see the response here.' }));
  clear(col, label(), h('pre', { class: 'raw muted', text: 'Loading…' }));
  let ex;
  try {
    ex = await getExchange(tab.cur);
  } catch (e) {
    return clear(col, label(), h('div', { class: 'rerr', text: e.message }));
  }
  const pretty = tab.pretty !== false;
  const isJson = /json/.test(header(ex.resp_headers, 'content-type') || '');
  const enc = header(ex.resp_headers, 'content-encoding');
  clear(
    col,
    label(
      h(
        'span',
        { class: 'r' },
        enc ? h('span', { class: 'decodetag', text: 'decoded · ' + enc }) : null,
        ' ',
        isJson ? h('button', { class: 'link', text: pretty ? 'raw' : 'pretty', onclick: () => ((tab.pretty = !pretty), drawBenchResponse(tab, col)) }) : null,
        ' ',
        h('span', { class: statusClass(ex.status), text: ex.status == null ? 'no response' : ex.status }),
        ` · ${ex.duration_ms} ms · ${fmtSize(b64len(ex.resp_body))} · #${ex.id}`,
      ),
    ),
    h('div', { class: 'spotslot' }),
    rawPre(responseText(ex, pretty)),
  );
  const list = await loadInsights(ex);
  if (list && col.isConnected) drawInsights(col, list.filter((i) => i.side === 'response'));
}

function historyPanel(tab, main) {
  const rows = tab.history.map((e) =>
    h(
      'div',
      {
        class: 'histrow' + (e.id === tab.cur ? ' cur' : ''),
        onclick: () => {
          tab.cur = e.id;
          tab.error = null;
          saveBench();
          renderBench(main);
        },
      },
      h('input', {
        type: 'checkbox',
        title: 'Pick two to compare',
        checked: (tab.picks || []).includes(e.id),
        onclick: (ev) => {
          ev.stopPropagation();
          tab.picks = (tab.picks || []).filter((x) => x !== e.id);
          if (ev.target.checked) tab.picks = [...tab.picks, e.id].slice(-2);
          renderBench(main);
        },
      }),
      h('span', { class: 'hid', text: '#' + e.id }),
      h('span', { class: statusClass(e.status), text: e.status == null ? 'ERR' : e.status }),
      h('span', { class: 'hu', text: `${e.req.method} ${e.req.url}`, title: e.req.url }),
      h('span', { class: 'when', text: `${e.ms} ms · ${fmtSize(e.len)} · ${fmtTime(e.ts)}` }),
      h('button', {
        class: 'btn sm',
        text: 'Restore',
        title: 'Load this request back into the editor',
        onclick: (ev) => {
          ev.stopPropagation();
          Object.assign(tab, { method: e.req.method, url: e.req.url, raw: e.req.raw, cur: e.id, error: null });
          saveBench();
          renderBench(main);
        },
      }),
      h('button', {
        class: 'btn sm',
        text: 'Branch',
        title: 'Open this request in a new tab and keep this one as is',
        onclick: (ev) => {
          ev.stopPropagation();
          R.tabs.push({ name: tab.name.replace(/ ⎇\d+$/, '') + ' ⎇' + tabNo(), from: e.id, method: e.req.method, url: e.req.url, raw: e.req.raw, bodyB64: tab.bodyB64, history: [e], cur: e.id, picks: [] });
          R.active = R.tabs.length - 1;
          saveBench();
          renderBench(main);
        },
      }),
      h('button', {
        class: 'btn sm',
        text: 'Finding',
        title: 'Record a finding with this request as evidence',
        onclick: (ev) => {
          ev.stopPropagation();
          newFinding([e.id], tab.name);
        },
      }),
    ),
  );
  const picks = tab.picks || [];
  return h(
    'div',
    { class: 'hist' },
    h(
      'div',
      { class: 'histhead' },
      h('span', { class: 't', text: 'History' }),
      h('span', { class: 'hint', text: tab.history.length ? 'tick two sends to compare them side by side' : 'every send is kept here and in Traffic' }),
      h(
        'div',
        { class: 'r' },
        h('button', { class: 'btn sm', text: picks.length === 2 ? 'Compare ✓' : `Compare (${picks.length}/2)`, disabled: tab.history.length < 2, onclick: () => compareLatest(tab, main) }),
      ),
    ),
    h('div', { class: 'histrows' }, rows.length ? rows : h('div', { class: 'histrow muted', text: 'No sends yet.' })),
  );
}

function compareLatest(tab, main) {
  if ((tab.picks || []).length !== 2) tab.picks = tab.history.slice(0, 2).map((e) => e.id).reverse();
  R.scrollToCompare = true;
  renderBench(main);
}

async function drawCompare(tab) {
  const slot = $('#cmpslot');
  const picks = tab.picks || [];
  if (!slot || picks.length !== 2) return;
  let a;
  let b;
  try {
    [a, b] = await Promise.all(picks.map(getExchange));
  } catch (e) {
    return toast(e.message, 'err');
  }
  if (a.id > b.id) [a, b] = [b, a];
  const textOf = (ex) => (R.mode === 'request' ? asPlain({ lines: [`${ex.method} ${target(ex)} HTTP/1.1`, ...ex.req_headers.map(([k, v]) => `${k}: ${v}`)], body: bodyOf(ex.req_text, ex.req_body, true, header(ex.req_headers, 'content-type')) }) : asPlain(responseText(ex, true)));
  const rows = diffRows(textOf(a).split('\n'), textOf(b).split('\n'));
  const changed = rows.filter((r) => r.t !== 'eq' && r.t !== 'gap').length;
  const left = h('pre');
  const right = h('pre');
  for (const r of rows) {
    if (r.t === 'gap') {
      left.append(h('span', { class: 'dl gap', text: `… ${r.n} unchanged line${r.n === 1 ? '' : 's'}` }));
      right.append(h('span', { class: 'dl gap', text: `… ${r.n} unchanged line${r.n === 1 ? '' : 's'}` }));
      continue;
    }
    left.append(h('span', { class: 'dl' + (r.a != null && r.t !== 'eq' ? ' del' : ''), text: r.a == null ? '' : r.a }));
    right.append(h('span', { class: 'dl' + (r.b != null && r.t !== 'eq' ? ' add' : ''), text: r.b == null ? '' : r.b }));
  }
  const seg = h(
    'span',
    { class: 'seg' },
    ['response', 'request'].map((m) => h('button', { class: R.mode === m ? 'on' : '', text: m[0].toUpperCase() + m.slice(1), onclick: () => ((R.mode = m), drawCompare(tab)) })),
  );
  const head = (ex) => h('div', { class: 'cch' }, h('b', { text: '#' + ex.id }), h('span', { class: statusClass(ex.status), text: ex.status == null ? 'ERR' : ex.status }), `${ex.duration_ms} ms · ${fmtSize(b64len(ex.resp_body))}`);
  clear(
    slot,
    h(
      'div',
      { class: 'cmpview' },
      h(
        'div',
        { class: 'cvh' },
        `Compare #${a.id} ↔ #${b.id}`,
        h('span', { class: 'r' }, seg, h('button', { class: 'btn sm', text: 'Close', onclick: () => ((tab.picks = []), renderBench($('#main'))) })),
      ),
      h('div', { class: 'cmpsum', text: changed ? `${changed} line${changed === 1 ? '' : 's'} differ in the ${R.mode}.` : `The ${R.mode}s are identical.` }),
      h('div', { class: 'cmpcols' }, h('div', { class: 'cc' }, head(a), left), h('div', { class: 'cc' }, head(b), right)),
    ),
  );
  if (R.scrollToCompare) {
    R.scrollToCompare = false;
    slot.scrollIntoView({ behavior: 'smooth', block: 'start' });
  }
}

/** Line diff (LCS), aligned side by side, long unchanged runs folded. */
function diffRows(A, B) {
  let pre = 0;
  while (pre < A.length && pre < B.length && A[pre] === B[pre]) pre++;
  let suf = 0;
  while (suf < A.length - pre && suf < B.length - pre && A[A.length - 1 - suf] === B[B.length - 1 - suf]) suf++;
  const a = A.slice(pre, A.length - suf);
  const b = B.slice(pre, B.length - suf);
  const ops = [];
  for (let i = 0; i < pre; i++) ops.push({ t: 'eq', a: A[i], b: B[i] });
  if (a.length * b.length > 4e6) {
    a.forEach((x) => ops.push({ t: 'del', a: x }));
    b.forEach((x) => ops.push({ t: 'add', b: x }));
  } else {
    const n = a.length;
    const m = b.length;
    const L = new Uint32Array((n + 1) * (m + 1));
    for (let i = n - 1; i >= 0; i--)
      for (let j = m - 1; j >= 0; j--) L[i * (m + 1) + j] = a[i] === b[j] ? L[(i + 1) * (m + 1) + j + 1] + 1 : Math.max(L[(i + 1) * (m + 1) + j], L[i * (m + 1) + j + 1]);
    let i = 0;
    let j = 0;
    while (i < n && j < m) {
      if (a[i] === b[j]) ops.push({ t: 'eq', a: a[i++], b: b[j++] });
      else if (L[(i + 1) * (m + 1) + j] >= L[i * (m + 1) + j + 1]) ops.push({ t: 'del', a: a[i++] });
      else ops.push({ t: 'add', b: b[j++] });
    }
    while (i < n) ops.push({ t: 'del', a: a[i++] });
    while (j < m) ops.push({ t: 'add', b: b[j++] });
  }
  for (let i = A.length - suf; i < A.length; i++) ops.push({ t: 'eq', a: A[i], b: B[i - A.length + B.length] });

  // Pair runs of deletions with following additions so changed lines line up.
  const rows = [];
  for (let k = 0; k < ops.length; ) {
    if (ops[k].t === 'eq') {
      rows.push(ops[k++]);
      continue;
    }
    const dels = [];
    const adds = [];
    while (k < ops.length && ops[k].t !== 'eq') (ops[k].t === 'del' ? dels : adds).push(ops[k++]);
    for (let x = 0; x < Math.max(dels.length, adds.length); x++) rows.push({ t: 'chg', a: dels[x] ? dels[x].a : null, b: adds[x] ? adds[x].b : null });
  }
  // Fold unchanged runs longer than 6 lines, keeping 2 lines of context.
  const out = [];
  for (let k = 0; k < rows.length; ) {
    if (rows[k].t !== 'eq') {
      out.push(rows[k++]);
      continue;
    }
    let e = k;
    while (e < rows.length && rows[e].t === 'eq') e++;
    const run = rows.slice(k, e);
    const keepHead = k === 0 ? 0 : 2;
    const keepTail = e === rows.length ? 0 : 2;
    if (run.length > keepHead + keepTail + 2) {
      out.push(...run.slice(0, keepHead), { t: 'gap', n: run.length - keepHead - keepTail }, ...run.slice(run.length - keepTail));
    } else out.push(...run);
    k = e;
  }
  return out;
}

/* ======================================================================
   Scope
   ====================================================================== */

function renderScope(main) {
  clear(
    main,
    h(
      'div',
      { class: 'view' },
      h('div', { class: 'toolbar' }, h('h2', { text: 'Scope' }), h('span', { class: 'hint', text: 'Plonix learns which domains belong to your target as you browse. You decide.' })),
      h('div', { class: 'pane' }, h('div', { class: 'stack', id: 'scopebody' })),
    ),
  );
  renderScopeBody();
}

function renderScopeBody() {
  const box = $('#scopebody');
  if (!box) return;
  const domain = h('input', { type: 'text', placeholder: 'example.com or *.example.com', spellcheck: 'false' });
  const subs = h('input', { type: 'checkbox' });
  const add = async (action) => {
    const d = domain.value.trim();
    if (!d) return domain.focus();
    if (await decideDomain(action, d, subs.checked)) {
      domain.value = '';
      renderScopeBody();
    }
  };
  domain.addEventListener('keydown', (e) => e.key === 'Enter' && add('accept'));
  const sugg = stillPending(S.scope.suggestions);
  const rules = (S.scope.rules || []).slice().sort((x, y) => x.decision.localeCompare(y.decision) || x.pattern.localeCompare(y.pattern));
  clear(
    box,
    h(
      'div',
      { class: 'sechead' },
      h('h3', { text: `Suggested domains (${sugg.length})` }),
      sugg.length > 1
        ? h('span', { class: 'shacts' }, h('button', { class: 'btn sm', text: 'Accept all', title: 'Accept every suggested host on its own', onclick: () => decideAll('accept') }), h('button', { class: 'btn sm danger', text: 'Reject all', onclick: () => decideAll('reject') }))
        : null,
    ),
    sugg.length
      ? sugg.map((s) =>
          h(
            'div',
            { class: 'card sugg' },
            h(
              'div',
              { class: 'top' },
              h('span', { class: 'dom', text: s.domain }),
              h('span', { class: 'meta', text: `${s.requests} request${s.requests === 1 ? '' : 's'} · score ${s.score}` }),
              h(
                'span',
                { class: 'acts' },
                askButton({ kind: 'host', host: suggestionBase(s.domain) }, 'Ask whether this host belongs to your target'),
                h('button', { class: 'btn sm', text: 'Traffic', onclick: () => setQuery('host:' + s.domain) }),
                scopeButtons(s, 'sm', renderScopeBody),
              ),
            ),
            evidenceList(s.evidence),
          ),
        )
      : h('div', { class: 'card' }, h('div', { class: 'empty', text: 'Nothing to review. Suggestions appear when in-scope pages call, redirect to, link to or share a session with another domain.' })),
    h('div', { class: 'sechead' }, h('h3', { text: `Rules (${rules.length})` })),
    h(
      'div',
      { class: 'card' },
      h(
        'div',
        { class: 'addrule' },
        domain,
        h('label', null, subs, 'include subdomains'),
        h('button', { class: 'btn danger', text: 'Reject', onclick: () => add('reject') }),
        h('button', { class: 'btn primary', text: 'Accept', onclick: () => add('accept') }),
      ),
      rules.length
        ? h(
            'table',
            { class: 'grid' },
            h('thead', null, h('tr', null, h('th', { text: 'Domain' }), h('th', { text: 'Decision' }), h('th', { text: 'Note' }), h('th', { text: 'Since' }), h('th'))),
            h(
              'tbody',
              null,
              rules.map((r) =>
                h(
                  'tr',
                  null,
                  h('td', { class: 'mono', text: (r.include_subdomains ? '*.' : '') + r.pattern }),
                  h('td', null, h('span', { class: 'tag ' + scopeTag(r.decision), text: r.decision })),
                  h('td', { class: 'muted', text: r.note || '' }),
                  h('td', { class: 'muted', text: fmtDate(r.created_at) }),
                  h(
                    'td',
                    { style: { textAlign: 'right' } },
                    h('button', {
                      class: 'btn sm',
                      text: 'Remove',
                      onclick: async () => {
                        try {
                          await api('/api/scope/remove', { method: 'POST', body: { domain: r.pattern } });
                          toast('Removed the rule for ' + r.pattern);
                          await loadScope();
                          renderScopeBody();
                        } catch (e) {
                          toast(e.message, 'err');
                        }
                      },
                    }),
                  ),
                ),
              ),
            ),
          )
        : null,
    ),
  );
}

/* ======================================================================
   Map: hosts, endpoints, technologies
   ====================================================================== */

const M = { hosts: [], tech: {}, sel: null, dirty: true };

function renderMap(main) {
  clear(
    main,
    h(
      'div',
      { class: 'view' },
      h(
        'div',
        { class: 'toolbar' },
        h('h2', { text: 'Map' }),
        h('span', { class: 'hint', id: 'rulesinfo', text: 'Hosts, endpoints and parameters learned from traffic, with detected technologies.' }),
        h('button', { class: 'btn sm', text: 'Refresh', onclick: () => loadMap(true) }),
      ),
      h('div', { class: 'mapwrap' }, h('div', { class: 'hostlist', id: 'hostlist' }), h('div', { class: 'hostdetail', id: 'hostdetail' })),
    ),
  );
  loadMap(true);
}

async function loadMap(withTech) {
  try {
    M.hosts = await api('/api/hosts');
  } catch (e) {
    return toast(e.message, 'err');
  }
  M.dirty = false;
  if (!M.sel && M.hosts.length) M.sel = (M.hosts.find((x) => x.scope === 'accepted') || M.hosts[0]).host;
  drawHostList();
  drawHostDetail();
  if (withTech) {
    api('/api/rules')
      .then((r) => {
        const el = $('#rulesinfo');
        if (el) el.textContent = `Hosts → endpoints → parameters, plus ${r.rules} detection rules from ${r.packs.length} pack${r.packs.length === 1 ? '' : 's'}.`;
      })
      .catch(() => {});
    api('/api/tech')
      .then((all) => {
        M.tech = Object.fromEntries(all.map((x) => [x.host, x.tech]));
        drawHostList();
      })
      .catch(() => {});
  }
}

function drawHostList() {
  const box = $('#hostlist');
  if (!box) return;
  if (!M.hosts.length) return clear(box, h('div', { class: 'empty', text: 'No hosts yet.' }));
  clear(
    box,
    M.hosts.map((x) =>
      h(
        'div',
        { class: 'hostrow' + (x.host === M.sel ? ' on' : ''), onclick: () => ((M.sel = x.host), drawHostList(), drawHostDetail()) },
        h('div', { class: 'hn' }, h('span', { text: x.host, title: x.host })),
        h(
          'div',
          { class: 'hm' },
          h('span', { class: 'tag ' + scopeTag(x.scope), text: x.scope === 'unknown' ? 'out' : x.scope === 'accepted' ? 'in' : 'rejected' }),
          `${x.requests} req`,
          (M.tech[x.host] || []).slice(0, 3).map((t) => h('span', { class: 'techchip', text: t.name })),
        ),
      ),
    ),
  );
}

async function drawHostDetail() {
  const box = $('#hostdetail');
  if (!box || !M.sel) return box && clear(box);
  const host = M.sel;
  clear(box, h('div', { class: 'empty', text: 'Loading ' + host + '…' }));
  let eps;
  let tech;
  try {
    [eps, tech] = await Promise.all([api('/api/hosts/' + encodeURIComponent(host) + '/endpoints'), api('/api/tech/' + encodeURIComponent(host))]);
  } catch (e) {
    return clear(box, h('div', { class: 'rerr', text: e.message }));
  }
  if (M.sel !== host) return;
  const hs = M.hosts.find((x) => x.host === host) || {};
  const d = decide(host);
  clear(
    box,
    h(
      'div',
      { class: 'toolbar' },
      h('h2', { class: 'mono', text: host }),
      h('span', { class: 'tag ' + scopeTag(d), text: scopeLabel(d) }),
      h('span', { class: 'count', text: `${hs.requests || 0} requests · ${eps.length} endpoints` }),
      h('span', { class: 'hint' }),
      d !== 'accepted' ? h('button', { class: 'btn sm', text: 'Accept into scope', onclick: async () => (await decideDomain('accept', host)) && (await loadMap()) }) : null,
      h('button', { class: 'btn sm', text: 'Show traffic', onclick: () => setQuery('host:' + host) }),
      askButton({ kind: 'host', host }),
    ),
    h('div', { class: 'lbl', style: { padding: '12px 12px 0' }, text: `Technologies (${tech.tech.length})` }),
    tech.tech.length
      ? h(
          'div',
          { class: 'techgrid' },
          tech.tech.map((t) =>
            h(
              'div',
              { class: 'tech' },
              h('div', { class: 'tn' }, t.name, t.version ? h('span', { class: 'tv', text: t.version }) : null),
              h('div', { class: 'tc', text: `${t.category} · ${t.confidence}% · ${t.pack}` }),
              h('div', { class: 'te' }, t.implied_by ? 'implied by ' + t.implied_by : t.evidence, t.exchange_id ? [' ', h('button', { class: 'link', text: '#' + t.exchange_id, onclick: () => showExchange(t.exchange_id) })] : null),
              h('div', { class: 'conf' }, h('i', { style: { width: t.confidence + '%' } })),
            ),
          ),
        )
      : h('div', { class: 'muted', style: { padding: '8px 12px' }, text: 'No technologies detected yet.' }),
    h('div', { class: 'lbl', style: { padding: '12px 12px 6px' }, text: 'Endpoints' }),
    h(
      'table',
      { class: 'grid' },
      h('thead', null, h('tr', null, h('th', { text: 'Method' }), h('th', { text: 'Path' }), h('th', { text: 'Status' }), h('th', { text: 'Parameters' }), h('th', { class: 'num', text: 'Requests' }))),
      h(
        'tbody',
        null,
        eps.map((e) =>
          h(
            'tr',
            { class: 'click', title: 'Open a sample request', onclick: () => showExchange(e.sample_id) },
            h('td', null, h('span', { class: 'meth m-' + e.method, text: e.method })),
            h('td', { class: 'mono', text: e.path }),
            h('td', null, e.statuses.map((s) => [h('span', { class: statusClass(s), text: s }), ' '])),
            h('td', null, e.params.map((p) => h('span', { class: 'param', text: p }))),
            h('td', { class: 'num', text: e.requests }),
          ),
        ),
      ),
    ),
  );
}

/* ======================================================================
   Findings
   ====================================================================== */

function renderFindings(main) {
  clear(
    main,
    h(
      'div',
      { class: 'view' },
      h('div', { class: 'toolbar' }, h('h2', { text: 'Findings' }), h('span', { class: 'hint', text: 'Reproducible issues, each tied to the requests that prove it.' }), h('button', { class: 'btn primary sm', text: 'New finding', onclick: () => newFinding([], '') })),
      h('div', { class: 'pane' }, h('div', { class: 'stack', id: 'findbody' })),
    ),
  );
  loadFindings();
}

const SEV_ORDER = { critical: 0, high: 1, medium: 2, low: 3, info: 4 };

async function loadFindings() {
  let list;
  try {
    list = await api('/api/findings');
  } catch (e) {
    return toast(e.message, 'err');
  }
  S.findingsCount = list.length;
  updateChrome();
  const box = $('#findbody');
  if (!box) return;
  if (!list.length) {
    return clear(
      box,
      h('div', { class: 'card' }, h('div', { class: 'empty' }, h('h3', { text: 'No findings yet' }), 'Open a request in Traffic or on the Bench and choose ', h('b', { text: 'New finding' }), ' to record what you found with the request as evidence.')),
    );
  }
  list.sort((a, b) => (SEV_ORDER[a.severity] ?? 9) - (SEV_ORDER[b.severity] ?? 9) || b.created_at - a.created_at);
  clear(
    box,
    list.map((f) =>
      h(
        'div',
        { class: 'card finding' },
        h('div', { class: 'fh' }, h('span', { class: 'sev ' + f.severity, text: f.severity }), h('span', { class: 'ft', text: f.title }), h('span', { class: 'fmeta', text: `#${f.id} · ${f.status} · by ${f.created_by} · ${fmtDate(f.created_at)}` }), askButton({ kind: 'finding', id: f.id })),
        f.description || f.exchange_ids.length
          ? h(
              'div',
              { class: 'fb' },
              f.description || null,
              f.exchange_ids.length ? h('div', { class: 'evid' }, f.exchange_ids.map((id) => h('button', { text: 'request #' + id, title: 'Open in Traffic', onclick: () => showExchange(id) }))) : null,
            )
          : null,
      ),
    ),
  );
}

function newFinding(ids, title) {
  const t = h('input', { value: title || '', placeholder: 'e.g. IDOR on /v2/orders/{id} exposes other users’ addresses' });
  const sev = h('select', null, ['info', 'low', 'medium', 'high', 'critical'].map((s) => h('option', { value: s, text: s, selected: s === 'medium' })));
  const desc = h('textarea', { placeholder: 'What happens, how to reproduce it, and why it matters.' });
  const ex = h('input', { value: ids.join(', '), placeholder: 'Request ids, e.g. 14, 22' });
  const save = async () => {
    const exchange_ids = ex.value
      .split(/[\s,]+/)
      .filter(Boolean)
      .map((x) => Number(x.replace('#', '')));
    if (exchange_ids.some((n) => !Number.isInteger(n))) return (m.err.textContent = 'Request ids must be numbers.');
    if (!t.value.trim()) return (m.err.textContent = 'Give the finding a title.');
    try {
      const f = await api('/api/findings', { method: 'POST', body: { title: t.value.trim(), severity: sev.value, description: desc.value, exchange_ids } });
      closeModal();
      toast(`Finding #${f.id} recorded`, 'ok');
      S.findingsCount = (S.findingsCount || 0) + 1;
      updateChrome();
      if (S.view === 'findings') loadFindings();
    } catch (e) {
      m.err.textContent = e.message;
    }
  };
  const m = modal(
    'New finding',
    [h('label', null, 'Title', t), h('label', null, 'Severity', sev), h('label', null, 'Description', desc), h('label', null, 'Evidence (request ids)', ex)],
    [h('button', { class: 'btn', text: 'Cancel', onclick: closeModal }), h('button', { class: 'btn primary', text: 'Save finding', onclick: save })],
  );
  m.el.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) save();
  });
}

/* ======================================================================
   Agents
   ====================================================================== */

// An agent counts as connected while its MCP server checks in (every 20 s).
const AGENT_LIVE_MS = 60000;

const EXAMPLE_PROMPTS = [
  'Use Plonix to find in-scope API endpoints that returned errors, then read the most interesting request and tell me what stands out.',
  'Using Plonix, list every endpoint on the target that takes an id parameter and group them by host.',
  'Look at Plonix scope suggestions and explain which ones really belong to the target and why.',
  'Summarize my Plonix findings and point to the requests that prove each one.',
];

function renderAgents(main) {
  clear(
    main,
    h(
      'div',
      { class: 'view' },
      h('div', { class: 'toolbar' }, h('h2', { text: 'Agents' }), h('span', { class: 'hint', text: 'Let an AI agent such as Claude Code read this project over MCP. Read-only.' })),
      h('div', { class: 'pane' }, h('div', { class: 'stack', id: 'agentsbody' }, h('div', { class: 'muted', text: 'Loading…' }))),
    ),
  );
  loadAgents();
}

async function loadAgents() {
  let a;
  try {
    a = await api('/api/agents');
  } catch (e) {
    return toast(e.message, 'err');
  }
  S.agentsAt = Date.now();
  const box = $('#agentsbody');
  if (!box || S.view !== 'agents') return;
  const now = Date.now();
  const clients = a.clients || [];
  const live = clients.filter((c) => now - c.last_seen < AGENT_LIVE_MS);
  const ago = (ms) => {
    const s = Math.max(0, Math.round((now - ms) / 1000));
    return s < 60 ? 'just now' : s < 3600 ? Math.round(s / 60) + ' min ago' : fmtDate(ms);
  };
  const cmd = (a.connect && a.connect.command) || 'plonix connect claude';
  // The settings card keeps its own state across the status refreshes.
  let settingsCard = $('#agentsettings');
  const fresh = !settingsCard;
  if (fresh) settingsCard = h('div', { class: 'card', id: 'agentsettings' });
  clear(
    box,
    h(
      'div',
      { class: 'card agentstatus' },
      h(
        'div',
        { class: 'ah' },
        h('span', { class: 'adot' + (live.length ? ' on' : '') }),
        h('b', { text: live.length ? (live.length === 1 ? '1 agent connected' : live.length + ' agents connected') : 'No agent connected' }),
        h('span', { class: 'mode', text: 'Read-only' }),
      ),
      clients.length
        ? h(
            'table',
            { class: 'grid' },
            h('tr', null, h('th', { text: 'Agent' }), h('th', { text: 'Status' }), h('th', { text: 'Requests' }), h('th', { text: 'Last request' })),
            clients.map((c) =>
              h(
                'tr',
                null,
                h('td', { class: 'mono', text: c.name }),
                h('td', { text: now - c.last_seen < AGENT_LIVE_MS ? 'connected' : 'last seen ' + ago(c.last_seen) }),
                h('td', { text: c.requests + (c.refused ? ` (${c.refused} refused)` : '') }),
                h('td', { class: 'mono muted', text: c.last_request }),
              ),
            ),
          )
        : h('div', { class: 'ab muted', text: 'When an agent starts the Plonix MCP server it shows up here, with every request it makes.' }),
    ),
    h('div', { class: 'sechead' }, h('h3', { text: 'Claude Code settings' })),
    settingsCard,
    h('div', { class: 'sechead' }, h('h3', { text: 'Connect Claude Code' })),
    h(
      'div',
      { class: 'card' },
      h(
        'div',
        { class: 'ab' },
        h('p', null, 'Run this once in a terminal. It adds Plonix to Claude Code for all your projects:'),
        h('div', { class: 'cmdline' }, h('code', { text: cmd }), h('button', { class: 'btn sm', text: 'Copy', onclick: () => copyText(cmd) })),
        h(
          'p',
          { class: 'muted' },
          'Any other MCP client can run ',
          h('code', { text: 'plonix mcp' }),
          ' as a stdio server. The ',
          h('code', { text: 'plonix' }),
          ' command comes from ',
          h('code', { text: 'cargo install --path crates/plonix-cli' }),
          '. The agent reads whichever engine is running, including this one.',
        ),
      ),
    ),
    h('div', { class: 'sechead' }, h('h3', { text: 'What agents can do' })),
    h(
      'div',
      { class: 'card capgrid' },
      h('div', null, h('div', { class: 'caph ok', text: '✓ Allowed' }), [...new Set((a.capabilities || []).filter((c) => c.path !== '/api/agents').map((c) => c.what))].map((t) => h('div', { class: 'cap', text: t }))),
      h('div', null, h('div', { class: 'caph no', text: '✗ Not allowed' }), (a.not_allowed || []).map((t) => h('div', { class: 'cap', text: t }))),
    ),
    h(
      'p',
      { class: 'muted fine' },
      'The engine enforces this: agents sign in with their own token, and anything outside this list is refused. Captured traffic never leaves this Mac through Plonix, but it can hold passwords and session tokens, so connect only agents you trust.',
    ),
    h('div', { class: 'sechead' }, h('h3', { text: 'Try asking' })),
    h(
      'div',
      { class: 'card' },
      EXAMPLE_PROMPTS.map((p) => h('div', { class: 'prompt' }, h('span', { text: '“' + p + '”' }), h('button', { class: 'btn sm ghost', text: 'Copy', onclick: () => copyText(p) }))),
    ),
  );
  if (fresh) renderAgentSettings(settingsCard);
}

/* ======================================================================
   Ask Claude Code: hand the agent the context for one spot in the app
   ====================================================================== */

async function loadAgentSettings() {
  try {
    S.agentSettings = await api('/api/agents/settings');
  } catch (_) {
    S.agentSettings = null;
  }
  for (const b of document.querySelectorAll('.askbtn')) b.hidden = !agentsOn();
}

const agentsOn = () => !S.agentSettings || S.agentSettings.settings.enabled;

/** A small "Ask Claude" button for a request, finding or host. */
function askButton(subject, title) {
  return h('button', {
    class: 'btn sm askbtn',
    hidden: !agentsOn(),
    title: title || 'Ask Claude Code about this, with just this context',
    onclick: (e) => {
      e.stopPropagation();
      askClaude(subject);
    },
  }, h('span', { class: 'askico', text: '✦' }), ' Ask Claude');
}

const fmtTok = (n) => (n >= 1000 ? (n / 1000).toFixed(n >= 10000 ? 0 : 1) + 'k' : String(n));

/**
 * The Ask sheet: shows exactly what will be shared and how big it is, lets
 * the user edit the question, drop parts and shorten bodies, and asks for
 * an explicit confirmation when it is larger than their limit.
 */
async function askClaude(subject) {
  const st = { exclude: [], question: null, max: null, bundle: null, confirmBig: false };
  const q = h('textarea', { class: 'askq', rows: 3 });
  const partsBox = h('div', { class: 'askparts' });
  const meter = h('div', { class: 'askmeter' });
  const warn = h('div', { class: 'askwarn', hidden: true });
  const clipSel = h('select', { title: 'Each request and response body is clipped to this length' }, [1000, 2000, 4000, 8000, 16000, 50000].map((n) => h('option', { value: n, text: fmtTok(n) + ' chars' })));
  const copyBtn = h('button', { class: 'btn', text: 'Copy prompt' });
  const openBtn = h('button', { class: 'btn primary', text: 'Open in Claude Code' });
  let timer;
  const rebuild = async () => {
    try {
      st.bundle = await api('/api/agents/ask', { method: 'POST', body: { ...subject, question: st.question, exclude: st.exclude, max_body_chars: st.max } });
    } catch (e) {
      m.err.textContent = e.message;
      return;
    }
    m.err.textContent = '';
    draw();
  };
  const later = () => {
    clearTimeout(timer);
    timer = setTimeout(rebuild, 350);
  };
  const draw = () => {
    const b = st.bundle;
    if (st.question == null) q.value = b.question;
    clipSel.value = String(b.max_body_chars);
    if (![...clipSel.options].some((o) => o.value === String(b.max_body_chars))) clipSel.append(h('option', { value: b.max_body_chars, text: fmtTok(b.max_body_chars) + ' chars', selected: true }));
    clear(
      partsBox,
      b.parts.map((p) => {
        const box = h('input', {
          type: 'checkbox',
          checked: p.included,
          onchange: () => {
            st.exclude = box.checked ? st.exclude.filter((x) => x !== p.id) : [...st.exclude, p.id];
            st.confirmBig = false;
            rebuild();
          },
        });
        const pre = h('pre', { class: 'askpre', hidden: true, text: p.text });
        return h(
          'div',
          { class: 'askpart' + (p.included ? '' : ' off') },
          h('label', null, box, h('span', { class: 'pl', text: p.label }), p.clipped ? h('span', { class: 'clipped', text: 'clipped' }) : null, h('span', { class: 'pt', text: '~' + fmtTok(p.tokens) + ' tokens' })),
          h('button', { class: 'link', text: 'preview', onclick: () => (pre.hidden = !pre.hidden) }),
          pre,
        );
      }),
    );
    const pct = Math.min(100, Math.round((b.tokens / b.budget) * 100));
    clear(meter, h('div', { class: 'bar' + (b.over_budget ? ' over' : '') }, h('i', { style: { width: pct + '%' } })), h('span', { text: `~${fmtTok(b.tokens)} of your ${fmtTok(b.budget)}-token limit` }));
    warn.hidden = !b.over_budget;
    if (b.over_budget) {
      const ok = h('input', { type: 'checkbox', checked: st.confirmBig, onchange: () => ((st.confirmBig = ok.checked), sync()) });
      clear(
        warn,
        h('b', { text: `This is about ${fmtTok(b.tokens)} tokens, over your limit of ${fmtTok(b.budget)}.` }),
        ' A large context makes answers slower and less focused. Untick parts or shorten the bodies, or ',
        h('label', null, ok, ' send it anyway'),
        '. The limit is in Agents → Claude Code.',
      );
    }
    sync();
  };
  const sync = () => {
    const blocked = !st.bundle || (st.bundle.over_budget && !st.confirmBig);
    openBtn.disabled = blocked;
    copyBtn.disabled = blocked;
  };
  q.addEventListener('input', () => {
    st.question = q.value;
    later();
  });
  clipSel.addEventListener('change', () => {
    st.max = Number(clipSel.value);
    st.confirmBig = false;
    rebuild();
  });
  copyBtn.onclick = async () => {
    await copyText(st.bundle.prompt);
    closeModal();
  };
  openBtn.onclick = async () => {
    try {
      await api('/api/agents/launch', { method: 'POST', body: { prompt: st.bundle.prompt } });
      closeModal();
      toast('Opened Claude Code in Terminal', 'ok');
    } catch (e) {
      if (e.code === 'unsupported') {
        await copyText(st.bundle.prompt);
        m.err.textContent = 'Opening a terminal works on macOS only. The prompt is on your clipboard: paste it into Claude Code.';
      } else m.err.textContent = e.message;
    }
  };
  const m = modal(
    'Ask Claude Code',
    [
      h('label', null, 'Your question', q),
      h('div', { class: 'askhead' }, h('span', { text: 'What Claude Code gets' }), h('label', { class: 'askclip' }, 'Bodies up to ', clipSel)),
      partsBox,
      meter,
      warn,
      h('p', { class: 'muted fine', text: 'Only what is ticked is sent, straight from this Mac to Claude Code. It may include passwords or session tokens from captured traffic.' }),
    ],
    [h('button', { class: 'btn', text: 'Cancel', onclick: closeModal }), copyBtn, openBtn],
  );
  m.el.querySelector('.mcard').classList.add('wide');
  await rebuild();
}

/** Claude Code settings: on or off, what it may see, and how much context to hand over. */
async function renderAgentSettings(box) {
  let cfg;
  try {
    cfg = await api('/api/agents/settings');
  } catch (e) {
    return clear(box, h('div', { class: 'ab rerr', text: e.message }));
  }
  S.agentSettings = cfg;
  const st = cfg.settings;
  const save = async (patch) => {
    try {
      S.agentSettings = await api('/api/agents/settings', { method: 'PUT', body: { ...S.agentSettings.settings, ...patch } });
      toast('Saved', 'ok');
    } catch (e) {
      toast(e.message, 'err');
    }
    for (const b of document.querySelectorAll('.askbtn')) b.hidden = !agentsOn();
    renderAgentSettings(box);
  };
  const on = h('input', { type: 'checkbox', checked: st.enabled, onchange: () => save({ enabled: on.checked }) });
  const radio = (value, label, note) =>
    h('label', { class: 'opt' }, h('input', { type: 'radio', name: 'agentdata', checked: st.data === value, disabled: !st.enabled, onchange: () => save({ data: value }) }), h('span', null, h('b', { text: label }), h('small', { text: note })));
  const budget = h('select', { disabled: !st.enabled, onchange: () => save({ context_budget: Number(budget.value) }) }, cfg.budgets.map((n) => h('option', { value: n, text: `${fmtTok(n)} tokens`, selected: n === st.context_budget })));
  const clip = h('select', { disabled: !st.enabled, onchange: () => save({ max_body_chars: Number(clip.value) }) }, [1000, 2000, 4000, 8000, 16000].map((n) => h('option', { value: n, text: `${fmtTok(n)} characters`, selected: n === st.max_body_chars })));
  clear(
    box,
    h('div', { class: 'ab setrow' }, h('label', { class: 'switch' }, on, h('span', { text: st.enabled ? 'Claude Code and other agents can read this project' : 'Agent access is off: every agent request is refused' })), h('span', { class: 'mode', text: 'Read-only' })),
    h(
      'div',
      { class: 'setgrid' + (st.enabled ? '' : ' disabled') },
      h('div', null, h('div', { class: 'caph', text: 'What agents can see' }), radio('in_scope', 'In-scope hosts only', 'Requests, hosts and technologies for hosts you accepted into scope'), radio('all', 'Everything captured', 'Also third-party and out-of-scope traffic')),
      h(
        'div',
        null,
        h('div', { class: 'caph', text: 'Tools agents get' }),
        cfg.groups.map((g) => {
          const c = h('input', { type: 'checkbox', checked: g.on, disabled: !st.enabled, onchange: () => save({ off: c.checked ? st.off.filter((x) => x !== g.group) : [...st.off, g.group] }) });
          return h('label', { class: 'opt' }, c, h('span', { text: g.label }));
        }),
      ),
      h(
        'div',
        null,
        h('div', { class: 'caph', text: 'Ask Claude' }),
        h('label', { class: 'opt col' }, h('span', { text: 'Warn me before sending more than' }), budget),
        h('label', { class: 'opt col' }, h('span', { text: 'Clip each request and response body to' }), clip),
      ),
    ),
  );
}

/* ---------- keyboard ---------- */

document.addEventListener('keydown', (e) => {
  if (!S.token || !$('#main')) return;
  const typing = /^(INPUT|TEXTAREA|SELECT)$/.test(document.activeElement && document.activeElement.tagName);
  if (e.key === 'Escape') {
    if ($('.popover, .ctxmenu')) return closePopover();
    if ($('.modal')) return closeModal();
    if (S.view === 'traffic' && !typing) return closeInspector();
  }
  if (S.view === 'bench' && e.key === 'Enter' && (e.metaKey || e.ctrlKey) && R.send) {
    e.preventDefault();
    return R.send();
  }
  if (typing || $('.modal') || e.metaKey || e.ctrlKey || e.altKey) {
    if (S.view === 'traffic' && (e.metaKey || e.ctrlKey) && e.key === 'f') {
      e.preventDefault();
      $('#q').focus();
    }
    return;
  }
  const keys = Object.keys(VIEWS);
  if (/^[1-6]$/.test(e.key)) return go(keys[Number(e.key) - 1]);
  if (e.key === '\\') return toggleSidebar();
  if (S.view !== 'traffic') return;
  if (e.key === '/') {
    e.preventDefault();
    $('#q').focus();
  } else if (e.key === 'ArrowDown' || e.key === 'j') {
    e.preventDefault();
    selectRow(1);
  } else if (e.key === 'ArrowUp' || e.key === 'k') {
    e.preventDefault();
    selectRow(-1);
  } else if (e.key === 'b' && T.sel) sendToBench(T.sel);
});

// Entry points for the Plonix app's menu bar.
window.plonix = {
  go: (view) => S.token && $('#main') && go(view),
  openTarget: () => S.token && $('#main') && openTarget(),
  toggleSidebar: () => S.token && $('#main') && toggleSidebar(),
};

boot();
