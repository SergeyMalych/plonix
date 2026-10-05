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

/* State kept in the browser for one project (search, Bench tabs, last
 * target) is keyed by the project's id, so it stays with the project even
 * when another project later opens at the same address. */
const PROJECT_ID = (document.querySelector('meta[name="plonix-project"]') || {}).content || '';
const pkey = (key) => (PROJECT_ID && !PROJECT_ID.includes('{') ? `${key}@${PROJECT_ID}` : key);
const pstore = (key, value) => store(pkey(key), value);

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
  constructor(status, code, message, data) {
    super(message);
    this.status = status;
    this.code = code;
    this.problems = data && data.problems;
    this.data = data;
  }
}

const S = {
  token: null,
  railCollapsed: store('plonix.rail') === 'collapsed',
  status: null,
  engineUp: true,
  view: 'traffic',
  scope: { rules: [], suggestions: [] },
  /** Named filters (is:id) from filter packs, for the filter builder and chips. */
  named: [],
  exclusions: { groups: [], asked: true },
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
  if (!resp.ok) throw new ApiError(resp.status, (data && data.code) || 'error', (data && data.error) || resp.statusText, data);
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
  if (S.status.demo) await loadDemoBench();
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

/* ---------- the demo project ---------- */

/** The demo ships its Bench experiments with the project; they are used
 * until this window has Bench tabs of its own. */
async function loadDemoBench() {
  if (pstore('plonix.bench')) return;
  try {
    const seed = await api('/api/views/bench');
    if (Array.isArray(seed.tabs) && seed.tabs.length) {
      R.tabs = seed.tabs;
      R.active = seed.active || 0;
      saveBench();
    }
  } catch (_) {}
}

/** A strip with a short tour of what the demo shows. */
function demoBar() {
  const lensSample = async () => {
    try {
      const r = await api('/api/traffic?limit=1&q=' + encodeURIComponent('path:/v1/orders/1042 mime:json'));
      if (r.items.length) return showExchange(r.items[0].id);
    } catch (_) {}
    go('traffic');
  };
  const bar = h(
    'div',
    { class: 'demobar' },
    h('span', null, h('b', { text: 'Demo project. ' }), 'Traffic from Brightcart, a made-up shop, captured ahead of time. Its hosts are not real, so nothing here reaches the internet. Try:'),
    h(
      'span',
      { class: 'tour' },
      h('button', { class: 'btn sm', text: 'What Lens spots', title: 'An order with a token, an email and a card number in it', onclick: lensSample }),
      h('button', { class: 'btn sm', text: 'Scope suggestions', title: 'Hosts tied to the shop, with the evidence for each', onclick: () => go('scope') }),
      h('button', { class: 'btn sm', text: 'Filters', title: 'Ready-made include and exclude filters, and how to write your own', onclick: filterTour }),
      h('button', { class: 'btn sm', text: 'Bench experiment', title: 'Order lookup: two sends ready to compare', onclick: () => go('bench') }),
      h('button', { class: 'btn sm', text: 'Findings', onclick: () => go('findings') }),
      h('button', { class: 'btn sm', text: 'Scans', title: 'Which checks fit this app, and why', onclick: () => go('scans') }),
    ),
    h('button', {
      class: 'iconbtn x',
      title: 'Hide the tour',
      text: '✕',
      onclick: () => {
        pstore('plonix.demoTourClosed', true);
        bar.remove();
      },
    }),
  );
  return bar;
}

/** The demo's filters overview: views of its traffic, each one click away,
 * and the search language at a glance. */
async function filterTour() {
  let views = [];
  try {
    views = (await api('/api/views/filter_tour')).views || [];
  } catch (_) {}
  const chip = (f) => {
    const { key, value } = filterLabel(f.term);
    return h('span', { class: 'fchip ' + f.mode }, h('span', { class: 'fbody' }, h('span', { class: 'fmode', text: f.mode === 'include' ? '+' : '−' }), key ? h('span', { class: 'fk', text: key }) : null, h('span', { class: 'fv', text: value })), h('span', { class: 'fpad' }));
  };
  const apply = (v) => {
    closeModal();
    T.filters = v.filters.map((f) => ({ ...f }));
    T.text = '';
    T.viewLoaded = true;
    if (S.view === 'traffic') {
      const q = $('#q');
      if (q) q.value = '';
      filtersChanged();
    } else {
      saveTrafficView();
      go('traffic');
    }
    toast(v.title + ': ' + v.why);
  };
  const rows = views.map((v) => {
    const n = h('span', { class: 'n muted' });
    api('/api/traffic?limit=0&q=' + encodeURIComponent(queryFor(v.filters, '')))
      .then((r) => (n.textContent = r.total + ' requests'))
      .catch(() => {});
    return h(
      'div',
      { class: 'ftour-row' },
      h('div', { class: 'ftour-main' }, h('b', { text: v.title }), h('span', { class: 'muted', text: v.why }), h('span', { class: 'fgroup' }, v.filters.map(chip))),
      n,
      h('button', { class: 'btn sm', text: 'Apply', onclick: () => apply(v) }),
    );
  });
  const lang = [
    ['host:api.example.com', 'a host and its subdomains (globs: host:*.cdn.*)'],
    ['-host:a.com,b.com', 'a leading minus hides; a comma list matches any value'],
    ['status:4xx,5xx', 'status classes or codes; status:none for no response'],
    ['method:POST', 'request method'],
    ['path:/api', 'path prefix (globs: path:*admin*)'],
    ['ext:js  mime:json', 'file extension, response type'],
    ['kind:static', 'images, fonts, styles, scripts and media'],
    ['scope:in  source:replay', 'in scope or not; captured or sent from the Bench'],
    ['is:auth', 'a named filter from a filter pack (see + Filter)'],
    ['"set-cookie: sid"', 'anything else is full text: URLs, headers, bodies'],
  ];
  const m = modal(
    'Filters',
    [
      h('p', { class: 'muted', text: 'Filters narrow the traffic list. Each is a chip: + chips show only what matches, − chips hide it. Click a chip to flip it. Add them with + Filter, from the suggested chips under the search box, from a row’s right-click menu, or by typing a term such as host:api.example.com and pressing Enter. They are saved with the project.' }),
      h('div', { class: 'ftour' }, rows.length ? rows : h('p', { class: 'muted', text: 'No examples in this project.' })),
      h('h4', { class: 'ftour-h', text: 'The search language' }),
      h('div', { class: 'ftour-lang' }, lang.map(([q, what]) => [h('code', { text: q }), h('span', { class: 'muted', text: what })])),
    ],
    [h('button', { class: 'btn', text: 'Clear filters', onclick: () => (closeModal(), clearFilters(), go('traffic')) }), h('button', { class: 'btn primary', text: 'Done', onclick: closeModal })],
  );
  m.el.querySelector('.mcard').classList.add('wide');
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
  access: { label: 'Access', ico: '⚿', render: renderAccess, tool: 'access-check' },
  findings: { label: 'Findings', ico: '⚑', render: renderFindings },
  agents: { label: 'Agents', ico: '✦', render: renderAgents },
  market: { label: 'Market', ico: '⬢', render: renderMarket },
  scans: { label: 'Scans', ico: '⌖', render: renderScans },
  settings: { label: 'Settings', ico: '⚙', render: renderSettings, footer: true },
};

const IN_APP = !!window.__PLONIX_APP__;

/** Whether a built-in tool has been switched on from the Market. */
const toolOn = (id) => !!(S.status && S.status.tools && S.status.tools.includes(id));

function renderShell() {
  const nav = h('div', { class: 'nav' });
  Object.entries(VIEWS).forEach(([key, v], i) => {
    if (v.footer) return;
    if (v.tool && !toolOn(v.tool)) return;
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
    S.status.demo && !pstore('plonix.demoTourClosed') ? demoBar() : null,
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
          'div',
          { class: 'nav navfoot' },
          h(
            'button',
            { 'data-v': 'settings', title: `Settings: proxy, storage and more${IN_APP ? '  (⌘,)' : ''}`, onclick: () => go('settings') },
            h('span', { class: 'ico', text: '⚙' }),
            h('span', { class: 'nl', text: 'Settings' }),
          ),
        ),
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
  const input = h('input', { placeholder: 'example.com', spellcheck: 'false', autocomplete: 'off', value: pstore('plonix.lastTarget') || '' });
  const go = async () => {
    const target = input.value.trim();
    if (!target) return input.focus();
    btn.disabled = true;
    m.err.textContent = '';
    const r = await launchTarget(target);
    btn.disabled = false;
    // launchTarget may have replaced this dialog with one of its own.
    if (!m.el.isConnected) return;
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
    pstore('plonix.lastTarget', target);
    toast(`Opened ${r.url} in ${r.browser}. Browse the site; requests appear in Traffic.`, 'ok');
    await loadScope();
    if (S.view !== 'traffic') go('traffic');
    if (r.needs_trust) trustCertificate(r.browser, r.can_trust);
    return { ok: true };
  } catch (e) {
    if (e.code === 'no_browser' && e.data && e.data.can_install) {
      getPlonixBrowser(target);
      return { ok: false, message: '' };
    }
    return { ok: false, message: e.message };
  }
}

const megabytes = (n) => (n / 1048576).toFixed(0);

/** No browser to launch: offers the Plonix browser (Chromium, downloaded once), then opens the target in it. */
function getPlonixBrowser(target) {
  const fill = h('div', { class: 'pbar-fill' });
  const bar = h('div', { class: 'pbar', hidden: true }, fill);
  const line = h('div', { class: 'muted fine' });
  const btn = h('button', { class: 'btn primary', text: 'Download', onclick: () => start() });
  const m = modal(
    'Get the Plonix browser',
    [
      h('p', { class: 'mnote', text: 'No Chrome, Brave, Edge or Firefox was found on this computer. Plonix can download its own browser: Chromium, about 150 MB, once.' }),
      h('p', {
        class: 'muted mnote',
        text: 'It is kept with your Plonix data, opens with a profile of its own and trusts the Plonix certificate, so HTTPS sites work right away. Your everyday browser is not touched.',
      }),
      bar,
      line,
    ],
    [h('button', { class: 'btn', text: 'Close', onclick: closeModal }), btn],
  );
  const stages = { looking: 'Finding the current Chromium build…', unpacking: 'Unpacking…', verifying: 'Checking that it starts…' };
  const show = (p) => {
    bar.hidden = false;
    const pct = p.total ? Math.min(100, (p.received * 100) / p.total) : p.stage === 'done' ? 100 : 0;
    fill.style.width = pct + '%';
    bar.classList.toggle('busy', !['downloading', 'done'].includes(p.stage));
    line.textContent =
      p.stage === 'downloading'
        ? `Downloading Chromium ${p.version} · ${megabytes(p.received)}${p.total ? ' of ' + megabytes(p.total) : ''} MB`
        : stages[p.stage] || '';
  };
  const follow = async () => {
    while (m.el.isConnected) {
      let st;
      try {
        st = await api('/api/browser');
      } catch (e) {
        m.err.textContent = e.message;
        btn.disabled = false;
        return;
      }
      const p = st.install || {};
      if (p.stage === 'failed') {
        bar.hidden = true;
        line.textContent = '';
        m.err.textContent = 'Could not get the Plonix browser: ' + (p.error || 'unknown error');
        btn.textContent = 'Try again';
        btn.disabled = false;
        return;
      }
      if (p.stage === 'done') {
        show(p);
        line.textContent = 'Ready. Opening ' + target + '…';
        const r = await launchTarget(target);
        if (!m.el.isConnected) return;
        if (r.ok) closeModal();
        else m.err.textContent = r.message;
        return;
      }
      show(p);
      await new Promise((ok) => setTimeout(ok, 400));
    }
  };
  const start = async () => {
    btn.disabled = true;
    m.err.textContent = '';
    try {
      await api('/api/browser/install', { method: 'POST' });
    } catch (e) {
      btn.disabled = false;
      m.err.textContent = e.message;
      return;
    }
    follow();
  };
  // A download started earlier keeps going in the background; pick it up.
  api('/api/browser')
    .then((st) => {
      if (st.install && ['looking', 'downloading', 'unpacking', 'verifying'].includes(st.install.stage)) {
        btn.disabled = true;
        follow();
      }
    })
    .catch(() => {});
}

/** Firefox checks the certificates the system trusts: offers to trust the Plonix CA, once. */
function trustCertificate(browser, canTrust) {
  if (!canTrust) {
    toast(`${browser} needs the Plonix certificate for HTTPS: import ca.pem from your Plonix folder in its certificate settings (plonix ca shows where).`, 'err');
    return;
  }
  const btn = h('button', {
    class: 'btn primary',
    text: 'Trust certificate',
    onclick: async () => {
      btn.disabled = true;
      btn.textContent = 'Waiting for macOS…';
      m.err.textContent = '';
      try {
        await api('/api/ca/trust', { method: 'POST' });
        closeModal();
        toast(`Certificate trusted. Reload the page in ${browser}; HTTPS now goes through Plonix.`, 'ok');
      } catch (e) {
        btn.disabled = false;
        btn.textContent = 'Try again';
        m.err.textContent = e.message;
      }
    },
  });
  const m = modal(
    'Trust the Plonix certificate',
    [
      h('p', { class: 'mnote', text: `${browser} checks the certificates your Mac trusts, so HTTPS sites show a warning until the Plonix certificate is trusted.` }),
      h('p', {
        class: 'muted mnote',
        text: 'Plonix adds it to your login keychain; macOS asks for your password or Touch ID. The certificate was made on this Mac and its key never leaves it. Remove it any time in Keychain Access ("Plonix CA").',
      }),
    ],
    [h('button', { class: 'btn', text: 'Not now', onclick: closeModal }), btn],
  );
}

function go(view, force) {
  if (!VIEWS[view]) return;
  if (VIEWS[view].tool && !toolOn(VIEWS[view].tool)) return go('traffic', force);
  if (S.view === view && !force) return;
  // Going somewhere from the sidebar forgets the way back; leaveTo keeps it.
  if (!S.keepBack) S.back = null;
  S.keepBack = false;
  if (S.view === 'map') saveMapScroll();
  S.view = view;
  if (location.hash !== '#/' + view) history.replaceState(null, '', '#/' + view);
  for (const b of document.querySelectorAll('.nav button')) b.classList.toggle('on', b.dataset.v === view);
  const main = $('#main');
  clear(main);
  VIEWS[view].render(main);
  // Counted for anonymous usage statistics, when they are on: the screen's name only.
  api('/api/usage', { method: 'POST', body: { event: 'screen_' + view } }).catch(() => {});
}

/** Moves to another screen from inside one, remembering where to go back to. */
function leaveTo(view) {
  if (S.view !== view && VIEWS[S.view]) S.back = S.view;
  S.keepBack = true;
  go(view, true);
}

function goBack() {
  const back = S.back;
  S.back = null;
  if (back) go(back, true);
}

/** "← Map": returns to the screen the user came from, exactly as they left it. */
function backButton() {
  if (!S.back || !VIEWS[S.back]) return null;
  return h('button', { class: 'btn sm backbtn', text: '← ' + VIEWS[S.back].label, title: 'Back to ' + VIEWS[S.back].label, onclick: goBack });
}

function updateChrome() {
  const st = S.status;
  if (!st || !$('#engine')) return;
  $('#proj').textContent = '· ' + st.project;
  $('#proj').title = st.project_dir ? 'Project folder: ' + st.project_dir : '';
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
  const held = (st.intercept && st.intercept.held) || 0;
  const ctTraffic = $('#ct-traffic');
  ctTraffic.textContent = held || st.exchanges || '';
  ctTraffic.classList.toggle('hot', held > 0);
  ctTraffic.title = held ? held + ' held in Intercept, waiting for you' : '';
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
    if (st.intercept && st.intercept.seq !== IC.seq) loadIntercept();
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
    if (!S.namedAt || Date.now() - S.namedAt > 15000) {
      S.named = (await api('/api/filters')).filters || [];
      S.namedAt = Date.now();
    }
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
  try {
    S.exclusions = await api('/api/scope/exclusions');
  } catch (_) {}
  updateChrome();
  renderRail();
  if (S.view === 'traffic') renderBanner();
  if (S.view === 'bench') renderScopeHint();
  maybeAskExclusions();
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

/** Says how much of a body was kept, when it was longer than the recording limit. */
function cutTag(truncated, size, b64) {
  if (!truncated) return null;
  const kept = fmtSize(b64len(b64));
  return h('span', {
    class: 'decodetag',
    text: size != null ? `first ${kept} of ${fmtSize(size)} kept` : `first ${kept} kept`,
    title: 'This body was longer than Settings > Proxy > Keep bodies up to. It went through in full; Plonix kept the start.',
  });
}

function requestText(ex) {
  const lines = [`${ex.method} ${target(ex)} ${ex.http_version || 'HTTP/1.1'}`, ...ex.req_headers.map(([k, v]) => `${k}: ${v}`)];
  const b = bodyOf(ex.req_text, ex.req_body, false);
  return { lines, body: b };
}

function responseText(ex, pretty) {
  if (ex.status == null) return { lines: [], body: { note: ex.error ? 'No response: ' + ex.error : 'No response' } };
  const lines = [`${ex.http_version || 'HTTP/1.1'} ${ex.status}`, ...ex.resp_headers.map(([k, v]) => `${k}: ${v}`)];
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

const T = { text: '', filters: [], items: [], total: 0, sel: null, live: true, maxId: 0, pretty: true, inspH: store('plonix.inspH'), picked: new Set(), pickAnchor: null, group: !!store('plonix.groupAlike'), open: new Set(), visible: null };

/** A path with its ids folded to {id}, the same way the Map groups endpoints. */
function foldPath(path) {
  return (path || '/')
    .split('/')
    .map((seg) => (/^\d+$/.test(seg) || (seg.length >= 16 && /^[0-9a-f-]+$/i.test(seg)) ? '{id}' : seg))
    .join('/');
}

const alikeKey = (ex) => `${ex.method} ${ex.host}:${ex.port} ${foldPath(ex.path)}`;

/** Look-alike requests in the current page: same method, host and path once ids are folded. */
function alikeGroups(items) {
  const groups = new Map();
  for (const ex of items) {
    const k = alikeKey(ex);
    if (!groups.has(k)) groups.set(k, []);
    groups.get(k).push(ex);
  }
  return groups;
}

function setGrouping(on) {
  T.group = on;
  T.open.clear();
  store('plonix.groupAlike', on || null);
  renderChips();
  drawRows(Infinity);
}

/* ---------- include / exclude filters ----------
 * The search box holds free text (and accepts the full query syntax). Filters
 * are terms the user switched on: "include" shows only matching traffic,
 * "exclude" hides it. Together they make one query, the same one the CLI and
 * the API take: includes of one field match any value (status:2xx,3xx),
 * excludes are prefixed with "-".
 */
const FILTER_FIELDS = {
  is: 'Named filter',
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
const FIELD_RE = /^(-?)(host|method|status|path|mime|scope|source|ext|kind|is):(.+)$/i;

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
  return queryFor(T.filters, T.text);
}

function queryFor(filters, text) {
  const parts = [];
  for (const mode of ['include', 'exclude']) {
    const groups = new Map();
    for (const f of filters.filter((x) => x.mode === mode)) {
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
  if (text.trim()) parts.push(text.trim());
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
    'source:import': 'Imported from HAR',
    'status:none': 'No response',
  }[term.toLowerCase()];
  if (named) return { key: '', value: named };
  if (field === 'is') return { key: '', value: (S.named.find((n) => n.id === v.toLowerCase()) || {}).label || v };
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
/**
 * Traffic columns. Path takes whatever width is left; every other column has a
 * width the user can drag, saved with the project. Columns left of Path are
 * dragged by their right edge, the ones right of it by their left edge, so the
 * edge under the pointer always follows it.
 */
const TCOLS = [
  { key: 'n', label: '#', w: 52, th: 'num' },
  { key: 'method', label: 'Method', w: 66 },
  { key: 'host', label: 'Host', w: 220, col: 'c-host', th: 'c-host' },
  { key: 'path', label: 'Path' },
  { key: 'status', label: 'Status', w: 74 },
  { key: 'type', label: 'Type', w: 92 },
  { key: 'size', label: 'Size', w: 72, col: 'c-size', th: 'num c-size' },
  { key: 'ms', label: 'ms', w: 60, col: 'c-ms', th: 'num c-ms' },
  { key: 'time', label: 'Time', w: 84 },
];
const TCOL_MIN = 36;

function trafficColumns() {
  const widths = T.colW || {};
  const cols = TCOLS.map((c) => h('col', { class: c.col, style: c.w ? { width: (widths[c.key] || c.w) + 'px' } : null }));
  const flex = TCOLS.findIndex((c) => !c.w);
  const ths = TCOLS.map((c, i) => {
    const th = h('th', { class: (c.th ? c.th + ' ' : '') + 'sortable', 'data-col': c.key, title: 'Sort by ' + c.label, onclick: () => cycleSort(c.key) }, h('span', { class: 'sortarrow' }), h('span', { class: 'collabel', text: c.label }));
    if (c.w) th.append(h('span', { class: 'colgrip ' + (i < flex ? 'r' : 'l'), title: 'Drag to resize · double-click to reset', onclick: (e) => e.stopPropagation(), onmousedown: (e) => startColResize(e, c, cols[i], i < flex ? 1 : -1), ondblclick: (e) => (e.stopPropagation(), setColWidth(c, cols[i], null)) }));
    return th;
  });
  const head = h('thead', null, h('tr', null, ths));
  markSort(head);
  if (!T.colsLoaded) loadTrafficColumns(cols, head);
  return [h('colgroup', null, cols), head];
}

/** Ascending, then descending, then back to newest first. */
function cycleSort(key) {
  const cur = T.sort || '';
  T.sort = cur === key ? '-' + key : cur === '-' + key ? '' : key;
  markSort();
  saveTrafficColumns();
  T.refresh(true);
}

function markSort(head = $('.ttable thead')) {
  if (!head) return;
  const key = (T.sort || '').replace(/^-/, '');
  const desc = (T.sort || '').startsWith('-');
  for (const th of head.querySelectorAll('th[data-col]')) {
    const on = th.dataset.col === key;
    th.classList.toggle('sorted', on);
    th.querySelector('.sortarrow').textContent = on ? (desc ? '↓' : '↑') : '';
    th.setAttribute('aria-sort', on ? (desc ? 'descending' : 'ascending') : 'none');
  }
}

async function loadTrafficColumns(cols, head) {
  try {
    const v = await api('/api/views/traffic-columns');
    T.colsLoaded = true;
    T.colW = {};
    for (const c of TCOLS) {
      const w = v.widths && v.widths[c.key];
      if (c.w && Number.isFinite(w) && w >= TCOL_MIN) T.colW[c.key] = Math.round(w);
    }
    TCOLS.forEach((c, i) => c.w && (cols[i].style.width = (T.colW[c.key] || c.w) + 'px'));
    const sort = typeof v.sort === 'string' && TCOLS.some((c) => c.key === v.sort.replace(/^-/, '')) ? v.sort : '';
    if (sort !== (T.sort || '')) {
      T.sort = sort;
      markSort(head);
      if (S.view === 'traffic' && T.refresh) T.refresh(true);
    }
  } catch (_) {
    /* engine unreachable: default widths and order */
  }
}

function setColWidth(c, col, w) {
  T.colW = T.colW || {};
  if (w == null) delete T.colW[c.key];
  else T.colW[c.key] = w;
  col.style.width = (w || c.w) + 'px';
  saveTrafficColumns();
}

function saveTrafficColumns() {
  clearTimeout(T.colSaveT);
  T.colSaveT = setTimeout(() => api('/api/views/traffic-columns', { method: 'PUT', body: { widths: T.colW || {}, sort: T.sort || '' } }).catch(() => {}), 300);
}

function startColResize(e, c, col, dir) {
  e.preventDefault();
  e.stopPropagation();
  const startX = e.clientX;
  const startW = e.currentTarget.parentElement.getBoundingClientRect().width || (T.colW || {})[c.key] || c.w;
  document.body.classList.add('colresize');
  const move = (ev) => setColWidth(c, col, Math.round(Math.max(TCOL_MIN, Math.min(900, startW + dir * (ev.clientX - startX)))));
  const up = () => {
    document.body.classList.remove('colresize');
    window.removeEventListener('mousemove', move);
    window.removeEventListener('mouseup', up);
  };
  window.addEventListener('mousemove', move);
  window.addEventListener('mouseup', up);
}

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
    placeholder: 'Search any text, or type a filter such as host:example.com',
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
  const table = h('table', { class: 'ttable' }, trafficColumns(), tbody);
  const wrap = h('div', { class: 'tablewrap', id: 'tablewrap' }, table);
  append(main, [
    h(
      'div',
      { class: 'view' },
      h(
        'div',
        { class: 'toolbar' },
        backButton(),
        h('div', { class: 'search', id: 'searchbox' }, h('span', { class: 'mg', text: '⌕' }), input, h('kbd', { text: '/' })),
        h('span', { class: 'count', id: 'tcount' }),
        liveBtn,
        interceptButton(),
        h('button', { class: 'btn sm', id: 'harbtn', text: 'HAR ▾', title: 'Import a HAR file, or export traffic as one', onclick: (e) => harMenu(e.currentTarget) }),
      ),
      h('div', { class: 'filterchips', id: 'chips' }),
      h('div', { class: 'qerr', id: 'qerr', hidden: true }),
      h('div', { id: 'bannerslot' }),
      h('div', { id: 'icslot' }),
      h('div', { class: 'traffic' }, wrap, h('div', { id: 'inspslot' })),
    ),
  ]);
  renderChips();
  renderBanner();
  IC.drawn = null;
  drawInterceptPanel();
  loadIntercept();
  T.refresh = refreshTraffic;
  loadTrafficView().then(() => {
    if (S.view !== 'traffic' || !input.isConnected) return;
    input.value = T.text;
    renderChips();
    T.refresh(true);
  });
  if (T.sel) openInspector(T.sel);
}

/* ---------- intercept ---------- */

/** Requests and responses held in the proxy, the one being edited, and unsent edits by id. */
const IC = { on: false, queue: [], sel: null, drafts: {}, seq: -1, opts: {}, drawn: null };

function interceptButton() {
  const n = IC.queue.length;
  return h(
    'button',
    {
      class: 'btn sm icbtn' + (IC.on ? ' on' : ''),
      id: 'icbtn',
      title: IC.on
        ? 'Intercept is on: matching requests wait for you. Turn it off to let everything held go on (i)'
        : 'Intercept: hold requests to in-scope hosts to edit, forward or drop them (i)',
      onclick: () => setIntercept(!IC.on),
    },
    h('span', { class: 'dot' }),
    'Intercept',
    n ? h('span', { class: 'qn', text: n }) : null,
  );
}

async function setIntercept(on) {
  try {
    const r = await api('/api/intercept', { method: 'PUT', body: { on } });
    if (!on && r.released) toast(`Intercept is off. ${r.released} held item(s) went on unchanged.`, 'ok');
    applyIntercept(r);
  } catch (e) {
    toast(e.message, 'err');
  }
}

async function loadIntercept() {
  try {
    applyIntercept(await api('/api/intercept'));
  } catch (_) {}
}

function applyIntercept(r) {
  Object.assign(IC, { on: r.on, queue: r.queue || [], seq: r.seq, opts: r });
  const ids = new Set(IC.queue.map((i) => i.id));
  for (const k of Object.keys(IC.drafts)) if (!ids.has(Number(k))) delete IC.drafts[k];
  if (!ids.has(IC.sel)) IC.sel = IC.queue.length ? IC.queue[0].id : null;
  const b = $('#icbtn');
  if (b) b.replaceWith(interceptButton());
  drawInterceptPanel();
}

const fmtSecs = (s) => (s >= 60 && s % 60 === 0 ? s / 60 + ' min' : s + ' s');

function drawInterceptPanel() {
  const slot = $('#icslot');
  if (!slot) return;
  const o = IC.opts;
  // Redraw only when something changed, so typing in the editor is not interrupted.
  const key = JSON.stringify([IC.on, IC.sel, IC.queue.map((i) => i.id), o.hold, o.filter, o.responses, o.timeout_s]);
  if (IC.drawn === key && slot.firstChild) return;
  IC.drawn = key;
  if (!IC.on && !IC.queue.length) return clear(slot);
  const what = (o.hold === 'everything' ? 'every host' : 'in-scope hosts') + (o.responses ? ', requests and responses' : ', requests only') + (o.filter ? ', matching ' + o.filter : '');
  const head = h(
    'div',
    { class: 'ichead' },
    h('b', { text: 'Intercept' }),
    h('span', { class: 'muted', text: `Holding ${what}. Anything unanswered goes on after ${fmtSecs(o.timeout_s || 0)}.` }),
    h('button', { class: 'btn sm ghost', text: 'Options…', onclick: () => ((S.settingsSection = 'intercept'), leaveTo('settings')) }),
    h('button', { class: 'btn sm', text: 'Forward all', disabled: !IC.queue.length, title: 'Send everything held on, unchanged', onclick: forwardAllHeld }),
  );
  if (!IC.queue.length) return clear(slot, h('div', { class: 'icpanel' }, head, h('div', { class: 'icnone muted', text: 'Nothing held. Matching requests wait here until you forward or drop them.' })));
  const list = h(
    'div',
    { class: 'iclist' },
    IC.queue.map((it) =>
      h(
        'button',
        { class: 'icitem' + (it.id === IC.sel ? ' sel' : ''), onclick: () => ((IC.sel = it.id), drawInterceptPanel()) },
        h('span', { class: 'tag ' + (it.kind === 'response' ? 'pend' : 'replay'), text: it.kind === 'response' ? 'resp' : 'req' }),
        h('span', { class: 'meth m-' + it.method, text: it.method }),
        h('span', { class: 'icurl', text: it.url, title: it.url }),
        it.status ? h('span', { class: statusClass(it.status), text: it.status }) : null,
      ),
    ),
  );
  const it = IC.queue.find((i) => i.id === IC.sel);
  const focused = document.activeElement && document.activeElement.classList.contains('icedit') ? document.activeElement : null;
  const ta = h('textarea', {
    class: 'icedit',
    spellcheck: 'false',
    'aria-label': 'Held ' + it.kind + ' as text',
    value: IC.drafts[it.id] != null ? IC.drafts[it.id] : it.raw,
    oninput: () => (IC.drafts[it.id] = ta.value),
    onkeydown: (e) => {
      if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
        e.preventDefault();
        forwardHeld(it.id);
      }
      if (e.key === 'Escape') ta.blur();
    },
  });
  const editor = h(
    'div',
    { class: 'iceditor' },
    h('div', { class: 'lbl' }, it.kind === 'response' ? 'Response' : 'Request', h('span', { class: 'r', text: `${it.http_version} · goes on by itself at ${fmtTime(it.expires_at)}` })),
    it.note ? h('div', { class: 'icnote', text: it.note }) : null,
    ta,
    h('div', { class: 'qerr', id: 'icerr' }),
    h(
      'div',
      { class: 'icact' },
      h('button', { class: 'btn sm', text: 'Revert', title: 'Undo your edits', onclick: () => (delete IC.drafts[it.id], (IC.drawn = null), drawInterceptPanel()) }),
      h('button', { class: 'btn sm danger', text: 'Drop', title: 'Stop it here; the browser gets an error page (d)', onclick: () => dropHeld(it.id) }),
      h('button', { class: 'btn sm primary', text: 'Forward', title: 'Send it on, with your edits (⌘↵, or f)', onclick: () => forwardHeld(it.id) }),
    ),
  );
  clear(slot, h('div', { class: 'icpanel' }, head, h('div', { class: 'icbody' }, list, editor)));
  if (focused) ta.focus();
}

async function forwardHeld(id) {
  const it = IC.queue.find((i) => i.id === id);
  if (!it) return;
  const raw = IC.drafts[id];
  try {
    await api(`/api/intercept/${id}/forward`, { method: 'POST', body: raw != null && raw !== it.raw ? { raw } : {} });
    delete IC.drafts[id];
  } catch (e) {
    const err = $('#icerr');
    if (err && e.code === 'bad_edit') return (err.textContent = e.message);
    toast(e.message, 'err');
  }
  loadIntercept();
}

async function dropHeld(id) {
  try {
    await api(`/api/intercept/${id}/drop`, { method: 'POST', body: {} });
  } catch (e) {
    toast(e.message, 'err');
  }
  loadIntercept();
}

async function forwardAllHeld() {
  try {
    await api('/api/intercept/forward-all', { method: 'POST', body: {} });
  } catch (e) {
    toast(e.message, 'err');
  }
  loadIntercept();
}

/** The request or response as it arrived, before it was edited in Intercept. */
function showOriginal(ex) {
  const block = (title, text) => (text ? [h('div', { class: 'lbl', text: title }), h('pre', { class: 'raw origraw', text })] : []);
  modal(
    'Before it was edited',
    [
      h('p', { class: 'muted mnote', text: 'This exchange was changed in Intercept. The Lens shows what was sent; this is what arrived.' }),
      block('Original request', ex.original_request),
      block('Original response', ex.original_response),
    ],
    [h('button', { class: 'btn', text: 'Close', onclick: closeModal })],
  );
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
  // Many requests that differ only by an id: offer to fold them into one row each.
  let alike = null;
  if (!T.group && T.items.length) {
    const g = alikeGroups(T.items);
    const saved = T.items.length - g.size;
    const biggest = Math.max(...[...g.values()].map((x) => x.length));
    if (saved >= 5 && biggest >= 3) alike = saved;
  }
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
    T.group
      ? h(
          'span',
          { class: 'fgroup' },
          h('span', { class: 'chipslbl', text: 'View' }),
          h('span', { class: 'fchip include' }, h('span', { class: 'fbody' }, h('span', { class: 'fv', text: 'Look-alikes grouped' })), h('button', { class: 'fx', title: 'Show every request on its own row', 'aria-label': 'Stop grouping look-alikes', text: '×', onclick: () => setGrouping(false) })),
        )
      : null,
    sugg.length || alike ? h('span', { class: 'fsep' }) : null,
    // Labelled so one-click suggestions are not mistaken for active filters.
    sugg.length || alike ? h('span', { class: 'chipslbl', text: 'Suggested' }) : null,
    alike
      ? h(
          'button',
          { class: 'chip k-alike', title: 'Fold requests that differ only by an id (like /orders/1041 and /orders/1042) into one row each. Nothing is hidden: expand a row to see them all.', onclick: () => setGrouping(true) },
          h('span', { text: 'Group look-alikes' }),
          h('span', { class: 'n', text: '−' + alike + ' rows' }),
        )
      : null,
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
    case 'is':
      return S.named.map((n) => n.id);
    case 'scope':
      return ['in', 'out'];
    case 'source':
      return ['proxy', 'replay', 'import'];
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
  // Named filters from filter packs: one click each, in the chosen mode.
  const named = S.named.length
    ? h(
        'div',
        { class: 'namedlist', 'aria-label': 'Named filters' },
        S.named.map((n) =>
          h('button', {
            class: 'named',
            text: n.label,
            title: (n.description ? n.description + '\n' : '') + 'is:' + n.id + ' = ' + n.query + '\nfrom the ' + n.pack + ' filter pack',
            onclick: () => {
              closePopover();
              addFilter('is:' + n.id, mode);
            },
          }),
        ),
      )
    : null;
  const pop = h(
    'div',
    { class: 'popover', role: 'dialog', 'aria-label': 'Add filter' },
    seg,
    named,
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
    [{ label: 'Copy as curl', run: () => copyCurl(ex.id) }],
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
  leaveTo('traffic');
}

async function refreshTraffic(userAction) {
  if (!userAction && !T.live) return;
  if (S.view !== 'traffic') return;
  const seq = (T.seq = (T.seq || 0) + 1);
  let data;
  try {
    data = await api('/api/traffic?limit=500&q=' + encodeURIComponent(fullQuery()) + (T.sort ? '&sort=' + encodeURIComponent(T.sort) : ''));
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
  if (!T.group) renderChips();
  drawRows(userAction ? Infinity : prevMax);
}

function emptyTraffic(st) {
  const input = h('input', { placeholder: 'example.com', spellcheck: 'false', autocomplete: 'off', value: pstore('plonix.lastTarget') || '' });
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
  const groups = T.group ? alikeGroups(T.items) : null;
  const shown = [];
  for (const ex of T.items) {
    const g = groups && groups.get(alikeKey(ex));
    if (!g || g.length < 2) shown.push({ ex });
    else if (g[0] === ex) shown.push({ ex, group: g });
    else if (T.open.has(alikeKey(ex))) shown.push({ ex, member: true });
  }
  T.visible = groups ? shown.map((r) => r.ex.id) : null;
  const rows = shown.map(({ ex, group, member }) => {
    const tags = [];
    if (group) {
      const k = alikeKey(ex);
      const isOpen = T.open.has(k);
      tags.push(
        h('button', {
          class: 'tag alike' + (isOpen ? ' on' : ''),
          text: (isOpen ? '▾ ' : '▸ ') + '×' + group.length,
          title: `${group.length} look-alike requests to ${foldPath(ex.path)}. Click to ${isOpen ? 'fold them' : 'show them all'}.`,
          onclick: (e) => {
            e.stopPropagation();
            if (isOpen) T.open.delete(k);
            else T.open.add(k);
            drawRows(Infinity);
          },
        }),
      );
    }
    if (ex.source === 'replay') tags.push(h('span', { class: 'tag replay', text: 'sent' }));
    if (ex.source === 'import') tags.push(h('span', { class: 'tag imported', text: 'har', title: 'Imported from a HAR file' }));
    if (ex.edited) tags.push(h('span', { class: 'tag edited', text: 'edited', title: 'Changed in Intercept before it went on' }));
    const tr = h(
      'tr',
      {
        'data-id': ex.id,
        class: [ex.id === T.sel ? 'sel' : '', T.picked.has(ex.id) ? 'picked' : '', ex.in_scope ? '' : 'out', ex.id > freshAbove ? 'fresh' : '', member ? 'member' : ''].join(' ').trim(),
        onclick: (e) => (e.metaKey || e.ctrlKey || e.shiftKey ? pickRow(ex.id, e.shiftKey) : openInspector(ex.id)),
        ondblclick: () => sendToBench(ex.id),
        oncontextmenu: (e) => rowMenu(e, ex),
      },
      h('td', { class: 'num', text: ex.id }),
      h('td', null, h('span', { class: 'meth m-' + ex.method, text: ex.method })),
      h('td', { class: 'host c-host', text: ex.host + (ex.port !== 443 && ex.port !== 80 ? ':' + ex.port : ''), title: ex.host }),
      h('td', { class: 'url', title: target(ex) }, tags, tags.length ? ' ' : '', group && !T.open.has(alikeKey(ex)) ? foldPath(ex.path) : target(ex)),
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
  const ids = T.visible || T.items.map((x) => x.id);
  if (!ids.length) return;
  let i = ids.indexOf(T.sel);
  i = i < 0 ? 0 : Math.max(0, Math.min(ids.length - 1, i + delta));
  openInspector(ids[i]);
  const tr = document.querySelector(`#rows tr[data-id="${ids[i]}"]`);
  if (tr) tr.scrollIntoView({ block: 'nearest' });
}

/** ⌘/Ctrl-click picks a row for export, Shift-click picks every row up to it. */
function pickRow(id, range) {
  if (range && T.pickAnchor != null) {
    const ids = T.items.map((x) => x.id);
    const [a, b] = [ids.indexOf(T.pickAnchor), ids.indexOf(id)].sort((x, y) => x - y);
    if (a >= 0) ids.slice(a, b + 1).forEach((x) => T.picked.add(x));
  } else if (T.picked.has(id)) {
    T.picked.delete(id);
  } else {
    T.picked.add(id);
  }
  T.pickAnchor = id;
  for (const tr of document.querySelectorAll('#rows tr[data-id]')) tr.classList.toggle('picked', T.picked.has(Number(tr.dataset.id)));
  updatePicked();
}

function clearPicked() {
  T.picked.clear();
  T.pickAnchor = null;
  for (const tr of document.querySelectorAll('#rows tr.picked')) tr.classList.remove('picked');
  updatePicked();
}

function updatePicked() {
  const btn = $('#harbtn');
  if (btn) btn.textContent = T.picked.size ? `HAR · ${T.picked.size} picked ▾` : 'HAR ▾';
}

/** Import a HAR file, or export all traffic, the filtered view or the picked rows. */
function harMenu(anchor) {
  closePopover();
  const q = fullQuery();
  const item = (label, run, title) => h('button', { role: 'menuitem', text: label, title, onclick: () => (closePopover(), run()) });
  const items = [
    item('Export all traffic…', () => exportHar({ q: '' })),
    q ? item(`Export the filtered view (${T.total})…`, () => exportHar({ q }), q) : null,
    T.picked.size ? item(`Export ${T.picked.size} picked row${T.picked.size === 1 ? '' : 's'}…`, () => exportHar({ ids: [...T.picked] })) : null,
    T.picked.size && toolOn('access-check') ? item(`Check access on ${T.picked.size} picked`, () => startAccessCheck({ targets: [...T.picked], sourceLabel: 'picked in Traffic' })) : null,
    T.picked.size ? item('Clear picked rows', clearPicked) : null,
    h('div', { class: 'msep' }),
    item('Import HAR file…', importHar),
  ];
  const menu = h('div', { class: 'ctxmenu', role: 'menu' }, items, T.picked.size ? null : h('div', { class: 'mnote', text: '⌘-click or Shift-click rows to pick them for export.' }));
  showPopover(menu, anchor.getBoundingClientRect());
}

/** The app saves with the system's Save dialog; a browser downloads. */
const nativeFiles = () => IN_APP && S.status && S.status.native_dialogs;

async function exportHar({ q = '', ids = [] } = {}) {
  try {
    if (nativeFiles()) {
      const r = await api('/api/har/export-file', { method: 'POST', body: { q, ids } });
      if (!r.cancelled) toast(`Saved ${r.entries} request${r.entries === 1 ? '' : 's'} to ${r.path}`, 'ok');
      return;
    }
    const qs = ids.length ? 'ids=' + ids.join(',') : 'q=' + encodeURIComponent(q);
    let resp;
    try {
      resp = await fetch('/api/har?' + qs, { headers: { Authorization: 'Bearer ' + S.token, 'X-Plonix-Client': 'gui' }, cache: 'no-store' });
    } catch (_) {
      throw new ApiError(0, 'engine_down', 'The Plonix engine is not reachable.');
    }
    if (!resp.ok) {
      const data = await resp.json().catch(() => null);
      throw new ApiError(resp.status, (data && data.code) || 'error', (data && data.error) || resp.statusText, data);
    }
    const name = ((resp.headers.get('content-disposition') || '').match(/filename="([^"]+)"/) || [])[1] || 'plonix.har';
    const url = URL.createObjectURL(await resp.blob());
    const a = h('a', { href: url, download: name, hidden: true });
    document.body.append(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(url), 10000);
    toast(`Exported ${name}`, 'ok');
  } catch (e) {
    toast(e.message, 'err');
  }
}

async function importHar() {
  const done = (r) => {
    if (r.cancelled) return;
    const parts = [`Imported ${r.imported} request${r.imported === 1 ? '' : 's'}`];
    if (r.duplicates) parts.push(`${r.duplicates} already here`);
    if (r.skipped) parts.push(`${r.skipped} could not be read`);
    toast(parts.join(' · '), r.imported || !r.skipped ? 'ok' : 'err');
    if (r.imported) setQuery('source:import');
  };
  if (nativeFiles()) {
    try {
      done(await api('/api/har/import-file', { method: 'POST', body: {} }));
    } catch (e) {
      toast(e.message, 'err');
    }
    return;
  }
  const input = h('input', { type: 'file', accept: '.har,application/json', hidden: true });
  input.addEventListener('change', async () => {
    const file = input.files && input.files[0];
    input.remove();
    if (!file) return;
    if (file.size > 64 * 1024 * 1024) return toast('This file is over 64 MB. Import it with `plonix har import ' + file.name + '`, or from the Plonix app.', 'err');
    toast('Importing ' + file.name + '…');
    try {
      const resp = await fetch('/api/har/import', {
        method: 'POST',
        headers: { Authorization: 'Bearer ' + S.token, 'X-Plonix-Client': 'gui', 'Content-Type': 'application/json' },
        body: file,
      });
      const data = await resp.json().catch(() => null);
      if (!resp.ok) throw new ApiError(resp.status, (data && data.code) || 'error', (data && data.error) || resp.statusText, data);
      done(data);
    } catch (e) {
      toast(e.message, 'err');
    }
  });
  document.body.append(input);
  input.click();
}

async function openInspector(id) {
  T.sel = id;
  for (const tr of document.querySelectorAll('#rows tr')) tr.classList.toggle('sel', Number(tr.dataset.id) === id);
  for (const tr of document.querySelectorAll('tr[data-ex]')) tr.classList.toggle('sel', Number(tr.dataset.ex) === id);
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
  const suggSlot = h('div', { class: 'lenssugg', hidden: true });
  if (T.inspH) insp.style.height = T.inspH + 'px';
  const enc = header(ex.resp_headers, 'content-encoding');
  const isJson = /json/.test(header(ex.resp_headers, 'content-type') || '');
  const respCol = h('div', { class: 'col' });
  // A WebSocket handshake: its messages go under the response.
  const msgBox = ex.status === 101 ? h('div', { class: 'wsbox' }) : null;
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
      h('div', { class: 'lbl' }, 'Response', enc ? h('span', { class: 'decodetag', text: 'decoded · ' + enc }) : null, cutTag(ex.resp_truncated, ex.resp_size, ex.resp_body), seg),
      h('div', { class: 'spotslot' }),
      rawPre(responseText(ex, T.pretty)),
      msgBox,
    );
    if (ex.insights) drawInsights(respCol, ex.insights.filter((i) => i.side === 'response'));
  };
  drawResp();
  if (msgBox) drawMessages(msgBox, ex);
  const reqCol = h(
    'div',
    { class: 'col' },
    h('div', { class: 'lbl' }, 'Request', cutTag(ex.req_truncated, ex.req_size, ex.req_body), h('span', { class: 'r', text: '#' + ex.id + ' · ' + fmtTime(ex.ts) + ' · ' + (ex.initiator || ex.source || 'proxy') })),
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
        h('span', { text: fmtSize(ex.resp_size != null ? ex.resp_size : b64len(ex.resp_body)) }),
        ex.http_version ? h('span', { text: ex.http_version, title: 'The protocol spoken with the server' }) : null,
        ex.edited ? h('button', { class: 'tag edited', text: 'edited', title: 'Changed in Intercept before it went on. Show the original', onclick: () => showOriginal(ex) }) : null,
        ex.replaced && ex.replaced.length ? h('span', { class: 'tag edited', text: 'replaced', title: 'Changed by match-and-replace rules:\n' + ex.replaced.join('\n') }) : null,
        clientCertTag(ex),
        ex.source === 'import' ? h('span', { class: 'tag imported', text: 'har', title: 'Imported from a HAR file' }) : null,
        h('span', { class: 'tag ' + scopeTag(decide(ex.host)), text: scopeLabel(decide(ex.host)) }),
      ),
      h('button', { class: 'btn sm primary', text: 'Send to Bench', title: 'Edit and re-send on the Bench (b, or double-click a row)', onclick: () => sendToBench(id) }),
      h('button', { class: 'btn sm', text: 'Copy curl', title: 'Copy this request as a curl command', onclick: () => copyCurl(id) }),
      h('button', { class: 'btn sm', text: 'New finding', onclick: () => newFinding([id], `${ex.method} ${ex.path}`) }),
      askButton({ kind: 'request', id }),
      h('button', { class: 'iconbtn', text: '✕', title: 'Close (Esc)', onclick: closeInspector }),
    ),
    suggSlot,
    sideBySide('split', 'lens', reqCol, respCol),
  ]);
  const splitter = h('div', { class: 'splitter', onmousedown: (e) => startResize(e, insp) });
  clear(slot, splitter, insp);
  slot.style.display = 'contents';
  loadInsights(ex).then((list) => {
    if (T.sel !== id) return;
    drawLensSuggestions(suggSlot, ex, list || []);
    if (!list) return;
    drawInsights(reqCol, list.filter((i) => i.side === 'request'));
    drawInsights(respCol, list.filter((i) => i.side === 'response'));
  });
}

/** The client certificate Plonix presented for this exchange, if the server asked for one. */
function clientCertTag(ex) {
  if (!ex.client_cert) return null;
  return h('span', { class: 'tag cert', text: 'cert · ' + ex.client_cert.split(' for ')[0], title: 'Client certificate presented: ' + ex.client_cert + '\n(Settings › Client certificates)' });
}

/* ---------- WebSocket messages of a handshake ---------- */

async function drawMessages(box, ex) {
  let page;
  try {
    page = await api(`/api/traffic/${ex.id}/messages?limit=2000`);
  } catch (e) {
    clear(box, h('div', { class: 'hl-note', text: 'Could not load the messages: ' + e.message }));
    return;
  }
  const shown = page.items.length;
  const rows = page.items.map((m) => {
    const out = m.direction === 'to_server';
    return h(
      'div',
      { class: 'wsmsg' },
      h('span', { class: 'wsdir ' + (out ? 'out' : 'in'), text: out ? '↑' : '↓', title: out ? 'Sent by the client' : 'Sent by the server' }),
      h('span', { class: 'wsop', text: m.opcode }),
      h('span', { class: 'wsmeta', text: fmtTime(m.ts) + ' · ' + fmtSize(m.size) + (m.truncated ? ' · first ' + fmtSize(b64len(m.payload)) + ' kept' : '') }),
      h('pre', { class: 'wsbody', text: messageText(m) }),
    );
  });
  clear(
    box,
    h('div', { class: 'lbl' }, 'Messages', h('span', { class: 'r', text: shown < page.total ? `first ${shown} of ${page.total}` : String(page.total) })),
    page.total ? h('div', { class: 'wslist' }, rows) : h('div', { class: 'wsempty hl-note', text: 'No messages yet. Messages show up as the connection carries them.' }),
  );
}

/** A message's text, or a hex preview of binary data. */
function messageText(m) {
  if (m.text != null) return m.text;
  let bytes = '';
  try {
    bytes = atob(m.payload || '');
  } catch (_) {}
  if (!bytes.length) return '';
  const hex = Array.from(bytes.slice(0, 64), (c) => c.charCodeAt(0).toString(16).padStart(2, '0')).join(' ');
  return hex + (bytes.length > 64 ? ' …' : '');
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

const CAT_TITLE = { secret: 'Exposed secret', decode: 'Decodable value', pii: 'Personal data', info: 'Infrastructure detail', extension: 'From an installed extension, not from Plonix' };

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

/* ---------- Lens suggestions: one-click next steps for the open request ---------- */

/** Why this exchange may be worth recording as a finding, if anything stands out. */
function findingHint(ex, list) {
  const ok = ex.status >= 200 && ex.status < 300;
  const where = `${ex.method} ${ex.path}`;
  const req = list.filter((i) => i.side === 'request');
  const resp = list.filter((i) => i.side === 'response');
  const noted = (i, word) => (i.notes || []).some((n) => n.startsWith(word));
  const secret = resp.find((i) => i.category === 'secret');
  if (secret) return { chip: secret.label + ' exposed', title: `${secret.label} exposed in ${where}`, severity: 'high', note: `Plonix spotted a ${secret.label} in the ${secret.location}.` };
  const unsigned = ok && req.find((i) => noted(i, 'unsigned'));
  if (unsigned) return { chip: 'Unsigned token accepted', title: `Unsigned token accepted by ${where}`, severity: 'high', note: `The request carries an unsigned token in the ${unsigned.location}, and the server answered ${ex.status}.` };
  const expired = ok && req.find((i) => noted(i, 'expired'));
  if (expired) return { chip: 'Expired token accepted', title: `Expired token still accepted by ${where}`, severity: 'medium', note: `The token in the ${expired.location} has expired, and the server still answered ${ex.status}.` };
  const card = resp.find((i) => i.kind === 'card-number');
  if (card) return { chip: 'Card number in response', title: `Card number returned by ${where}`, severity: 'medium', note: `The response contains a card number in the ${card.location}.` };
  const emails = resp.filter((i) => i.kind === 'email');
  if (emails.length >= 3) return { chip: `${emails.length} email addresses`, title: `Several people's email addresses returned by ${where}`, severity: 'medium', note: `The response lists ${emails.length} different email addresses.` };
  const trace = resp.find((i) => i.kind === 'stack-trace');
  if (trace) return { chip: 'Stack trace in response', title: `Stack trace exposed by ${where}`, severity: 'low', note: `The response shows a stack trace: ${trace.value.slice(0, 160)}` };
  const ip = resp.find((i) => i.kind === 'private-ip');
  if (ip && ex.status >= 500) return { chip: 'Error shows an internal address', title: `Error on ${where} shows an internal address`, severity: 'low', note: `The ${ex.status} response shows the internal address ${ip.value}.` };
  if (ex.status >= 500) return { chip: 'Server error', title: `Server error on ${where}`, severity: 'low', note: `The server answered ${ex.status}.` };
  return null;
}

const IDEAS_QUESTION =
  'Suggest up to three things worth trying next on this endpoint. For each, say in plain words what to change, what result would mean there is a problem, and give the exact request to send from the Plonix Bench. Start with the most promising one.';

/** A quick look at whether a response is an API description (OpenAPI or Swagger). */
function looksLikeApiSpec(ex) {
  const t = ex.resp_text;
  return !!t && t.length < 8_000_000 && /"(openapi|swagger)"\s*:/.test(t.slice(0, 4000)) && /"paths"\s*:/.test(t);
}

/** The "Suggested" row under the Lens header. Every chip is one click to act on, and nothing is sent until clicked. */
async function drawLensSuggestions(slot, ex, list) {
  const chips = [];
  const inScope = decide(ex.host) === 'accepted';
  const hint = inScope ? findingHint(ex, list) : null;
  if (hint) {
    chips.push(
      h('button', { class: 'chip k-warn', title: 'Record this as a finding, with this request as evidence. Claude can write it up for you.', onclick: () => findingForm(null, [ex.id], hint.title, hint) }, h('span', { text: '+ Finding: ' + hint.chip })),
    );
  }
  if (looksLikeApiSpec(ex)) {
    let spec = null;
    try {
      spec = await api(`/api/traffic/${ex.id}/spec`);
    } catch (_) {}
    if (spec && spec.endpoints.length && slot.isConnected) {
      const todo = spec.endpoints.filter((e) => !e.visited).length;
      chips.push(
        h(
          'button',
          { class: 'chip k-path', title: `${spec.title || 'API description'}: ${spec.endpoints.length} endpoints for ${spec.host}. Show them in the Map.`, onclick: () => showSpecInMap(spec.host) },
          h('span', { text: 'API description' }),
          h('span', { class: 'n', text: todo ? `${todo} not visited` : `${spec.endpoints.length} endpoints` }),
        ),
      );
    }
  }
  if (agentsOn() && inScope) {
    chips.push(h('button', { class: 'chip k-ai', title: 'Ask Claude Code what to try next here. You see what is shared first.', onclick: () => askClaude({ kind: 'request', id: ex.id }, { question: IDEAS_QUESTION }) }, h('span', { text: '✦ Ideas for this endpoint' })));
  }
  if (!slot.isConnected) return;
  slot.hidden = !chips.length;
  clear(slot, chips.length ? [h('span', { class: 'chipslbl', text: 'Suggested' }), chips] : null);
}

function showSpecInMap(host) {
  M.sel = host;
  M.specOpen = host;
  leaveTo('map');
}

/* ---------- Claude writes the finding ---------- */

function findingQuestion(ids, note) {
  return [
    'Write this up as a security finding for a report.',
    note ? 'What Plonix noticed: ' + note : '',
    ids.length > 1 ? `More evidence: requests ${ids.slice(1).map((i) => '#' + i).join(', ')}. Read them with get_request.` : '',
    'Use simple, plain words a developer who is new to security can follow. Answer with only a JSON object and no other text:',
    '{"title": "a short title, under 90 characters", "severity": "info, low, medium, high or critical", "severity_reason": "one or two sentences on why this severity", "what_happens": "two or three sentences", "why_it_matters": "one or two sentences on the impact", "steps": ["each step to reproduce it, in order"]}',
    'If the evidence does not show a real issue, say so in what_happens and use info.',
  ]
    .filter(Boolean)
    .join('\n');
}

/** Pulls the JSON object out of Claude's answer. */
function parseFindingAnswer(text) {
  const a = text.indexOf('{');
  const b = text.lastIndexOf('}');
  if (a < 0 || b <= a) return null;
  try {
    const o = JSON.parse(text.slice(a, b + 1));
    return o && typeof o.title === 'string' ? o : null;
  } catch (_) {
    return null;
  }
}

function findingDescription(o, curl) {
  const out = [];
  if (o.what_happens) out.push('What happens\n' + o.what_happens);
  if (o.why_it_matters) out.push('Why it matters\n' + o.why_it_matters);
  if (o.severity_reason) out.push('Why this severity\n' + o.severity_reason);
  const steps = Array.isArray(o.steps) ? o.steps.filter((x) => typeof x === 'string' && x.trim()) : [];
  if (steps.length) out.push('How to reproduce\n' + steps.map((x, i) => `${i + 1}. ${x.trim()}`).join('\n'));
  if (curl) out.push('The request, as curl\n' + curl);
  return out.join('\n\n');
}

/**
 * Asks Claude Code to write a finding from its evidence requests. Uses the
 * same context bundle and in-app conversation as Ask Claude, then fills the
 * form; nothing is saved until the user presses Save.
 */
async function writeFindingWithClaude(ids, note, onProgress, signal) {
  let bundle = await api('/api/agents/ask', { method: 'POST', body: { kind: 'request', id: ids[0], question: findingQuestion(ids, note) } });
  if (bundle.over_budget) bundle = await api('/api/agents/ask', { method: 'POST', body: { kind: 'request', id: ids[0], question: findingQuestion(ids, note), max_body_chars: 2000 } });
  if (bundle.over_budget) throw new Error('This request is too large to send as is. Use Ask Claude to choose what to share.');
  const { id } = await api('/api/agents/run', { method: 'POST', body: { prompt: bundle.prompt } });
  signal.run = id;
  let since = 0;
  let text = '';
  for (;;) {
    if (signal.stop) return null;
    const snap = await api(`/api/agents/run/${id}?since=${since}`);
    for (const ev of snap.events) {
      since = ev.seq + 1;
      if (ev.type === 'text') text += ev.text + '\n';
      else if (ev.type === 'error') throw new Error(ev.text);
    }
    if (snap.status !== 'running') break;
    const p = snap.progress;
    if (p) onProgress(`${p.step}… ${claudeStats(p)}` + (p.idle_ms > CLAUDE_QUIET_MS ? ' · waiting on Claude Code' : ''));
    await new Promise((r) => setTimeout(r, 600));
  }
  const o = parseFindingAnswer(text);
  if (!o) throw new Error('Claude did not answer in the expected shape. Try again, or use Ask Claude.');
  let curl = '';
  try {
    curl = curlForExchange(await getExchange(ids[0]));
  } catch (_) {}
  return { title: o.title.trim().slice(0, 200), severity: SEVERITIES.includes(String(o.severity).toLowerCase()) ? String(o.severity).toLowerCase() : null, description: findingDescription(o, curl) };
}

/* ---------- copy as curl ---------- */

const shq = (s) => "'" + String(s).replace(/'/g, "'\\''") + "'";

/**
 * A curl command that sends this request again. Headers curl works out by
 * itself (Content-Length, HTTP/2 pseudo-headers, a Host matching the URL)
 * are left out; a binary body is noted rather than pasted.
 */
function curlFor(method, url, headers, body, binary) {
  const host = hostOf(url);
  const parts = ['curl'];
  const m = (method || 'GET').toUpperCase();
  if (m !== 'GET' || (body && m !== 'POST')) parts.push('-X ' + m);
  parts.push(shq(url));
  let compressed = false;
  for (const [k, v] of headers || []) {
    const name = k.toLowerCase();
    if (name.startsWith(':') || name === 'content-length' || name === 'connection') continue;
    if (name === 'host' && v.split(':')[0].toLowerCase() === host) continue;
    if (name === 'accept-encoding' && /gzip|br|deflate/.test(v)) compressed = true;
    parts.push('-H ' + shq(`${k}: ${v}`));
  }
  if (compressed) parts.push('--compressed');
  if (body) parts.push('--data-raw ' + shq(body));
  let cmd = parts.join(' \\\n  ');
  if (binary) cmd += '\n# The body is binary and is not included.';
  return cmd;
}

function curlForExchange(ex) {
  const binary = ex.req_text == null && b64len(ex.req_body) > 0;
  return curlFor(ex.method, ex.url, ex.req_headers, binary ? '' : ex.req_text || '', binary);
}

async function copyCurl(id) {
  try {
    await copyText(curlForExchange(await getExchange(id)));
  } catch (e) {
    toast(e.message, 'err');
  }
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
  for (const tr of document.querySelectorAll('#rows tr.sel, tr[data-ex].sel')) tr.classList.remove('sel');
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

/** Opens a request in the Lens. Screens with a Lens of their own (Traffic,
 * Map, Findings) show it in place, so the user never loses their spot. */
function showExchange(id) {
  if ($('#inspslot')) return openInspector(id);
  T.sel = id;
  leaveTo('traffic');
}

/* ======================================================================
   Bench
   ====================================================================== */

const R = { tabs: [], active: 0, mode: 'response' };
(function loadBench() {
  const saved = pstore('plonix.bench');
  if (saved && Array.isArray(saved.tabs)) {
    R.tabs = saved.tabs;
    R.active = Math.min(saved.active || 0, Math.max(0, R.tabs.length - 1));
  }
})();
function saveBench() {
  const tabs = R.tabs.map((t) => ({ ...t, error: undefined, picks: [], runState: undefined }));
  pstore('plonix.bench', { tabs: tabs.slice(-30), active: R.active });
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
  leaveTo('bench');
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
  if (toolOn('saved-users') && S.users == null) loadUsers().then(() => S.view === 'bench' && renderBench(main));
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
    h('div', { class: 'toolbar' }, backButton(), h('h2', { text: 'Bench' }), h('span', { class: 'hint', text: 'Each tab is an experiment: edit a request, send it, branch it, compare responses. Sends only reach in-scope hosts.' })),
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

  const method = h('input', { class: 'method', value: tab.method, spellcheck: 'false', list: 'methods', oninput: () => ((tab.method = method.value.toUpperCase()), saveBench(), proposalEdited(tab, main)) });
  const url = h('input', {
    value: tab.url,
    spellcheck: 'false',
    oninput: () => {
      tab.url = url.value;
      saveBench();
      renderScopeHint();
      refreshRun();
      proposalEdited(tab, main);
    },
    onkeydown: (e) => e.key === 'Enter' && !e.metaKey && !e.ctrlKey && send(),
  });
  const editor = h('textarea', {
    value: tab.raw,
    spellcheck: 'false',
    oninput: () => ((tab.raw = editor.value), saveBench(), refreshRun(), proposalEdited(tab, main), editor._onedit && editor._onedit()),
    onkeydown: (e) => {
      if (e.key === 'Tab') {
        e.preventDefault();
        const s = editor.selectionStart;
        editor.setRangeText('  ', s, editor.selectionEnd, 'end');
        tab.raw = editor.value;
        refreshRun();
        editor._onedit && editor._onedit();
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
    if (tab.asUser && toolOn('saved-users')) req.as_user = tab.asUser;
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

  const runMode = tab.panel === 'run';
  // Refresh just the run column (positions, preview, lists) as the request is
  // edited, without rebuilding the editor and losing the cursor.
  const refreshRun = () => {
    if (!runMode) return;
    clearTimeout(R.refreshT);
    R.refreshT = setTimeout(() => renderRunPanel(tab, main, runCol), 120);
  };
  // The last focused position field, so "Add position" knows where to insert.
  const marker = (field) => {
    const el = field === 'url' ? url : editor;
    const s = el.selectionStart ?? el.value.length;
    const e = el.selectionEnd ?? s;
    const before = el.value.slice(0, s);
    const sel = el.value.slice(s, e) || 'value';
    const after = el.value.slice(e);
    el.value = before + MARK + sel + MARK + after;
    if (field === 'url') tab.url = url.value;
    else tab.raw = editor.value;
    saveBench();
    renderBench(main);
  };
  const binaryNote = tab.bodyB64
    ? h('span', { class: 'r', text: `binary body (${fmtSize(b64len(tab.bodyB64))}) is sent unchanged unless you type a body` })
    : runMode
      ? h('span', { class: 'r' }, h('button', { class: 'btn sm', text: '+ Mark position', title: 'Wrap the selected text as a payload position', onclick: () => marker('editor') }))
      : h(
          'span',
          { class: 'r' },
          'headers, blank line, body · ',
          h('button', {
            class: 'link',
            text: 'Copy curl',
            title: 'Copy this request as a curl command',
            onclick: () => {
              const { headers, body } = parseRaw(editor.value);
              copyText(curlFor(method.value.trim() || 'GET', url.value.trim(), headers, body, !!tab.bodyB64 && !body));
            },
          }),
        );
  // Lens on the Bench: reads the request being edited and lets its encoded
  // values (JWTs, URL-encoding, Base64) be edited in decoded form, plus quick
  // actions on any selected text. Send mode only, to stay clear of the Run
  // panel's position markers.
  const lensStrip = runMode ? null : h('div', { class: 'benchlens' });
  const editWrap = runMode ? editor : h('div', { class: 'editorwrap' }, editor);
  const reqCol = h(
    'div',
    { class: 'rcol' },
    h('div', { class: 'lbl' }, 'Request', binaryNote),
    editWrap,
    lensStrip,
  );
  if (lensStrip) wireBenchLens(tab, editor, editWrap, lensStrip, main);
  const respCol = h('div', { class: 'rcol' });
  const runCol = h('div', { class: 'rcol runcol' });
  const panelToggle = h(
    'span',
    { class: 'seg benchpanel' },
    [
      ['send', 'Send'],
      ['run', 'Run'],
    ].map(([id, lbl]) =>
      h('button', {
        class: (tab.panel || 'send') === id ? 'on' : '',
        text: lbl,
        title: id === 'send' ? 'Send one request and inspect it' : 'Run lists of payloads through marked positions',
        onclick: () => {
          tab.panel = id;
          saveBench();
          renderBench(main);
        },
      }),
    ),
  );
  const urlMarkBtn = runMode ? h('button', { class: 'iconbtn', text: '•', title: 'Mark the selected part of the URL as a payload position', onclick: () => marker('url') }) : null;
  const body = h(
    'div',
    { class: 'pane' },
    h(
      'div',
      { class: 'rbody' },
      h('datalist', { id: 'methods' }, ['GET', 'POST', 'PUT', 'PATCH', 'DELETE', 'HEAD', 'OPTIONS'].map((m) => h('option', { value: m }))),
      h('div', { class: 'reqbar' }, panelToggle, method, h('div', { class: 'urlwrap' }, url), urlMarkBtn, !runMode && toolOn('saved-users') ? userSwitcher(tab, main) : null, runMode ? null : sendBtn),
      h('div', { id: 'scopehint' }),
      runMode ? null : h('div', { id: 'propslot' }),
      runMode
        ? sideBySide('rsplit', 'bench', reqCol, runCol)
        : sideBySide('rsplit', 'bench', reqCol, respCol),
      runMode ? h('div', { id: 'runresults' }) : historyPanel(tab, main),
      runMode ? null : h('div', { id: 'cmpslot' }),
    ),
  );
  view.append(body);
  renderScopeHint();
  if (runMode) {
    R.send = () => startRun(tab, main);
    renderRunPanel(tab, main, runCol);
    drawRunResults(tab, main);
  } else {
    drawBenchResponse(tab, respCol);
    drawCompare(tab);
    drawProposal(tab, main);
  }
}

/* =======================================================================
   Editable Lens on the Bench
   Reads the request being edited and surfaces its encoded values the way
   Lens does in Traffic — but here each one can be edited in its decoded
   form and is re-encoded straight back into the request. Any selected text
   gets the same quick actions, and the whole draft can be sent to Claude.
   ===================================================================== */

/* ---- codecs (unicode-safe, all local) ---- */
function bl_b64decode(s) {
  const t = s.replace(/-/g, '+').replace(/_/g, '/').replace(/=+$/, '');
  try {
    const bin = atob(t + '='.repeat((4 - (t.length % 4)) % 4));
    const bytes = Uint8Array.from(bin, (c) => c.charCodeAt(0));
    return new TextDecoder('utf-8', { fatal: false }).decode(bytes);
  } catch (_) {
    return null;
  }
}
function bl_b64encode(s) {
  const bytes = new TextEncoder().encode(s);
  let bin = '';
  for (const b of bytes) bin += String.fromCharCode(b);
  return btoa(bin);
}
function bl_b64urlDecode(s) {
  return bl_b64decode(s);
}
function bl_b64urlEncode(s) {
  return bl_b64encode(s).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}
function bl_b64urlBytes(bytes) {
  let bin = '';
  for (const b of bytes) bin += String.fromCharCode(b);
  return btoa(bin).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}
function bl_hexDecode(s) {
  try {
    const bytes = new Uint8Array(s.length / 2);
    for (let i = 0; i < bytes.length; i++) bytes[i] = parseInt(s.substr(i * 2, 2), 16);
    return new TextDecoder('utf-8', { fatal: true }).decode(bytes);
  } catch (_) {
    return null;
  }
}
function bl_hexEncode(s) {
  return [...new TextEncoder().encode(s)].map((b) => b.toString(16).padStart(2, '0')).join('');
}
function bl_tryJson(s) {
  try {
    const v = JSON.parse(s);
    return v && typeof v === 'object' ? v : null;
  } catch (_) {
    return null;
  }
}
function bl_pretty(s) {
  const v = bl_tryJson(s);
  return v ? JSON.stringify(v, null, 2) : s;
}
/** Mostly-printable text, the only decoded form worth offering. */
function bl_readable(s) {
  if (s == null || s.length < 4) return false;
  let printable = 0, letters = 0;
  for (const c of s) {
    const code = c.codePointAt(0);
    if (code >= 32 || c === '\n' || c === '\r' || c === '\t') printable++;
    if (/[A-Za-z0-9]/.test(c)) letters++;
  }
  const n = [...s].length;
  return printable * 100 >= n * 95 && letters * 100 >= n * 40;
}
/** HS256 signature over `data` with `key`, as base64url. */
async function bl_hs256(data, key) {
  const enc = new TextEncoder();
  const k = await crypto.subtle.importKey('raw', enc.encode(key), { name: 'HMAC', hash: 'SHA-256' }, false, ['sign']);
  const sig = await crypto.subtle.sign('HMAC', k, enc.encode(data));
  return bl_b64urlBytes(new Uint8Array(sig));
}

/** Works out what one token is and how to read and rewrite it, or null. */
function classifyToken(tok) {
  if (/^eyJ[A-Za-z0-9_-]{5,}\.eyJ[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]*$/.test(tok)) {
    const [h3, p3, sig = ''] = tok.split('.');
    const header = bl_tryJson(bl_b64urlDecode(h3) || '');
    const payload = bl_tryJson(bl_b64urlDecode(p3) || '');
    if (header && payload) {
      const alg = String(header.alg || '?');
      const notes = ['alg ' + alg];
      if (typeof payload.exp === 'number') notes.push(payload.exp * 1000 < Date.now() ? 'expired' : 'expires ' + fmtDate(payload.exp * 1000));
      else notes.push('no expiry');
      if (!sig || alg.toLowerCase() === 'none') notes.push('unsigned');
      return { kind: 'jwt', label: 'JWT', category: 'decode', notes, jwt: { header, payload, sig } };
    }
  }
  if (/%[0-9A-Fa-f]{2}/.test(tok)) {
    let dec = null;
    try {
      dec = decodeURIComponent(tok);
    } catch (_) {
      dec = null;
    }
    if (dec != null && dec !== tok) return { kind: 'url', label: 'URL-encoded', category: 'decode', notes: [], decode: () => dec, encode: (v) => encodeURIComponent(v) };
  }
  if (/^[A-Za-z0-9+/_-]{12,}={0,2}$/.test(tok)) {
    const dec = bl_b64decode(tok);
    if (dec != null && bl_readable(dec)) {
      const j = bl_tryJson(dec);
      if (j) return { kind: 'b64json', label: 'Base64 JSON', category: 'decode', notes: [], decode: () => bl_pretty(dec), encode: (v) => bl_b64encode(JSON.stringify(JSON.parse(v))) };
      if (/\d/.test(tok) && !tok.includes('/')) return { kind: 'b64', label: 'Base64', category: 'decode', notes: [], decode: () => dec, encode: (v) => bl_b64encode(v) };
    }
  }
  if (/^[0-9a-fA-F]{16,}$/.test(tok) && tok.length % 2 === 0) {
    const dec = bl_hexDecode(tok);
    if (dec != null && bl_readable(dec)) return { kind: 'hex', label: 'Hex', category: 'decode', notes: [], decode: () => dec, encode: (v) => bl_hexEncode(v) };
  }
  return null;
}

/** Best effort for an arbitrary selection the detectors did not claim. */
function genericDecode(t) {
  const trimmed = t.trim();
  const known = classifyToken(trimmed);
  if (known) return known;
  if (/%[0-9A-Fa-f]{2}/.test(t)) {
    try {
      const dec = decodeURIComponent(t);
      return { kind: 'url', label: 'URL-encoded', category: 'decode', notes: [], decode: () => dec, encode: (v) => encodeURIComponent(v) };
    } catch (_) {
      /* fall through */
    }
  }
  const b = bl_b64decode(trimmed);
  if (b != null && bl_readable(b)) return { kind: 'b64', label: 'Base64', category: 'decode', notes: [], decode: () => b, encode: (v) => bl_b64encode(v) };
  return { kind: 'text', label: 'Text', category: 'info', notes: ['not an encoded value'], decode: () => t, encode: (v) => v };
}

function benchLoc(raw, idx) {
  const lineStart = raw.lastIndexOf('\n', idx - 1) + 1;
  const nl = raw.indexOf('\n', idx);
  const line = raw.slice(lineStart, nl < 0 ? raw.length : nl);
  if (lineStart === 0) {
    const q = raw.indexOf('?');
    return q >= 0 && idx > q ? 'query' : 'request line';
  }
  const m = line.match(/^([A-Za-z0-9-]+):/);
  if (m) return 'header ' + m[1];
  return 'body';
}

/** Every editable encoded value in the draft, with its exact span. */
function scanDraft(raw) {
  const items = [];
  const re = /[^\s&?#"';,<>=]{8,}={0,2}/g;
  let m;
  while ((m = re.exec(raw)) && items.length < 30) {
    const d = classifyToken(m[0]);
    if (!d) continue;
    items.push(Object.assign(d, { start: m.index, end: m.index + m[0].length, value: m[0], loc: benchLoc(raw, m.index) }));
  }
  return items;
}

/* ---- wiring ---- */
function wireBenchLens(tab, editor, editWrap, strip, main) {
  const qbar = buildQbar(tab, editor, strip, main);
  editWrap.appendChild(qbar);
  let t;
  const redraw = () => drawBenchLens(tab, editor, strip, main);
  editor._onedit = () => {
    clearTimeout(t);
    t = setTimeout(redraw, 220);
    qbar.classList.remove('show');
  };
  const poke = () => qbar.classList.toggle('show', editor.selectionStart !== editor.selectionEnd);
  editor.addEventListener('mouseup', () => setTimeout(poke, 0));
  editor.addEventListener('keyup', (e) => {
    if (e.shiftKey || e.key.startsWith('Arrow') || e.key === 'a') setTimeout(poke, 0);
  });
  editor.addEventListener('scroll', () => qbar.classList.remove('show'));
  redraw();
}

function drawBenchLens(tab, editor, strip, main) {
  const items = scanDraft(editor.value);
  const slot = h('div', { class: 'blslot' });
  const chips = items.length
    ? items.map((it) => benchChip(it, tab, editor, strip, slot, main))
    : h('span', { class: 'blempty', text: 'Nothing to decode here yet. Select any value for quick actions.' });
  clear(strip, h('div', { class: 'spots blspots' }, h('span', { class: 'spotlbl', text: 'Spotted' }), chips, askDraftButton(tab, editor, null)), slot);
  strip._slot = slot;
}

function benchChip(it, tab, editor, strip, slot, main) {
  const chip = h(
    'button',
    {
      class: 'spot c-' + it.category,
      title: it.label + ' · ' + it.loc,
      onclick: () => {
        const was = chip.classList.contains('on');
        for (const c of strip.querySelectorAll('.spot')) c.classList.remove('on');
        if (was) return clear(slot);
        chip.classList.add('on');
        clear(slot, buildValueCard(it, { start: it.start, end: it.end }, editor, tab, strip, main, true));
      },
    },
    h('i'),
    h('span', { class: 'sl', text: it.label }),
    h('span', { class: 'sw', text: it.loc }),
    h('span', { class: 'edb', text: 'Edit' }),
  );
  return chip;
}

/** Splice `enc` into the editor over `span`, keeping the span in sync. */
function spliceEditor(editor, tab, span, enc) {
  editor.value = editor.value.slice(0, span.start) + enc + editor.value.slice(span.end);
  span.end = span.start + enc.length;
  tab.raw = editor.value;
  saveBench();
}
function blFlash(el) {
  el.classList.add('on');
  setTimeout(() => el.classList.remove('on'), 1100);
}

function buildValueCard(it, span, editor, tab, strip, main, editable) {
  const needle = it.value.replace(/"/g, '').slice(0, 120);
  const head = h(
    'div',
    { class: 'sdh' },
    h('b', { text: it.label }),
    h('span', { class: 'muted', text: ' in ' + (it.loc || 'selection') }),
    h('span', { class: 'sdact' }, needle.length >= 4 ? h('button', { class: 'btn sm', text: 'Find in traffic', onclick: () => setQuery('"' + needle + '"') }) : null, askDraftButton(tab, editor, it.value)),
    h('button', { class: 'blx', text: '✕', title: 'Close', onclick: () => clear(strip._slot) }),
  );
  const notes = it.notes && it.notes.length ? h('div', { class: 'sdnotes' }, it.notes.map((n) => h('span', { class: 'sdnote' + (/^(unsigned|expired)/.test(n) ? ' warn' : ''), text: n }))) : null;
  if (it.kind === 'jwt') return h('div', { class: 'spotdetail' }, head, notes, jwtEditor(it, span, editor, tab));
  const live = h('span', { class: 'bllive', text: '✓ re-encoded' });
  const ta = h('textarea', { class: 'sdv blv', spellcheck: 'false', value: it.decode() });
  if (!editable) ta.readOnly = true;
  ta.addEventListener('input', () => {
    try {
      spliceEditor(editor, tab, span, it.encode(ta.value));
      blFlash(live);
    } catch (_) {
      /* invalid mid-edit (e.g. half-typed JSON); wait for a valid value */
    }
  });
  return h('div', { class: 'spotdetail' }, head, h('div', { class: 'sdk' }, editable ? 'Decoded — edit to rewrite the request' : 'Decoded', live), ta, editable ? null : h('div', { class: 'dnote blnote', text: 'Read-only preview.' }));
}

/** The JWT editor: edit the claims (or header) as JSON; the token in the
 *  request is rebuilt live. The signature is kept as-is and the token marked
 *  unsigned, unless a signing key is given, in which case it is re-signed. */
function jwtEditor(it, span, editor, tab) {
  const st = { part: 'payload', key: '' };
  const live = h('span', { class: 'bllive' });
  const ta = h('textarea', { class: 'sdv blv bljwt', spellcheck: 'false' });
  const load = () => (ta.value = JSON.stringify(st.part === 'payload' ? it.jwt.payload : it.jwt.header, null, 2));
  load();
  const mk = (p, label) =>
    h('button', {
      class: st.part === p ? 'on' : '',
      text: label,
      onclick: () => {
        st.part = p;
        for (const b of seg.children) b.classList.remove('on');
        seg.children[p === 'payload' ? 0 : 1].classList.add('on');
        load();
      },
    });
  const seg = h('span', { class: 'seg blseg' }, mk('payload', 'Claims'), mk('header', 'Header'));
  const keyIn = h('input', { class: 'blkey', placeholder: 'HS256 key to re-sign (optional)', spellcheck: 'false' });

  const rebuild = async () => {
    const h3 = bl_b64urlEncode(JSON.stringify(it.jwt.header));
    const p3 = bl_b64urlEncode(JSON.stringify(it.jwt.payload));
    let sig = it.jwt.sig, signed = false;
    if (st.key) {
      try {
        sig = await bl_hs256(h3 + '.' + p3, st.key);
        signed = true;
      } catch (_) {
        signed = false;
      }
    }
    spliceEditor(editor, tab, span, h3 + '.' + p3 + '.' + sig);
    live.textContent = signed ? '✓ re-signed with key' : '✓ rebuilt · signature not re-signed';
    live.classList.toggle('warn', !signed);
    blFlash(live);
  };
  ta.addEventListener('input', () => {
    const obj = bl_tryJson(ta.value);
    if (!obj) return;
    if (st.part === 'payload') it.jwt.payload = obj;
    else it.jwt.header = obj;
    rebuild();
  });
  keyIn.addEventListener('input', () => {
    st.key = keyIn.value.trim();
    rebuild();
  });
  return h(
    'div',
    { class: 'bljwtbox' },
    h('div', { class: 'sdk' }, seg, live),
    ta,
    keyIn,
    h('div', { class: 'dnote blnote', text: 'Editing a claim changes the token, so the original signature no longer matches — which is exactly what you want to test whether the server verifies it. Add the key to sign a valid token.' }),
  );
}

/* ---- selection quick actions ---- */
/** A stable id for a Bench tab, so Claude's suggested edits find their way back to it. */
function draftId(tab) {
  if (!tab.did) {
    tab.did = 'd' + Date.now().toString(36) + Math.random().toString(36).slice(2, 8);
    saveBench();
  }
  return tab.did;
}
function draftSubject(tab, editor, selection) {
  const { headers, body } = parseRaw(editor.value);
  return { kind: 'draft', method: (tab.method || 'GET').toUpperCase(), url: tab.url || '', headers, body, selection: selection || null, draft_id: draftId(tab) };
}

/* ---- Claude's suggested edits ----
   Claude can answer a question about a draft with a concrete edited request
   (the propose_bench_edit tool). The engine only keeps it; here it is shown
   as a diff against the draft as it is now, and nothing changes until the
   user presses Apply. Apply only rewrites the draft: sending stays the
   user's own click on Send. */

/** Folds long unchanged runs, keeping a little context around changes. */
function foldLines(lines, ctx = 3) {
  const out = [];
  let i = 0;
  while (i < lines.length) {
    if (lines[i].op !== 'same') {
      out.push(lines[i++]);
      continue;
    }
    let j = i;
    while (j < lines.length && lines[j].op === 'same') j++;
    const run = j - i;
    const head = i === 0 ? 0 : ctx;
    const tail = j === lines.length ? 0 : ctx;
    if (run > head + tail + 1) {
      out.push(...lines.slice(i, i + head), { op: 'gap', n: run - head - tail }, ...lines.slice(j - tail, j));
    } else out.push(...lines.slice(i, j));
    i = j;
  }
  return out;
}
function propPre(lines) {
  return h(
    'pre',
    { class: 'propdiff' },
    foldLines(lines).map((l) =>
      l.op === 'gap'
        ? h('span', { class: 'dl gap', text: `… ${l.n} unchanged line${l.n === 1 ? '' : 's'}` })
        : h('span', { class: 'dl' + (l.op === 'add' ? ' add' : l.op === 'del' ? ' del' : ''), text: (l.op === 'add' ? '+ ' : l.op === 'del' ? '− ' : '  ') + l.text }),
    ),
  );
}
/** Named values (headers, query or form fields) as removed/added lines. */
function fieldLines(fields, sep) {
  const out = [];
  for (const f of fields) {
    if (f.old != null) out.push({ op: 'del', text: f.name + sep + f.old });
    if (f.new != null) out.push({ op: 'add', text: f.name + sep + f.new });
  }
  return out;
}
function propSection(title, ...kids) {
  return h('div', { class: 'propsec' }, h('div', { class: 'propsech', text: title }), ...kids);
}

function proposalCard(tab, main, p, d, count) {
  const secs = [];
  if (d.same) secs.push(h('div', { class: 'propsame', text: 'This matches your draft as it is now.' }));
  if (d.method || d.url) {
    const m0 = d.method ? d.method.old : d.proposed.method;
    const u0 = d.url ? d.url.old : d.proposed.url;
    secs.push(propSection('Request line', propPre([{ op: 'del', text: `${m0} ${u0}` }, { op: 'add', text: `${d.proposed.method} ${d.proposed.url}` }])));
  }
  if (d.query.length) secs.push(propSection('Query, decoded', propPre(fieldLines(d.query, ' = '))));
  if (d.headers.length) secs.push(propSection('Headers', propPre(fieldLines(d.headers, ': '))));
  if (d.body.changed) {
    const label = { json: 'Body, as JSON', form: 'Body, form fields decoded', text: 'Body' }[d.body.view] || 'Body';
    secs.push(propSection(label, propPre(d.body.view === 'form' ? fieldLines(d.body.fields, ' = ') : d.body.lines)));
  }
  for (const t of d.tokens) {
    secs.push(
      propSection(
        'JWT in ' + t.location + ', decoded',
        t.notes.length ? h('div', { class: 'sdnotes propnotes' }, t.notes.map((n) => h('span', { class: 'sdnote' + (/^(unsigned|not re-signed|signature)/.test(n) ? ' warn' : ''), text: n }))) : null,
        propPre(t.lines),
      ),
    );
  }
  const discard = async () => {
    try {
      await api(`/api/bench/proposals/${p.id}`, { method: 'DELETE' });
    } catch (_) {}
    drawProposal(tab, main);
  };
  const apply = async () => {
    const q = d.proposed;
    tab.undo = { method: tab.method, url: tab.url, raw: tab.raw };
    tab.method = q.method;
    tab.url = q.url;
    tab.raw = q.headers.map(([k, v]) => `${k}: ${v}`).join('\n') + '\n\n' + q.body;
    saveBench();
    try {
      await api(`/api/bench/proposals/${p.id}`, { method: 'DELETE' });
    } catch (_) {}
    renderBench(main);
    toast('Applied to the draft. Nothing was sent: press Send when you are ready.', 'ok');
  };
  const meta = [p.from && p.from !== 'you' ? 'from ' + p.from : null, fmtTime(p.created), count > 1 ? `${count - 1} more waiting` : null].filter(Boolean).join(' · ');
  return h(
    'div',
    { class: 'propcard' },
    h('div', { class: 'proph' }, h('span', { class: 'askico', text: '✦' }), h('b', { text: 'Claude suggests an edit' }), h('span', { class: 'muted', text: meta })),
    p.summary ? h('p', { class: 'propsum', text: p.summary }) : null,
    h('div', { class: 'propbody' }, secs),
    h(
      'div',
      { class: 'propfoot' },
      h('span', { class: 'muted', text: 'Apply only changes the draft. Nothing is sent until you press Send.' }),
      h('button', { class: 'btn sm', text: 'Discard', onclick: discard }),
      h('button', { class: 'btn sm primary', text: 'Apply to draft', disabled: d.same, onclick: apply }),
    ),
  );
}

function undoBar(tab, main) {
  if (!tab.undo) return null;
  const undo = () => {
    Object.assign(tab, tab.undo);
    tab.undo = null;
    saveBench();
    renderBench(main);
  };
  return h(
    'div',
    { class: 'propundo' },
    h('span', { class: 'askico', text: '✦' }),
    h('span', { text: "Claude's edit is in the draft. Nothing has been sent." }),
    h('button', { class: 'btn sm', text: 'Undo', onclick: undo }),
    h('button', { class: 'blx', text: '✕', title: 'Dismiss', onclick: () => ((tab.undo = null), saveBench(), drawProposal(tab, main)) }),
  );
}

/** Shows the newest suggestion for the active tab, diffed against its draft now. */
async function drawProposal(tab, main) {
  const slot = $('#propslot');
  if (!slot) return;
  let list = [];
  if (tab.did) {
    try {
      list = (await api('/api/bench/proposals?draft=' + encodeURIComponent(tab.did))).proposals || [];
    } catch (_) {
      list = [];
    }
  }
  if (R.tabs[R.active] !== tab || !document.body.contains(slot)) return;
  R.propSeen = tab.did + ':' + list.map((p) => p.id).join(',');
  if (!list.length) return clear(slot, undoBar(tab, main));
  const { headers, body } = parseRaw(tab.raw || '');
  let v;
  try {
    v = await api(`/api/bench/proposals/${list[0].id}/diff`, { method: 'POST', body: { method: (tab.method || 'GET').toUpperCase(), url: tab.url || '', headers, body } });
  } catch (_) {
    return clear(slot, undoBar(tab, main));
  }
  if (R.tabs[R.active] !== tab || !document.body.contains(slot)) return;
  clear(slot, proposalCard(tab, main, v.proposal, v.diff, list.length));
}

/** Re-compares after the user edits the draft, while a suggestion is shown. */
function proposalEdited(tab, main) {
  if (!$('#propslot .propcard')) return;
  clearTimeout(R.propT);
  R.propT = setTimeout(() => drawProposal(tab, main), 400);
}

// Suggestions can arrive while the Bench is open (from the in-app answer or a
// Claude Code session in a terminal), so the active tab checks for new ones.
setInterval(() => {
  const tab = R.tabs[R.active];
  if (S.view !== 'bench' || document.hidden || !tab || !tab.did || !$('#propslot')) return;
  api('/api/bench/proposals?draft=' + encodeURIComponent(tab.did))
    .then((v) => {
      const seen = tab.did + ':' + (v.proposals || []).map((p) => p.id).join(',');
      if (seen !== R.propSeen) drawProposal(tab, $('#main'));
    })
    .catch(() => {});
}, 3000);
function askDraftButton(tab, editor, selection) {
  return h(
    'button',
    { class: 'btn sm askbtn blask', hidden: !agentsOn(), title: selection ? 'Ask Claude about the selected text' : 'Ask Claude about this request', onclick: () => askClaude(draftSubject(tab, editor, selection)) },
    h('span', { class: 'askico', text: '✦' }),
    selection ? ' Ask Claude' : ' Ask about this request',
  );
}

function buildQbar(tab, editor, strip, main) {
  const sel = () => editor.value.slice(editor.selectionStart, editor.selectionEnd);
  const openSel = (editable) => {
    const t = sel();
    if (!t) return;
    const it = genericDecode(t);
    it.value = t;
    it.loc = 'selection';
    clear(strip._slot, buildValueCard(it, { start: editor.selectionStart, end: editor.selectionEnd }, editor, tab, strip, main, editable));
    strip.scrollIntoView({ block: 'nearest' });
  };
  const applyEnc = (fn, label) => {
    const t = sel();
    if (!t) return;
    let out;
    try {
      out = fn(t);
    } catch (_) {
      return toast('Could not ' + label, 'err');
    }
    if (out == null) return toast('Could not ' + label, 'err');
    spliceEditor(editor, tab, { start: editor.selectionStart, end: editor.selectionEnd }, out);
    toast(label + ' ✓', 'ok');
    editor._onedit && editor._onedit();
  };
  const menu = h(
    'div',
    { class: 'qmenu' },
    h('button', { text: 'URL-encode', onclick: () => applyEnc((t) => encodeURIComponent(t), 'URL-encoded') }),
    h('button', { text: 'URL-decode', onclick: () => applyEnc((t) => decodeURIComponent(t), 'URL-decoded') }),
    h('button', { text: 'Base64', onclick: () => applyEnc((t) => bl_b64encode(t), 'Base64-encoded') }),
    h('button', { text: 'Base64-decode', onclick: () => applyEnc((t) => bl_b64decode(t), 'Base64-decoded') }),
  );
  const encBtn = h('button', {
    text: 'Encode ▾',
    onclick: (e) => {
      e.stopPropagation();
      menu.classList.toggle('show');
    },
  });
  document.addEventListener('click', (e) => {
    if (!e.target.closest('.qcaret')) menu.classList.remove('show');
  });
  return h(
    'div',
    { class: 'qbar' },
    h('button', { text: '◇ Decode', onclick: () => openSel(false) }),
    h('button', { text: '✎ Edit decoded', onclick: () => openSel(true) }),
    h('span', { class: 'qcaret' }, encBtn, menu),
    h('span', { class: 'qsep' }),
    h('button', { class: 'ai', onclick: () => askClaude(draftSubject(tab, editor, sel())) }, '✦ Ask Claude'),
  );
}

/* ----- Bench payload runs ----- */

const MARK = '•'; // •, the position marker, matched to the engine.
let LIST_CATALOG = null;
// Which position cards have their full list picker expanded, per tab. Kept off
// the tab object so it is never persisted to the project file.
const RUN_MORE = new WeakMap();
function moreSet(tab) {
  let s = RUN_MORE.get(tab);
  if (!s) RUN_MORE.set(tab, (s = new Set()));
  return s;
}

async function loadLists() {
  if (LIST_CATALOG) return LIST_CATALOG;
  try {
    const v = await api('/api/run/lists');
    LIST_CATALOG = v.lists || [];
  } catch (_) {
    LIST_CATALOG = [];
  }
  return LIST_CATALOG;
}

function countPositions(tab) {
  const n = ((tab.url || '').split(MARK).length - 1 + (tab.raw || '').split(MARK).length - 1) / 2;
  return Number.isInteger(n) ? n : Math.floor(n); // odd counts mean an unclosed marker
}

function runCfg(tab) {
  if (!tab.run) tab.run = { mode: 'sweep', lists: [], base: false, max: '', delay: '50' };
  return tab.run;
}

/** The marked positions of a tab, in the order the engine fills them (URL
 * first, then the raw headers/body). Each carries where it sits and a guess
 * at what kind of value it is, so the panel can label it and suggest a list. */
function positionsOf(tab) {
  const out = [];
  const scan = (text, locate) => {
    const parts = (text || '').split(MARK);
    let before = '';
    for (let i = 0; i < parts.length; i++) {
      if (i % 2 === 1) out.push(locate(parts[i], before));
      before += parts[i] + (i < parts.length - 1 ? MARK : '');
    }
  };
  scan(tab.url, (value, before) => {
    const q = before.includes('?');
    const ctx = q ? (before.match(/[?&]([^=&]*)=[^?&=]*$/) || [])[1] || '' : '';
    return { value, where: q ? 'url-query' : 'url-path', ctx };
  });
  const raw = tab.raw || '';
  const headEnd = raw.indexOf('\n\n');
  scan(raw, (value, before) => {
    const inBody = headEnd >= 0 && before.length > headEnd + 1;
    const line = before.slice(before.lastIndexOf('\n') + 1);
    const ctx = inBody ? '' : (line.match(/^([A-Za-z0-9-]+)\s*:/) || [])[1] || '';
    return { value, where: inBody ? 'body' : 'header', ctx };
  });
  return out.map((p) => ({ ...p, ...detectKind(p) }));
}

/** A plain-language name for where a position sits. */
function whereLabel(where) {
  return { 'url-path': 'in the URL path', 'url-query': 'in the query', header: 'in a header', body: 'in the body' }[where] || '';
}

/** Guesses what a position holds and which lists suit it. `suggest` is a
 * priority order of list ids; only those actually in the library are shown. */
function detectKind(p) {
  const v = (p.value || '').trim();
  const ctx = (p.ctx || '').toLowerCase();
  const parts = v.split('.');
  const b64url = (s) => s.length > 0 && /^[A-Za-z0-9_-]+$/.test(s);
  if (p.where === 'header' && ctx === 'authorization') return { label: 'auth token', suggest: ['input-probes'] };
  if (p.where === 'header' && ctx === 'content-type') return { label: 'content type', suggest: ['content-types'] };
  if (p.where === 'header' && ctx === 'user-agent') return { label: 'user agent', suggest: ['user-agents'] };
  if (parts.length === 3 && parts[0].startsWith('ey') && parts.slice(0, 2).every(b64url)) return { label: 'JWT token', suggest: ['input-probes'] };
  if (/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(v)) return { label: 'UUID', suggest: ['id-formats', 'numbers-1-100'] };
  if (/^-?\d+$/.test(v)) {
    const idish = /(^id$|_id$|id$)/.test(ctx) || p.where === 'url-path';
    return { label: idish ? 'identifier' : 'number', suggest: idish ? ['id-formats', 'numbers-1-100', 'digits'] : ['numbers-1-100', 'digits'] };
  }
  if (/^(true|false)$/i.test(v)) return { label: 'boolean', suggest: ['booleans'] };
  if (p.where === 'url-query' && /^(user|username|login|account|name|email)$/.test(ctx)) return { label: 'username', suggest: ['common-usernames'] };
  if (p.where === 'url-path') return { label: 'path segment', suggest: ['common-paths', 'api-paths', 'common-params'] };
  if (v.length >= 12 && /[A-Za-z]/.test(v) && /[0-9+/=_-]/.test(v) && /^[A-Za-z0-9+/=_-]+$/.test(v)) return { label: 'encoded value', suggest: ['input-probes'] };
  return { label: 'value', suggest: ['input-probes', 'common-params', 'common-usernames'] };
}

/** A number range centred on `n`, the smart default for a numeric id. */
function rangeAround(n) {
  const span = 10;
  return { kind: 'range', from: n - span, to: n + span, step: 1 };
}

/** The list title for a built-in id, for a suggestion chip label. */
function listTitle(id) {
  const l = (LIST_CATALOG || []).find((x) => x.id === id);
  return l ? l.title : id;
}

/** Ready-to-use list choices for a position, best first. Each is a chip the
 * user can apply in one click; the first is the recommended pre-pick, applied
 * automatically when the position is created. Numeric ids get a range centred
 * on the current value — the situation-aware default — ahead of the generic
 * built-in lists. */
function smartSuggestions(pos) {
  const have = new Set((LIST_CATALOG || []).map((l) => l.id));
  const out = [];
  const v = (pos.value || '').trim();
  if (/^-?\d+$/.test(v) && pos.label !== 'content type' && v.length <= 15) {
    const n = Number(v);
    const cfg = rangeAround(n);
    const { count } = listPreview(cfg);
    out.push({ key: 'around:' + n, title: `Around ${v}`, cfg, count });
  }
  for (const id of pos.suggest || []) {
    if (!have.has(id)) continue;
    const cfg = { kind: 'builtin', id };
    out.push({ key: 'builtin:' + id, title: listTitle(id), cfg, count: listPreview(cfg).count });
  }
  return out.slice(0, 4);
}

/** True when a list config is the one a suggestion would apply. */
function cfgMatches(cfg, s) {
  if (!cfg || cfg.kind !== s.cfg.kind) return false;
  if (s.cfg.kind === 'builtin') return cfg.id === s.cfg.id;
  if (s.cfg.kind === 'range') return Number(cfg.from) === s.cfg.from && Number(cfg.to) === s.cfg.to && Number(cfg.step || 1) === s.cfg.step;
  return false;
}

/** The spans of a URL or raw request that are worth varying — path segments,
 * query values, a few header values, and body values. Offsets are into `text`
 * as given; spans that fall inside an existing position marker are left out by
 * the caller. `which` is 'url' or 'raw'. */
function candidateSpans(text, which) {
  const spans = [];
  const add = (s, e) => {
    if (e > s && !text.slice(s, e).includes(MARK)) spans.push({ start: s, end: e });
  };
  if (which === 'url') {
    const q = text.indexOf('?');
    const pathEnd = q < 0 ? text.length : q;
    const m = text.match(/^[a-z][a-z0-9+.-]*:\/\/[^/]+/i);
    let seg = m ? m[0].length : 0;
    for (let k = seg; k <= pathEnd; k++) {
      if (k === pathEnd || text[k] === '/') {
        add(seg, k);
        seg = k + 1;
      }
    }
    if (q >= 0) {
      const re = /([?&])([^=&]+)=([^&]*)/g;
      re.lastIndex = q;
      let mm;
      while ((mm = re.exec(text))) {
        const vs = mm.index + 1 + mm[2].length + 1;
        add(vs, vs + mm[3].length);
      }
    }
    return spans;
  }
  const nl = text.indexOf('\n\n');
  const headEnd = nl < 0 ? text.length : nl;
  const hre = /^([A-Za-z0-9-]+):[ \t]*(.*)$/gm;
  let hm;
  while ((hm = hre.exec(text)) && hm.index < headEnd) {
    const name = hm[1].toLowerCase();
    const val = hm[2];
    const valStart = hm.index + hm[0].length - val.length;
    if (name === 'authorization') {
      const sm = val.match(/^(\S+)[ \t]+(\S.*)$/);
      if (sm) {
        const ts = valStart + sm[0].length - sm[2].length;
        add(ts, ts + sm[2].trimEnd().length);
      } else if (val.trim()) add(valStart, valStart + val.trimEnd().length);
    } else if (name === 'user-agent' || name === 'content-type') {
      if (val.trim()) add(valStart, valStart + val.trimEnd().length);
    }
  }
  if (nl >= 0) {
    const bodyStart = nl + 2;
    const body = text.slice(bodyStart);
    const trimmed = body.trimStart();
    if (trimmed.startsWith('{') || trimmed.startsWith('[')) {
      const jre = /:[ \t]*(?:"((?:[^"\\]|\\.)*)"|(-?\d+(?:\.\d+)?|true|false))/g;
      let jm;
      while ((jm = jre.exec(body))) {
        if (jm[1] !== undefined) {
          const vs = bodyStart + jm.index + jm[0].indexOf('"') + 1;
          add(vs, vs + jm[1].length);
        } else {
          const vs = bodyStart + jm.index + jm[0].length - jm[2].length;
          add(vs, vs + jm[2].length);
        }
      }
    } else if (/^[^=&\s]+=/.test(trimmed)) {
      const fre = /([^=&\n]+)=([^&\n]*)/g;
      let fm;
      while ((fm = fre.exec(body))) {
        const vs = bodyStart + fm.index + fm[1].length + 1;
        add(vs, vs + fm[2].length);
      }
    }
  }
  return spans;
}

/** Wraps the span [start,end) of the tab's URL or raw in position markers,
 * turning a candidate value into a run position, and re-renders the Bench. */
function addPositionAt(tab, which, start, end, main) {
  const key = which === 'url' ? 'url' : 'raw';
  const text = tab[key] || '';
  tab[key] = text.slice(0, start) + MARK + text.slice(start, end) + MARK + text.slice(end);
  // The list for the new position is filled with its recommended default by
  // renderRunPanel's sizing, so the run is ready without hunting for a list.
  saveBench();
  renderBench(main);
}

/** Removes the nth position's markers (0-based, engine fill order). */
function removePosition(tab, n, main) {
  let seen = 0;
  const strip = (text) => {
    const parts = (text || '').split(MARK);
    let out = '';
    for (let i = 0; i < parts.length; i++) {
      if (i % 2 === 1) {
        if (seen === n) {
          out += parts[i];
          seen++;
          continue;
        }
        out += MARK + parts[i] + (i < parts.length - 1 ? MARK : '');
        seen++;
      } else out += parts[i];
    }
    return out;
  };
  tab.url = strip(tab.url);
  // `seen` continues into the raw so the index is global across URL + raw.
  tab.raw = strip(tab.raw);
  saveBench();
  renderBench(main);
}

/** A short sample and total count for a list config, for the preview line. */
function listPreview(cfg) {
  if (cfg.kind === 'range') {
    const from = Number(cfg.from ?? 1);
    const to = Number(cfg.to ?? 100);
    const step = Number(cfg.step || 1) || 1;
    const sample = [];
    for (let v = from; (step > 0 ? v <= to : v >= to) && sample.length < 6; v += step) sample.push(String(v));
    const count = step !== 0 ? Math.max(0, Math.floor((to - from) / step) + 1) : 0;
    return { sample, count };
  }
  if (cfg.kind === 'values') {
    const vals = (cfg.values || []).filter((v) => v !== '');
    return { sample: vals.slice(0, 6), count: vals.length };
  }
  const l = (LIST_CATALOG || []).find((x) => x.id === cfg.id);
  return { sample: (l && l.sample) || [], count: l ? l.count : 0 };
}

/** One list picker: a kind select, the detail for that kind, and a live
 * preview of the values. `onchange` persists; `redraw` re-renders the card. */
function listPicker(cfg, onchange, redraw) {
  const kind = cfg.kind || 'builtin';
  const sel = h(
    'select',
    { class: 'listkind', onchange: () => pick(sel.value) },
    h('option', { value: 'builtin', text: 'From a list', selected: kind === 'builtin' }),
    h('option', { value: 'range', text: 'Number range', selected: kind === 'range' }),
    h('option', { value: 'values', text: 'Type my own', selected: kind === 'values' }),
  );
  const pick = (k) => {
    cfg.kind = k;
    if (k === 'builtin' && !cfg.id) cfg.id = (LIST_CATALOG[0] || {}).id;
    onchange();
    redraw();
  };
  const detail = h('span', { class: 'listdetail' });
  if (cfg.kind === 'range') {
    const num = (key, dflt) => h('input', { class: 'num', type: 'number', value: cfg[key] ?? dflt, oninput: (e) => ((cfg[key] = Number(e.target.value)), onchange(), redrawPreview()) });
    clear(detail, 'from ', num('from', 1), ' to ', num('to', 100), ' step ', num('step', 1));
  } else if (cfg.kind === 'values') {
    const ta = h('textarea', { class: 'listvals', placeholder: 'One value per line', value: (cfg.values || []).join('\n'), oninput: () => ((cfg.values = ta.value.split('\n')), onchange(), redrawPreview()) });
    clear(detail, ta);
  } else {
    const d = h(
      'select',
      { onchange: () => ((cfg.id = d.value), onchange(), redrawPreview()) },
      (LIST_CATALOG || []).map((l) => h('option', { value: l.id, text: `${l.title} (${l.count})${l.builtin ? '' : ' · ' + l.pack}`, selected: l.id === cfg.id })),
    );
    clear(detail, d);
  }
  const preview = h('div', { class: 'listprev' });
  const redrawPreview = () => {
    const { sample, count } = listPreview(cfg);
    clear(
      preview,
      h('span', { class: 'prevcount', text: count ? `${count} value${count === 1 ? '' : 's'}` : 'no values' }),
      sample.length ? h('span', { class: 'prevvals', text: sample.map((v) => (v === '' ? '∅' : v)).join('  ·  ') + (count > sample.length ? '  …' : '') }) : null,
    );
  };
  redrawPreview();
  return h('div', { class: 'listpick' }, h('div', { class: 'pickrow' }, sel, detail), preview);
}

/** The request drawn as a live surface: every value worth varying is a
 * clickable token, and every marked position is a numbered chip you can
 * remove. This is how positions are made — point at a value, no manual
 * marking. Positions are numbered in the engine's fill order (URL, then the
 * raw request). */
function runCanvas(tab, main) {
  let posIdx = 0;
  const draw = (text, which) => {
    const nodes = [];
    const markers = [];
    let open = -1;
    for (let i = 0; i < text.length; i++) {
      if (text[i] !== MARK) continue;
      if (open < 0) open = i;
      else {
        markers.push({ start: open, end: i + 1, inner: text.slice(open + 1, i) });
        open = -1;
      }
    }
    const inMarker = (s, e) => markers.some((m) => s < m.end && e > m.start);
    const regions = markers.map((m) => ({ type: 'pos', ...m }));
    for (const c of candidateSpans(text, which)) if (!inMarker(c.start, c.end)) regions.push({ type: 'cand', start: c.start, end: c.end });
    regions.sort((a, b) => a.start - b.start);
    let cur = 0;
    for (const r of regions) {
      if (r.start > cur) nodes.push(document.createTextNode(text.slice(cur, r.start)));
      if (r.type === 'pos') {
        const idx = posIdx++;
        nodes.push(
          h(
            'span',
            { class: 'cvchip', title: `Position ${idx + 1}` },
            h('span', { class: 'cvnum' }, h('span', { class: 'pnudge', text: String(idx + 1) })),
            h('span', { class: 'cvval', text: r.inner === '' ? '∅' : r.inner }),
            h('button', { class: 'cvx', text: '×', title: 'Stop varying this', onclick: (e) => (e.stopPropagation(), removePosition(tab, idx, main)) }),
          ),
        );
      } else {
        const s = r.start, e = r.end;
        nodes.push(h('span', { class: 'cvcand', title: 'Vary this value', onclick: () => addPositionAt(tab, which, s, e, main) }, text.slice(s, e)));
      }
      cur = r.end;
    }
    if (cur < text.length) nodes.push(document.createTextNode(text.slice(cur)));
    return nodes;
  };
  return h(
    'div',
    { class: 'runcanvas' },
    h('div', { class: 'cvline' }, h('span', { class: 'cvmethod', text: tab.method || 'GET' }), ' ', ...draw(tab.url || '', 'url')),
    (tab.raw || '').trim() ? h('div', { class: 'cvraw' }, ...draw(tab.raw || '', 'raw')) : null,
  );
}

/** A plain-language name for a list config, for the run summary. */
function listDesc(cfg) {
  const { count } = listPreview(cfg);
  if (cfg.kind === 'range') return `${count} nearby value${count === 1 ? '' : 's'}`;
  if (cfg.kind === 'values') return `${count} value${count === 1 ? '' : 's'} you typed`;
  return `the ${listTitle(cfg.id)} list`;
}

/** An estimate of how many requests the run will send, matching the engine's
 * mode semantics closely enough for a summary line. */
function estimateRequests(positions, cfg, lists) {
  const n = positions.length;
  if (!n) return 0;
  const counts = (cfg.mode === 'sweep' ? [lists[0]] : lists.slice(0, n)).map((c) => listPreview(c || {}).count);
  let total;
  if (cfg.mode === 'matrix') total = counts.reduce((a, b) => a * b, 1);
  else if (cfg.mode === 'parallel') total = Math.min(...counts);
  else total = n <= 1 ? counts[0] || 0 : (counts[0] || 0) * n;
  return total + (cfg.base ? 1 : 0);
}

/** The one-sentence description of what Start will do. */
function runSummary(positions, cfg, lists) {
  const n = positions.length;
  const req = estimateRequests(positions, cfg, lists);
  const tail = ` — about ${req} request${req === 1 ? '' : 's'}.`;
  if (n === 1) return `Vary the ${positions[0].label} through ${listDesc(lists[0])}${tail}`;
  if (cfg.mode === 'matrix') return `Try every combination across ${n} positions${tail}`;
  if (cfg.mode === 'parallel') return `Step ${n} positions together, each through its own list${tail}`;
  return `Vary ${n} positions one at a time through ${listDesc(lists[0])}${tail}`;
}

/** One position's list controls: a header, one-tap suggested lists (the first
 * is the smart default), a "More options" disclosure holding the full picker,
 * and a live preview of the values. */
function runCard(tab, pos, cfg, i, redraw) {
  const persist = () => saveBench();
  const apply = (s) => {
    for (const k of Object.keys(cfg)) delete cfg[k];
    Object.assign(cfg, JSON.parse(JSON.stringify(s.cfg)));
    persist();
    redraw();
  };
  const suggs = smartSuggestions(pos);
  const chips = suggs.map((s) => {
    const on = cfgMatches(cfg, s);
    return h('button', { class: 'suggchip' + (on ? ' on' : ''), title: `${s.count} value${s.count === 1 ? '' : 's'}`, onclick: () => apply(s) }, (on ? '✓ ' : '') + s.title);
  });
  const open = moreSet(tab).has(i);
  const moreBtn = h(
    'button',
    {
      class: 'morebtn' + (open ? ' on' : ''),
      onclick: () => {
        const m = moreSet(tab);
        open ? m.delete(i) : m.add(i);
        redraw();
      },
    },
    open ? 'Fewer options' : 'More options',
  );
  const { sample, count } = listPreview(cfg);
  return h(
    'div',
    { class: 'runcard' },
    h(
      'div',
      { class: 'cardhead' },
      h('span', { class: 'posnum' }, h('span', { class: 'pnudge', text: String(i + 1) })),
      h('span', { class: 'poskind', text: pos.label }),
      h('span', { class: 'poswhere', text: whereLabel(pos.where) }),
      h('span', { class: 'posval', title: pos.value, text: pos.value === '' ? '∅' : pos.value }),
    ),
    h('div', { class: 'cardsugg' }, ...chips, moreBtn),
    open ? listPicker(cfg, persist, redraw) : null,
    h(
      'div',
      { class: 'cardprev' },
      h('span', { class: 'prevcount', text: count ? `${count} value${count === 1 ? '' : 's'}` : 'no values' }),
      sample.length ? h('span', { class: 'prevvals', text: sample.map((v) => (v === '' ? '∅' : v)).join('  ·  ') + (count > sample.length ? '  …' : '') }) : null,
    ),
  );
}

async function renderRunPanel(tab, main, col) {
  await loadLists();
  if (!col.isConnected) return;
  const cfg = runCfg(tab);
  const positions = positionsOf(tab);
  const n = positions.length;
  const multi = cfg.mode !== 'sweep';
  const redraw = () => renderRunPanel(tab, main, col);
  const persist = () => saveBench();

  // Size the list array: nothing until a position exists, one per position in
  // the multi modes, one shared list for sweep. Each new slot is filled with
  // the recommended list for its position — the situation-aware default — so a
  // freshly picked position is ready to run.
  const lists = cfg.lists;
  const fallback = { kind: 'builtin', id: (LIST_CATALOG[0] || {}).id };
  const defaultFor = (i) => {
    const sug = smartSuggestions(positions[i] || positions[0] || {});
    return sug.length ? JSON.parse(JSON.stringify(sug[0].cfg)) : { ...fallback };
  };
  const need = n === 0 ? 0 : multi ? n : 1;
  while (lists.length < need) lists.push(defaultFor(lists.length));
  if (lists.length > need) lists.length = need;

  // Lead line: friendly invitation when nothing is marked, the run summary once
  // there is at least one position.
  const lead =
    n === 0
      ? h('div', { class: 'runlead' }, h('b', { text: 'Point at a value to vary it.' }), ' Click any highlighted value in the request below. Plonix picks a fitting list, so the run is ready to start.')
      : h('div', { class: 'runlead on', text: runSummary(positions, cfg, lists) });

  // The list controls under the canvas.
  let cards = null;
  if (n >= 1) {
    if (!multi) {
      // Sweep: one shared list. A single position shows its own card; several
      // positions get a compact read-only list, then one shared list card.
      if (n === 1) {
        cards = h('div', { class: 'runcards' }, runCard(tab, positions[0], lists[0], 0, redraw));
      } else {
        const rows = positions.map((p, i) =>
          h(
            'div',
            { class: 'posrow' },
            h('span', { class: 'posnum' }, h('span', { class: 'pnudge', text: String(i + 1) })),
            h('span', { class: 'poskind', text: p.label }),
            h('span', { class: 'poswhere', text: whereLabel(p.where) }),
            h('span', { class: 'posval', title: p.value, text: p.value === '' ? '∅' : p.value }),
          ),
        );
        cards = h('div', { class: 'runcards' }, h('div', { class: 'posrows' }, ...rows), runCard(tab, { ...positions[0], label: 'one shared list' }, lists[0], 0, redraw));
      }
    } else {
      cards = h('div', { class: 'runcards' }, ...positions.map((p, i) => runCard(tab, p, lists[i], i, redraw)));
    }
  }

  // Mode only matters with two or more positions.
  const modeHelp = {
    sweep: 'One position changes at a time, through one shared list.',
    parallel: 'Every position steps together, each through its own list.',
    matrix: 'Every combination of values across the positions.',
  };
  const modeSeg =
    n < 2
      ? null
      : h(
          'div',
          { class: 'runmode' },
          h('span', { class: 'plabel', text: 'How' }),
          h(
            'span',
            { class: 'seg' },
            [
              ['sweep', 'One at a time'],
              ['parallel', 'Lockstep'],
              ['matrix', 'All combinations'],
            ].map(([id, lbl]) =>
              h('button', {
                class: cfg.mode === id ? 'on' : '',
                text: lbl,
                title: modeHelp[id],
                onclick: () => {
                  cfg.mode = id;
                  saveBench();
                  redraw();
                },
              }),
            ),
          ),
          h('span', { class: 'modehelp', text: modeHelp[cfg.mode] }),
        );

  // Options are tucked away so the common path stays uncluttered.
  const base = h('input', { type: 'checkbox', checked: !!cfg.base, onchange: () => ((cfg.base = base.checked), persist(), redraw()) });
  const max = h('input', { class: 'num', type: 'number', placeholder: '1000', value: cfg.max, oninput: () => ((cfg.max = max.value), persist()) });
  const delay = h('input', { class: 'num', type: 'number', placeholder: '50', value: cfg.delay, oninput: () => ((cfg.delay = delay.value), persist()) });
  const optsOpen = moreSet(tab).has('opts');
  const optsBtn = h(
    'button',
    {
      class: 'morebtn' + (optsOpen ? ' on' : ''),
      onclick: () => {
        const m = moreSet(tab);
        optsOpen ? m.delete('opts') : m.add('opts');
        redraw();
      },
    },
    optsOpen ? 'Hide options' : 'Options',
  );
  const opts = optsOpen
    ? h(
        'div',
        { class: 'runopts' },
        h('label', { class: 'chk' }, base, ' Send the original first, as a baseline'),
        h('label', null, 'Stop after ', max, ' requests'),
        h('label', null, 'Wait ', delay, ' ms between requests'),
      )
    : null;

  const startBtn = h('button', { class: 'btn primary runstart', disabled: n === 0, text: n === 0 ? 'Pick a value to run' : 'Start run', onclick: () => startRun(tab, main) });

  const manualHint = n === 0 ? h('div', { class: 'runmanual' }, 'Varying something the highlights missed? Select it in the request on the left and press ', h('b', { text: '+ Mark position' }), '.') : null;

  clear(
    col,
    h('div', { class: 'lbl', text: 'Run' }),
    h('div', { class: 'runconf' }, lead, runCanvas(tab, main), modeSeg, cards, h('div', { class: 'runactions' }, startBtn, optsBtn), opts, manualHint),
  );
}

async function startRun(tab, main) {
  const positions = countPositions(tab);
  if ((tab.url || '').split(MARK).length % 2 === 0 || (tab.raw || '').split(MARK).length % 2 === 0) {
    return toast('A position is not closed: every • needs a matching •.', 'err');
  }
  if (positions === 0) return toast('Mark at least one position first.', 'err');
  const cfg = runCfg(tab);
  const { headers, body, bad } = parseRaw(tab.raw);
  if (bad.length) return toast('Not a header line: ' + bad[0], 'err');
  // Rebuild the raw with the parsed headers + body so marks in both survive.
  const raw = headers.map(([k, v]) => `${k}: ${v}`).join('\n') + '\n\n' + body;
  const req = {
    method: tab.method,
    url: tab.url,
    raw,
    lists: cfg.lists.map(cleanList),
    mode: cfg.mode,
    include_base: !!cfg.base,
    max_requests: cfg.max ? Number(cfg.max) : null,
    delay_ms: cfg.delay === '' ? null : Number(cfg.delay),
  };
  tab.runState = { busy: true, report: null, error: null, sort: tab.runState?.sort, sel: null };
  drawRunResults(tab, main);
  try {
    const report = await api('/api/run', { method: 'POST', body: req });
    tab.runState = { busy: false, report, error: null, sort: tab.runState?.sort, sel: null };
  } catch (e) {
    tab.runState = { busy: false, report: null, error: e.message, sort: null, sel: null };
  }
  drawRunResults(tab, main);
}

function cleanList(c) {
  if (c.kind === 'range') return { kind: 'range', from: Number(c.from ?? 1), to: Number(c.to ?? 100), step: Number(c.step || 1) };
  if (c.kind === 'values') return { kind: 'values', values: (c.values || []).filter((v, i, a) => !(v === '' && i === a.length - 1)) };
  return { kind: 'builtin', id: c.id };
}

function drawRunResults(tab, main) {
  const slot = $('#runresults');
  if (!slot) return;
  const st = tab.runState;
  if (!st) return clear(slot);
  if (st.busy) return clear(slot, h('div', { class: 'runbusy', text: 'Running… sending requests through scope.' }));
  if (st.error) return clear(slot, h('div', { class: 'rerr' }, h('b', { text: 'Run failed. ' }), st.error));
  const rep = st.report;
  if (!rep) return clear(slot);

  const baseLen = (rep.rows.find((r) => r.baseline) || {}).length;
  let rows = rep.rows.slice();
  const sort = st.sort;
  if (sort) {
    const key = sort.key;
    rows.sort((a, b) => {
      const av = key === 'values' ? a.values.join() : (a[key] ?? 0);
      const bv = key === 'values' ? b.values.join() : (b[key] ?? 0);
      return (av > bv ? 1 : av < bv ? -1 : 0) * (sort.dir === 'desc' ? -1 : 1);
    });
  }
  const th = (key, label) =>
    h('button', {
      class: 'sortbtn' + (sort && sort.key === key ? ' on' : ''),
      text: label + (sort && sort.key === key ? (sort.dir === 'desc' ? ' ↓' : ' ↑') : ''),
      onclick: () => {
        st.sort = sort && sort.key === key ? { key, dir: sort.dir === 'desc' ? 'asc' : 'desc' } : { key, dir: 'desc' };
        drawRunResults(tab, main);
      },
    });

  const table = h(
    'div',
    { class: 'runtable' },
    h('div', { class: 'rthead' }, th('n', '#'), th('status', 'Status'), th('length', 'Length'), th('duration_ms', 'Time'), th('values', 'Payload')),
    ...rows.map((r) =>
      h(
        'div',
        {
          class: 'rtrow' + (st.sel === r.exchange_id ? ' sel' : '') + (baseLen != null && !r.baseline && r.length !== baseLen ? ' diff' : ''),
          onclick: () => {
            st.sel = r.exchange_id;
            drawRunResults(tab, main);
          },
        },
        h('span', { class: 'c-n', text: '#' + r.n + (r.baseline ? ' ·' : '') }),
        h('span', { class: statusClass(r.status), text: r.status == null ? 'ERR' : r.status }),
        h('span', { class: 'c-len', text: fmtSize(r.length) }),
        h('span', { class: 'c-ms', text: r.duration_ms + ' ms' }),
        h('span', { class: 'c-val', text: r.values.join(' | ') || '(base)', title: r.values.join(' | ') }),
        h('button', {
          class: 'btn sm',
          text: 'Finding',
          onclick: (ev) => {
            ev.stopPropagation();
            newFinding([r.exchange_id], `${tab.name}: ${r.values.join(' | ')}`);
          },
        }),
      ),
    ),
  );

  const summary = h(
    'div',
    { class: 'runsum' },
    `${rep.requests_sent} request${rep.requests_sent === 1 ? '' : 's'} sent across ${rep.positions} position${rep.positions === 1 ? '' : 's'}.` +
      (rep.truncated ? ` Stopped at the budget (${rep.planned} planned).` : '') +
      ' Tick a row to inspect its response. Every request is in Traffic too.',
  );

  clear(slot, h('div', { class: 'runresultswrap' }, summary, table, h('div', { class: 'runsel' })));
  for (const n of rep.notes || []) slot.firstChild.append(h('div', { class: 'runnote', text: 'Note: ' + n }));
  if (st.sel != null) drawRunSelection(tab);
}

async function drawRunSelection(tab) {
  const slot = $('.runsel');
  if (!slot) return;
  clear(slot, h('pre', { class: 'raw muted', text: 'Loading…' }));
  let ex;
  try {
    ex = await getExchange(tab.runState.sel);
  } catch (e) {
    return clear(slot, h('div', { class: 'rerr', text: e.message }));
  }
  if (!slot.isConnected) return;
  clear(
    slot,
    h('div', { class: 'lbl' }, 'Response', h('span', { class: 'r' }, h('span', { class: statusClass(ex.status), text: ex.status == null ? 'no response' : ex.status }), ` · ${ex.duration_ms} ms · ${fmtSize(b64len(ex.resp_body))} · #${ex.id}`, ex.client_cert ? ' ' : null, clientCertTag(ex))),
    rawPre(responseText(ex, true)),
  );
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
        ex.client_cert ? ' ' : null,
        clientCertTag(ex),
      ),
    ),
    h('div', { class: 'authslot' }),
    h('div', { class: 'spotslot' }),
    rawPre(responseText(ex, pretty)),
  );
  checkLoggedOut(tab, ex, col.querySelector('.authslot'));
  const list = await loadInsights(ex);
  if (list && col.isConnected) drawInsights(col, list.filter((i) => i.side === 'response'));
}

/* ----- Session expired: offer the newest captured login ----- */

const AUTH_HEADER = /^(authorization|cookie|x-[\w-]*(token|csrf|xsrf|session|auth)[\w-]*|[\w-]*(csrf|xsrf)[\w-]*)$/i;

/** True when a response reads as "you are not logged in". */
function looksLoggedOut(ex) {
  if ([401, 419, 440].includes(ex.status)) return true;
  if (ex.status >= 300 && ex.status < 400) return /log-?in|sign-?in|logon|\/auth|sso|session/i.test(header(ex.resp_headers, 'location') || '');
  return false;
}

function authHeadersOf(headers) {
  return headers.filter(([k, v]) => AUTH_HEADER.test(k) && v);
}

/**
 * When a Bench send comes back logged out but the same request worked
 * before, the saved login has probably expired. Plonix looks for a newer
 * login in captured traffic and offers to swap it in, here and in the
 * other Bench tabs for the same host. Nothing is sent until Send.
 */
async function checkLoggedOut(tab, ex, slot) {
  if (!slot || !looksLoggedOut(ex)) return;
  const host = hostOf(tab.url);
  let worked = tab.history.some((e) => e.id !== ex.id && e.status >= 200 && e.status < 300);
  if (!worked && tab.from) {
    try {
      const orig = await getExchange(tab.from);
      worked = orig.status >= 200 && orig.status < 300;
    } catch (_) {}
  }
  if (!worked || !host) return;
  const mine = new Map(authHeadersOf(parseRaw(tab.raw).headers).map(([k, v]) => [k.toLowerCase(), v]));
  let fresh = null;
  try {
    const page = await api('/api/traffic?limit=40&q=' + encodeURIComponent(`host:${host} status:2xx -source:replay`));
    for (const item of page.items.filter((i) => i.id !== tab.from).slice(0, 15)) {
      const cand = await getExchange(item.id);
      const auth = authHeadersOf(cand.req_headers);
      if (auth.some(([k, v]) => mine.has(k.toLowerCase()) && mine.get(k.toLowerCase()) !== v)) {
        fresh = { ex: cand, auth };
        break;
      }
    }
  } catch (_) {}
  if (!slot.isConnected) return;
  // Other tabs still on the same old login; a tab whose token was changed on purpose is left alone.
  const same = (t) => {
    const theirs = new Map(authHeadersOf(parseRaw(t.raw).headers).map(([k, v]) => [k.toLowerCase(), v]));
    const changed = fresh.auth.map(([k]) => k.toLowerCase()).filter((k) => mine.has(k) && mine.get(k) !== fresh.auth.find(([n]) => n.toLowerCase() === k)[1]);
    return changed.length > 0 && changed.every((k) => theirs.get(k) === mine.get(k));
  };
  const others = fresh ? R.tabs.filter((t) => t !== tab && hostOf(t.url) === host && same(t)) : [];
  const apply = (all) => {
    for (const t of all ? [tab, ...others] : [tab]) t.raw = swapAuth(t.raw, fresh.auth);
    saveBench();
    toast(all && others.length ? `Newest login used in ${others.length + 1} Bench tabs` : 'Newest login used. Press Send to try again.', 'ok');
    renderBench($('#main'));
  };
  clear(
    slot,
    h(
      'div',
      { class: 'authhint' },
      h('b', { text: 'Your login looks expired. ' }),
      fresh
        ? [
            `This request worked before. A newer login for ${host} was captured at ${fmtTime(fresh.ex.ts)} (#${fresh.ex.id}).`,
            h('span', { class: 'acts' }, h('button', { class: 'btn sm primary', text: 'Use the newest login', onclick: () => apply(false) }), others.length ? h('button', { class: 'btn sm', text: `Use it in all ${others.length + 1} tabs`, onclick: () => apply(true) }) : null),
          ]
        : [
            'This request worked before. Log in again in the capture browser, then check again to use the new login here.',
            h('span', { class: 'acts' }, h('button', { class: 'btn sm', text: 'Check again', onclick: () => checkLoggedOut(tab, ex, slot) })),
          ],
    ),
  );
}

/** Replaces the login headers in a Bench request with fresh ones; values elsewhere stay as they are. */
function swapAuth(raw, auth) {
  const text = raw.replace(/\r\n/g, '\n');
  const cut = text.indexOf('\n\n');
  const head = cut < 0 ? text : text.slice(0, cut);
  const rest = cut < 0 ? '' : text.slice(cut);
  const fresh = new Map(auth.map(([k, v]) => [k.toLowerCase(), v]));
  const lines = head.split('\n').map((line) => {
    const c = line.indexOf(':');
    const name = c > 0 ? line.slice(0, c).trim() : '';
    if (!name || !fresh.has(name.toLowerCase())) return line;
    const v = fresh.get(name.toLowerCase());
    fresh.delete(name.toLowerCase());
    return `${name}: ${v}`;
  });
  return lines.join('\n') + rest;
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
  const rules = (S.scope.rules || [])
    .filter((r) => !(r.note || '').startsWith('group:'))
    .slice()
    .sort((x, y) => x.decision.localeCompare(y.decision) || x.pattern.localeCompare(y.pattern));
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
    excludedSection(),
  );
}

/* ---- exclusions: grouped out-of-scope domains ---- */

/** Which exclusion groups are expanded on the Scope screen. */
const X = { open: {} };

const groupStateLabel = { on: 'On', partial: 'Some', off: 'Off' };

async function reloadExclusions(ex) {
  if (ex) S.exclusions = ex;
  // Group changes add or drop reject rules, so refresh the rest of the screen too.
  await loadScope();
  renderScopeBody();
}

async function toggleGroup(id, on) {
  try {
    const ex = await api('/api/scope/exclusions/group', { method: 'POST', body: { id, on } });
    toast(on ? 'Excluded the group' : 'Removed the group from exclusions', on ? 'ok' : '');
    await reloadExclusions(ex);
  } catch (e) {
    toast(e.message, 'err');
  }
}

async function toggleExcludedDomain(id, host, on) {
  try {
    const ex = await api('/api/scope/exclusions/domain', { method: 'POST', body: { id, host, on } });
    await reloadExclusions(ex);
  } catch (e) {
    toast(e.message, 'err');
  }
}

function groupCard(g) {
  const open = !!X.open[g.id];
  const count = g.domains.filter((d) => d.excluded).length;
  const header = h(
    'div',
    { class: 'top' },
    h(
      'button',
      { class: 'disclose', title: open ? 'Hide domains' : 'Show domains', onclick: () => ((X.open[g.id] = !open), renderScopeBody()) },
      h('span', { class: 'caret', text: open ? '▾' : '▸' }),
      h('span', { class: 'dom', text: g.name }),
    ),
    h('span', { class: 'meta', text: `${count}/${g.domains.length} excluded${g.builtin ? '' : ' · custom'}` }),
    h(
      'span',
      { class: 'acts' },
      h('span', { class: 'tag ' + (g.state === 'off' ? 'out' : g.state === 'on' ? 'rej' : ''), text: groupStateLabel[g.state] }),
      h('button', { class: 'btn sm', text: g.state === 'on' ? 'Turn off' : 'Exclude all', onclick: () => toggleGroup(g.id, g.state !== 'on') }),
      g.builtin ? null : h('button', { class: 'btn sm danger', text: 'Delete', title: 'Delete this custom group', onclick: () => removeCustomGroup(g) }),
    ),
  );
  const desc = g.description ? h('div', { class: 'gdesc muted', text: g.description }) : null;
  const list = open
    ? h(
        'div',
        { class: 'domlist' },
        g.domains.map((d) =>
          h(
            'label',
            { class: 'domrow' },
            h('input', { type: 'checkbox', checked: d.excluded, onchange: (e) => toggleExcludedDomain(g.id, d.host, e.target.checked) }),
            h('span', { class: 'mono', text: d.host }),
          ),
        ),
      )
    : null;
  return h('div', { class: 'card group' }, header, desc, list);
}

function newGroupForm() {
  const name = h('input', { type: 'text', placeholder: 'Group name, e.g. Vendor widgets', spellcheck: 'false' });
  const domains = h('textarea', { placeholder: 'One domain per line, or comma-separated', rows: '3', spellcheck: 'false' });
  const create = async () => {
    const list = domains.value
      .split(/[\s,]+/)
      .map((d) => d.trim())
      .filter(Boolean);
    if (!name.value.trim()) return name.focus();
    if (!list.length) return domains.focus();
    try {
      const res = await api('/api/scope/exclusions/custom', { method: 'POST', body: { id: '', name: name.value.trim(), domains: list } });
      name.value = '';
      domains.value = '';
      toast('Created the group. Turn it on to exclude its domains.', 'ok');
      await reloadExclusions(res.exclusions);
    } catch (e) {
      toast(e.message, 'err');
    }
  };
  return h(
    'div',
    { class: 'card newgroup' },
    h('div', { class: 'caph', text: 'New group' }),
    name,
    domains,
    h('div', { class: 'row end' }, h('button', { class: 'btn primary', text: 'Create group', onclick: create })),
  );
}

function removeCustomGroup(g) {
  modal(
    'Delete group',
    h('p', null, 'Delete the custom group ', h('b', { text: g.name }), ' and remove any exclusions it added? This cannot be undone.'),
    [
      h('button', { class: 'btn', text: 'Cancel', onclick: closeModal }),
      h('button', {
        class: 'btn danger',
        text: 'Delete group',
        onclick: async () => {
          closeModal();
          try {
            const ex = await api('/api/scope/exclusions/custom', { method: 'DELETE', body: { id: g.id } });
            toast('Deleted the group');
            await reloadExclusions(ex);
          } catch (e) {
            toast(e.message, 'err');
          }
        },
      }),
    ],
  );
}

function excludedSection() {
  const groups = (S.exclusions && S.exclusions.groups) || [];
  const total = groups.reduce((n, g) => n + g.domains.filter((d) => d.excluded).length, 0);
  return h(
    'div',
    { class: 'excluded' },
    h(
      'div',
      { class: 'sechead' },
      h('h3', { text: `Excluded domains (${total})` }),
      h('span', { class: 'hint', text: 'Hosts you never want captured as targets. They are never suggested and never sent to.' }),
    ),
    groups.length ? groups.map(groupCard) : h('div', { class: 'card' }, h('div', { class: 'empty', text: 'No exclusion groups.' })),
    newGroupForm(),
  );
}

/** First run: offer to exclude common third-party domains, once per project. */
function maybeAskExclusions() {
  if (!S.exclusions || S.exclusions.asked || S.askedExclusionsThisSession) return;
  const groups = S.exclusions.groups || [];
  if (!groups.length) return;
  S.askedExclusionsThisSession = true;
  const picks = {};
  groups.forEach((g) => (picks[g.id] = true));
  const rows = groups.map((g) =>
    h(
      'label',
      { class: 'domrow' },
      h('input', { type: 'checkbox', checked: true, onchange: (e) => (picks[g.id] = e.target.checked) }),
      h('span', null, h('b', { text: g.name }), ' ', h('span', { class: 'muted', text: `(${g.domains.length} domains)` })),
    ),
  );
  const finish = async (enable) => {
    closeModal();
    try {
      if (enable) {
        for (const g of groups) if (picks[g.id]) await api('/api/scope/exclusions/group', { method: 'POST', body: { id: g.id, on: true } });
      }
      await api('/api/scope/exclusions/asked', { method: 'POST' });
      await loadScope();
      if (S.view === 'scope') renderScopeBody();
      if (enable) toast('Common domains excluded. Edit them anytime on the Scope screen.', 'ok');
    } catch (e) {
      toast(e.message, 'err');
    }
  };
  modal(
    'Exclude common domains?',
    h(
      'div',
      null,
      h('p', { class: 'muted', text: 'Plonix can keep common third parties — analytics, ads, payments, CDNs and the like — out of your target scope, so they are never suggested or sent to. Pick the groups to exclude; you can change these anytime on the Scope screen.' }),
      h('div', { class: 'domlist' }, rows),
    ),
    [
      h('button', { class: 'btn', text: 'Not now', onclick: () => finish(false) }),
      h('button', { class: 'btn primary', text: 'Exclude selected', onclick: () => finish(true) }),
    ],
  );
}

/* ======================================================================
   Map: hosts, endpoints, technologies
   ====================================================================== */

const M = { hosts: [], tech: {}, sel: null, dirty: true, scroll: null };

function saveMapScroll() {
  const list = $('#hostlist');
  const detail = $('#hostdetail');
  if (list && detail) M.scroll = [list.scrollTop, detail.scrollTop];
}

function restoreMapScroll() {
  const list = $('#hostlist');
  const detail = $('#hostdetail');
  if (!M.scroll || !list || !detail) return;
  [list.scrollTop, detail.scrollTop] = M.scroll;
  M.scroll = null;
}

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
      h('div', { class: 'traffic' }, h('div', { class: 'mapwrap' }, h('div', { class: 'hostlist', id: 'hostlist' }), h('div', { class: 'hostdetail', id: 'hostdetail' })), h('div', { id: 'inspslot' })),
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
  await drawHostDetail();
  restoreMapScroll();
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
  let spec;
  try {
    [eps, tech, spec] = await Promise.all([
      api('/api/hosts/' + encodeURIComponent(host) + '/endpoints'),
      api('/api/tech/' + encodeURIComponent(host)),
      api('/api/hosts/' + encodeURIComponent(host) + '/spec').catch(() => null),
    ]);
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
      toolOn('access-check') && d === 'accepted'
        ? h('button', { class: 'btn sm', text: 'Check access', title: 'Replay this host\'s endpoints as each saved user, and once signed out', onclick: () => startAccessCheck({ host, prefix: '/', sourceLabel: host }) })
        : null,
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
    spec ? specSection(spec) : null,
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
            { class: 'click' + (T.sel === e.sample_id ? ' sel' : ''), 'data-ex': e.sample_id, title: 'Open a sample request', onclick: () => showExchange(e.sample_id) },
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

/** Endpoints an API description lists that captured traffic has not visited yet, each one click from the Bench. */
function specSection(spec) {
  const todo = spec.endpoints.filter((e) => !e.visited);
  const open = M.specOpen === spec.host;
  M.specOpen = null;
  const body = h(
    'div',
    { hidden: !open },
    todo.length
      ? h(
          'table',
          { class: 'grid' },
          h('thead', null, h('tr', null, h('th', { text: 'Method' }), h('th', { text: 'Path' }), h('th', { text: 'What it does' }), h('th', { text: 'Parameters' }), h('th'))),
          h(
            'tbody',
            null,
            todo.map((e) =>
              h(
                'tr',
                null,
                h('td', null, h('span', { class: 'meth m-' + e.method, text: e.method })),
                h('td', { class: 'mono', text: e.path }),
                h('td', { class: 'muted', text: e.summary }),
                h('td', null, e.params.map((p) => h('span', { class: 'param', text: p }))),
                h('td', { class: 'num' }, h('button', { class: 'btn sm', text: 'Open on Bench', title: 'Start a request for this endpoint on the Bench. Nothing is sent until you press Send.', onclick: () => specToBench(spec, e) })),
              ),
            ),
          ),
        )
      : h('div', { class: 'muted', style: { padding: '4px 12px 8px' }, text: 'Every endpoint it lists has been visited.' }),
  );
  const toggle = h('button', {
    class: 'link',
    text: open ? 'Hide' : 'Show',
    onclick: () => {
      body.hidden = !body.hidden;
      toggle.textContent = body.hidden ? 'Show' : 'Hide';
    },
  });
  return h(
    'div',
    { class: 'specsec' },
    h(
      'div',
      { class: 'lbl', style: { padding: '12px 12px 6px' } },
      `Not visited yet (${todo.length} of ${spec.endpoints.length})`,
      h(
        'span',
        { class: 'r' },
        'from the API description ',
        h('button', { class: 'link', text: '#' + spec.source_id, title: (spec.title || 'API description') + (spec.version ? ' ' + spec.version : ''), onclick: () => showExchange(spec.source_id) }),
        ' · ',
        toggle,
      ),
    ),
    body,
  );
}

function specToBench(spec, e) {
  const url = `${spec.scheme || 'https'}://${spec.host}${e.path}`;
  const query = e.params.filter((p) => p.startsWith('query:')).map((p) => p.slice(6) + '=');
  const json = /^(POST|PUT|PATCH)$/.test(e.method);
  const head = ['Accept: */*', 'User-Agent: Plonix', json ? 'Content-Type: application/json' : null].filter(Boolean).join('\n');
  R.tabs.push({ name: `${e.method} ${e.path}`.slice(0, 60), method: e.method, url: url + (query.length ? '?' + query.join('&') : ''), raw: head + '\n\n' + (json ? '{}' : ''), bodyB64: null, history: [], cur: null, picks: [] });
  R.active = R.tabs.length - 1;
  saveBench();
  leaveTo('bench');
}

/* ======================================================================
   Saved users (the cookie jar) and the Access check
   Two Market tools. Saved users keep each person's cookies and tokens so the
   Bench can send a request as any of them; the Access check replays chosen
   requests as every user, and once signed out, and lines the responses up.
   ====================================================================== */

/** Loads the saved users for this project, cached on S. */
async function loadUsers(force) {
  if (S.users && !force) return S.users;
  try {
    const r = await api('/api/users');
    S.users = r.users || [];
  } catch (_) {
    S.users = [];
  }
  return S.users;
}

/** Parses a "Name: value" per line block into header pairs. */
function parseHeaderLines(text) {
  const headers = [];
  for (const line of (text || '').split('\n')) {
    if (!line.trim()) continue;
    const c = line.indexOf(':');
    if (c > 0) headers.push([line.slice(0, c).trim(), line.slice(c + 1).trim()]);
  }
  return headers;
}

const headerLines = (headers) => (headers || []).map(([k, v]) => `${k}: ${v}`).join('\n');

/** The manage-users sheet: add, edit and remove the saved users. */
async function manageUsers(afterSave) {
  const users = (await loadUsers(true)).map((u) => ({ ...u, headers: (u.headers || []).map((h) => [...h]) }));
  const list = h('div', { class: 'userlist' });
  const draw = () => {
    clear(
      list,
      users.length ? null : h('div', { class: 'muted', text: 'No users yet. Add one and paste its Cookie or Authorization header below.' }),
      users.map((u, i) =>
        h(
          'div',
          { class: 'usercard' },
          h(
            'div',
            { class: 'urow' },
            h('input', { class: 'uname', placeholder: 'Name, e.g. Alice (admin)', value: u.name || '', oninput: (e) => (u.name = e.target.value) }),
            h('button', { class: 'btn sm danger', text: 'Remove', onclick: () => (users.splice(i, 1), draw()) }),
          ),
          h('input', { class: 'unote', placeholder: 'Optional note', value: u.note || '', oninput: (e) => (u.note = e.target.value) }),
          h('label', { class: 'ulbl', text: 'Headers sent as this user (one per line)' }),
          h('textarea', {
            class: 'uhead',
            spellcheck: 'false',
            rows: '3',
            placeholder: 'Cookie: session=…\nAuthorization: Bearer …',
            value: headerLines(u.headers),
            oninput: (e) => (u._raw = e.target.value),
          }),
        ),
      ),
      h('button', { class: 'btn sm', text: '+ Add user', onclick: () => (users.push({ id: '', name: '', note: '', headers: [] }), draw()) }),
    );
  };
  draw();
  const save = async () => {
    const payload = users
      .map((u) => ({ id: u.id || '', name: (u.name || '').trim(), note: (u.note || '').trim(), headers: u._raw != null ? parseHeaderLines(u._raw) : u.headers }))
      .filter((u) => u.name);
    try {
      const r = await api('/api/users', { method: 'PUT', body: { users: payload } });
      S.users = r.users || [];
      closeModal();
      toast('Saved users updated', 'ok');
      if (afterSave) afterSave();
    } catch (e) {
      toast(e.message, 'err');
    }
  };
  modal('Saved users', h('div', { class: 'usersheet' }, h('p', { class: 'hint', text: 'Each user is a set of headers — usually a Cookie or a token — applied to a request before it is sent. Values stay in this project.' }), list), [
    h('button', { class: 'btn', text: 'Cancel', onclick: closeModal }),
    h('button', { class: 'btn primary', text: 'Save', onclick: save }),
  ]);
}

/** The Bench control that picks which saved user a request is sent as. */
function userSwitcher(tab, main) {
  const sel = h('select', {
    class: 'assel',
    title: 'Send this request as a saved user',
    onchange: () => {
      if (sel.value === '__manage') {
        sel.value = tab.asUser || '';
        return manageUsers(() => renderBench(main));
      }
      tab.asUser = sel.value || null;
      saveBench();
      renderBench(main);
    },
  });
  const opts = [h('option', { value: '', text: 'As written' })];
  for (const u of S.users || []) opts.push(h('option', { value: u.id, text: 'As ' + u.name }));
  opts.push(h('option', { value: '__manage', text: 'Manage users…' }));
  append(sel, opts);
  sel.value = (S.users || []).some((u) => u.id === tab.asUser) ? tab.asUser : '';
  if (sel.value !== (tab.asUser || '')) {
    tab.asUser = sel.value || null; // the saved user is gone; fall back
  }
  return h('span', { class: 'asbox' }, h('span', { class: 'aslbl', text: '⚿' }), sel);
}

/* ---- the Access check screen ---- */

const AC = { host: null, prefix: '/', targets: [], sourceLabel: '', anon: true, picks: null, running: false, report: null, err: null };

/** Opens the Access check on a selection from Traffic or the Map. */
function startAccessCheck({ targets = [], host = null, prefix = '/', sourceLabel = '' } = {}) {
  AC.targets = targets;
  AC.host = host;
  AC.prefix = prefix || '/';
  AC.sourceLabel = sourceLabel;
  AC.report = null;
  AC.err = null;
  AC.picks = null; // default: every saved user
  leaveTo('access');
}

async function renderAccess(main) {
  const view = h(
    'div',
    { class: 'view' },
    h(
      'div',
      { class: 'toolbar' },
      backButton(),
      h('h2', { text: 'Access check' }),
      h('span', { class: 'hint', text: 'Replays the chosen requests as each saved user, and once signed out, so you can see where access differs.' }),
    ),
  );
  clear(main, view);
  await loadUsers();
  if (AC.picks === null) AC.picks = new Set((S.users || []).map((u) => u.id));
  const body = h('div', { class: 'pane acpane' });
  view.append(body);
  drawAccess(body, main);
}

function drawAccess(body, main) {
  const hosts = ((S.facets && S.facets.hosts) || []).map((x) => x.value);
  const sourceCard = h('div', { class: 'accard' });
  if (AC.targets.length) {
    clear(
      sourceCard,
      h('div', { class: 'aclbl', text: 'Requests to check' }),
      h('div', { class: 'acsource' }, h('b', { text: AC.targets.length + (AC.targets.length === 1 ? ' request' : ' requests') }), AC.sourceLabel ? h('span', { class: 'muted', text: ' · ' + AC.sourceLabel }) : null, h('button', { class: 'link', text: 'choose a host instead', onclick: () => ((AC.targets = []), (AC.sourceLabel = ''), drawAccess(body, main)) })),
    );
  } else {
    const hostSel = h('select', { class: 'achost' }, h('option', { value: '', text: hosts.length ? 'Pick a host…' : 'No hosts captured yet' }), hosts.map((hn) => h('option', { value: hn, text: hn, selected: hn === AC.host })));
    hostSel.onchange = () => (AC.host = hostSel.value || null);
    const prefix = h('input', { class: 'acprefix mono', value: AC.prefix, spellcheck: 'false', title: 'Only endpoints whose path starts with this are checked', oninput: () => (AC.prefix = prefix.value || '/') });
    clear(
      sourceCard,
      h('div', { class: 'aclbl', text: 'A branch of an application' }),
      h('div', { class: 'acsource' }, hostSel, h('span', { class: 'muted', text: 'path starts with' }), prefix),
      h('div', { class: 'mnote', text: 'One captured request is checked for each endpoint on that branch. Or pick rows in Traffic, or a host in the Map, and choose "Check access".' }),
    );
  }

  // Identities to replay as.
  const idCard = h('div', { class: 'accard' });
  const userRows = (S.users || []).map((u) =>
    h('label', { class: 'acid' }, h('input', { type: 'checkbox', checked: AC.picks.has(u.id), onchange: (e) => (e.target.checked ? AC.picks.add(u.id) : AC.picks.delete(u.id)) }), h('span', { class: 'acname', text: u.name }), u.headers && u.headers.length ? h('span', { class: 'achdr', text: u.headers.map((h) => h[0]).join(', ') }) : null),
  );
  clear(
    idCard,
    h('div', { class: 'aclbl' }, 'Replay as', h('button', { class: 'link', style: { marginLeft: 'auto' }, text: (S.users || []).length ? 'Manage users' : 'Add users', onclick: () => manageUsers(() => renderAccess($('#main'))) })),
    (S.users || []).length ? h('div', { class: 'acids' }, userRows) : h('div', { class: 'muted', text: 'No saved users yet. Add some, or run the signed-out check on its own.' }),
    h('label', { class: 'acid anon' }, h('input', { type: 'checkbox', checked: AC.anon, onchange: (e) => (AC.anon = e.target.checked) }), h('span', { class: 'acname', text: 'Signed out' }), h('span', { class: 'achdr', text: 'auth headers removed' })),
  );

  const run = h('button', { class: 'btn primary', text: AC.running ? 'Checking…' : 'Check access', disabled: AC.running, onclick: () => runAccessCheck(body, main) });
  const results = h('div', { class: 'acresults', id: 'acresults' });
  clear(body, sourceCard, idCard, h('div', { class: 'acrun' }, run, AC.err ? h('span', { class: 'rerr', text: AC.err }) : null), results);
  drawAccessReport(results);
}

async function runAccessCheck(body, main) {
  const picks = [...AC.picks];
  if (!AC.anon && !picks.length) {
    AC.err = 'Pick at least one saved user, or keep the signed-out check on.';
    return drawAccess(body, main);
  }
  if (!AC.targets.length && !AC.host) {
    AC.err = 'Pick a host, or choose requests in Traffic or the Map.';
    return drawAccess(body, main);
  }
  AC.err = null;
  AC.running = true;
  AC.report = null;
  drawAccess(body, main);
  const req = { include_anon: AC.anon, user_ids: picks };
  if (AC.targets.length) req.targets = AC.targets;
  else {
    req.host = AC.host;
    req.prefix = AC.prefix;
  }
  try {
    AC.report = await api('/api/access-check', { method: 'POST', body: req });
  } catch (e) {
    AC.err = e.message;
  }
  AC.running = false;
  drawAccess(body, main);
}

const okStatus = (s) => s != null && s >= 200 && s < 300;

function drawAccessReport(box) {
  const r = AC.report;
  if (!r) return clear(box);
  if (!r.rows.length) return clear(box, h('div', { class: 'empty', text: 'Nothing was checked. Pick some requests and run again.' }));
  const flagged = r.rows.filter((row) => row.notes && row.notes.length).length;
  const head = h(
    'div',
    { class: 'aclead' },
    `Checked ${r.rows.length} request${r.rows.length === 1 ? '' : 's'} as ${r.identities.length} identit${r.identities.length === 1 ? 'y' : 'ies'}` + (r.truncated ? ` (stopped at the ${r.sent}-request limit)` : '') + '.',
    flagged ? h('b', { class: 'acflag', text: ` ${flagged} to look at.` }) : h('span', { class: 'muted', text: ' Nothing stood out.' }),
  );
  const headCells = [h('th', { class: 'actarget', text: 'Request' })];
  for (const id of r.identities) headCells.push(h('th', { class: 'acidcol' + (id.anon ? ' anon' : ''), text: id.label }));
  const rows = [];
  for (const row of r.rows) {
    const byId = {};
    for (const c of row.cells) byId[c.identity] = c;
    const tds = [h('td', { class: 'actarget' }, h('span', { class: 'meth m-' + row.method, text: row.method }), h('span', { class: 'mono acpath', text: row.path, title: row.host + row.path }))];
    for (const id of r.identities) {
      const c = byId[id.id];
      tds.push(
        h(
          'td',
          { class: 'accell' },
          c
            ? h(
                'button',
                { class: 'acbtn' + (c.error ? ' err' : okStatus(c.status) ? ' ok' : ''), title: (c.error || 'open this response') + ' · ' + fmtSize(c.len), onclick: () => c.exchange_id && showExchange(c.exchange_id) },
                h('span', { class: statusClass(c.status), text: c.status == null ? '—' : c.status }),
                h('span', { class: 'aclen', text: c.error ? 'error' : fmtSize(c.len) }),
              )
            : h('span', { class: 'muted', text: '—' }),
        ),
      );
    }
    rows.push(h('tr', { class: row.notes && row.notes.length ? 'acnote' : '' }, tds));
    if (row.notes && row.notes.length) {
      rows.push(
        h(
          'tr',
          { class: 'acnoterow' },
          h('td', { colspan: String(r.identities.length + 1) }, h('div', { class: 'acnotes' }, row.notes.map((n) => h('span', { class: 'acnotechip' }, h('span', { class: 'acnoteico', text: '⚑' }), h('span', { text: n }))))),
        ),
      );
    }
  }
  const cols = h('colgroup', null, [h('col', { class: 'acreqcol' }), ...r.identities.map(() => h('col', { class: 'acidc' }))]);
  clear(box, head, h('table', { class: 'actable' }, cols, h('thead', null, h('tr', null, headCells)), h('tbody', null, rows)));
}

/* ======================================================================
   Findings
   ====================================================================== */

function renderFindings(main) {
  const exportBtn = h('button', { class: 'btn sm', text: 'Export ▾', title: 'Save the findings as a report, with their evidence requests', onclick: () => exportMenu(exportBtn) });
  clear(
    main,
    h(
      'div',
      { class: 'view' },
      h('div', { class: 'toolbar' }, h('h2', { text: 'Findings' }), h('span', { class: 'hint', text: 'Reproducible issues, each tied to the requests that prove it.' }), exportBtn, h('button', { class: 'btn primary sm', text: 'New finding', onclick: () => newFinding([], '') })),
      h('div', { class: 'traffic' }, h('div', { class: 'pane' }, h('div', { class: 'stack', id: 'findbody' })), h('div', { id: 'inspslot' })),
    ),
  );
  loadFindings();
}

const SEV_ORDER = { critical: 0, high: 1, medium: 2, low: 3, info: 4 };
const SEVERITIES = ['info', 'low', 'medium', 'high', 'critical'];
const FINDING_STATUSES = { open: 'Open', confirmed: 'Confirmed', false_positive: 'False positive', fixed: 'Fixed' };

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
  // Closed findings (false positives, fixed) go below the ones still to deal with.
  const closed = (f) => (f.status === 'false_positive' || f.status === 'fixed' ? 1 : 0);
  list.sort((a, b) => closed(a) - closed(b) || (SEV_ORDER[a.severity] ?? 9) - (SEV_ORDER[b.severity] ?? 9) || b.created_at - a.created_at);
  clear(box, list.map(findingCard));
}

function findingCard(f) {
  const status = h(
    'select',
    { class: 'fstatus', 'aria-label': 'Status', title: 'Where this finding stands', onchange: () => setFindingStatus(f, status) },
    Object.entries(FINDING_STATUSES).map(([v, label]) => h('option', { value: v, text: label, selected: v === f.status })),
  );
  const edited = f.updated_at > f.created_at ? ` · edited ${fmtDate(f.updated_at)}` : '';
  return h(
    'div',
    { class: 'card finding' + (f.status === 'false_positive' || f.status === 'fixed' ? ' closed' : '') },
    h(
      'div',
      { class: 'fh' },
      h('span', { class: 'sev ' + f.severity, text: f.severity }),
      h('span', { class: 'ft', text: f.title }),
      h('span', { class: 'fmeta', text: `#${f.id} · by ${f.created_by} · ${fmtDate(f.created_at)}${edited}` }),
      status,
      askButton({ kind: 'finding', id: f.id }),
      h('button', { class: 'btn sm', text: 'Edit', onclick: () => findingForm(f) }),
      h('button', { class: 'btn sm danger', text: 'Delete…', onclick: () => confirmDeleteFinding(f) }),
    ),
    f.description || f.exchange_ids.length
      ? h(
          'div',
          { class: 'fb' },
          f.description || null,
          f.exchange_ids.length ? h('div', { class: 'evid' }, f.exchange_ids.map((id) => h('button', { text: 'request #' + id, title: 'Open in the Lens', onclick: () => showExchange(id) }))) : null,
        )
      : null,
  );
}

async function setFindingStatus(f, select) {
  try {
    await api(`/api/findings/${f.id}`, { method: 'PATCH', body: { status: select.value } });
    toast(`Finding #${f.id}: ${FINDING_STATUSES[select.value]}`, 'ok');
    loadFindings();
  } catch (e) {
    select.value = f.status;
    toast(e.message, 'err');
  }
}

function confirmDeleteFinding(f) {
  const m = modal(
    'Delete this finding?',
    [h('p', { text: `#${f.id} ${f.title}` }), h('p', { class: 'muted', text: 'The finding is deleted for good. The requests it points to stay in Traffic. To keep it on record instead, set its status to False positive or Fixed.' })],
    [
      h('button', { class: 'btn', text: 'Cancel', onclick: closeModal }),
      h('button', {
        class: 'btn primary',
        text: 'Delete',
        onclick: async () => {
          try {
            await api(`/api/findings/${f.id}`, { method: 'DELETE' });
            closeModal();
            toast(`Finding #${f.id} deleted`, 'ok');
            loadFindings();
          } catch (e) {
            m.err.textContent = e.message;
          }
        },
      }),
    ],
  );
}

/** The findings report, as the engine writes it (false positives left out). */
async function fetchReport(format) {
  let resp;
  try {
    resp = await fetch('/api/findings/export?format=' + format, { headers: { Authorization: 'Bearer ' + S.token, 'X-Plonix-Client': 'gui' }, cache: 'no-store' });
  } catch (_) {
    throw new ApiError(0, 'engine_down', 'The Plonix engine is not reachable.');
  }
  if (!resp.ok) {
    const data = await resp.json().catch(() => null);
    throw new ApiError(resp.status, (data && data.code) || 'error', (data && data.error) || resp.statusText, data);
  }
  const name = ((resp.headers.get('content-disposition') || '').match(/filename="([^"]+)"/) || [])[1] || 'plonix-findings.' + format;
  return { name, blob: await resp.blob() };
}

function exportMenu(anchor) {
  closePopover();
  const save = async (format) => {
    closePopover();
    try {
      const { name, blob } = await fetchReport(format);
      const url = URL.createObjectURL(blob);
      const a = h('a', { href: url, download: name, hidden: true });
      document.body.append(a);
      a.click();
      a.remove();
      setTimeout(() => URL.revokeObjectURL(url), 10000);
      toast(`Exported ${name}`, 'ok');
    } catch (e) {
      toast(e.message, 'err');
    }
  };
  const copy = async () => {
    closePopover();
    try {
      copyText(await (await fetchReport('md')).blob.text());
    } catch (e) {
      toast(e.message, 'err');
    }
  };
  const item = (label, run) => h('button', { role: 'menuitem', text: label, onclick: run });
  const menu = h(
    'div',
    { class: 'ctxmenu', role: 'menu' },
    item('Markdown (.md)', () => save('md')),
    item('HTML page (.html)', () => save('html')),
    item('JSON (.json)', () => save('json')),
    h('div', { class: 'msep' }),
    item('Copy as Markdown', copy),
    h('div', { class: 'msep' }),
    h('div', { class: 'mnote', text: 'False positives are left out. Choose findings with plonix findings export.' }),
  );
  showPopover(menu, anchor.getBoundingClientRect());
}

function newFinding(ids, title) {
  findingForm(null, ids, title);
}

/** Records a new finding, or edits `f`. */
function findingForm(f, ids = [], title = '', hint = null) {
  const t = h('input', { value: f ? f.title : title || '', placeholder: 'e.g. IDOR on /v2/orders/{id} exposes other users’ addresses' });
  const sev = h('select', null, SEVERITIES.map((s) => h('option', { value: s, text: s, selected: s === (f ? f.severity : (hint && hint.severity) || 'medium') })));
  const desc = h('textarea', { placeholder: 'What happens, how to reproduce it, and why it matters.', value: f ? f.description : '' });
  const ex = f ? null : h('input', { value: ids.join(', '), placeholder: 'Request ids, e.g. 14, 22', oninput: () => !job.busy && writeIdle() });
  // Claude can write the title, severity and description from the evidence.
  const job = {};
  const writeNote = h('span', { class: 'muted fine' });
  const writeBtn = h('button', { class: 'btn sm askbtn', type: 'button' }, h('span', { class: 'askico', text: '✦' }), ' Write it with Claude');
  const evidence = () => (f ? f.exchange_ids : ex.value.split(/[\s,]+/).filter(Boolean).map((x) => Number(x.replace('#', '')))).filter((n) => Number.isInteger(n) && n > 0);
  const writeIdle = () => {
    writeBtn.disabled = false;
    writeBtn.lastChild.textContent = ' Write it with Claude';
    const n = evidence();
    writeNote.textContent = n.length ? `Shares request #${n[0]} and its response with Claude Code on this Mac.` : 'Add an evidence request first.';
  };
  writeBtn.onclick = async () => {
    if (job.busy) {
      job.stop = true;
      if (job.run) api(`/api/agents/run/${job.run}`, { method: 'DELETE' }).catch(() => {});
      job.busy = false;
      return writeIdle();
    }
    const n = evidence();
    if (!n.length) return (m.err.textContent = 'Add the request that shows the issue as evidence first.');
    Object.assign(job, { busy: true, stop: false, run: null });
    m.err.textContent = '';
    writeBtn.lastChild.textContent = ' Stop';
    writeNote.textContent = 'Claude is writing the finding…';
    try {
      const out = await writeFindingWithClaude(n, hint && hint.note, (msg) => (writeNote.textContent = msg), job);
      if (!out || job.stop || !m.el.isConnected) return;
      t.value = out.title;
      if (out.severity) sev.value = out.severity;
      desc.value = out.description;
      writeNote.textContent = 'Written by Claude. Check it, edit anything, then save.';
    } catch (e) {
      if (!job.stop) m.err.textContent = e.message;
      writeIdle();
    } finally {
      job.busy = false;
      writeBtn.disabled = false;
      writeBtn.lastChild.textContent = ' Write it with Claude';
    }
  };
  writeIdle();
  const save = async () => {
    if (!t.value.trim()) return (m.err.textContent = 'Give the finding a title.');
    if (f) {
      try {
        await api(`/api/findings/${f.id}`, { method: 'PATCH', body: { title: t.value.trim(), severity: sev.value, description: desc.value } });
        closeModal();
        toast(`Finding #${f.id} saved`, 'ok');
        if (S.view === 'findings') loadFindings();
      } catch (e) {
        m.err.textContent = e.message;
      }
      return;
    }
    const exchange_ids = ex.value
      .split(/[\s,]+/)
      .filter(Boolean)
      .map((x) => Number(x.replace('#', '')));
    if (exchange_ids.some((n) => !Number.isInteger(n))) return (m.err.textContent = 'Request ids must be numbers.');
    try {
      const created = await api('/api/findings', { method: 'POST', body: { title: t.value.trim(), severity: sev.value, description: desc.value, exchange_ids } });
      closeModal();
      toast(`Finding #${created.id} recorded`, 'ok');
      S.findingsCount = (S.findingsCount || 0) + 1;
      updateChrome();
      if (S.view === 'findings') loadFindings();
    } catch (e) {
      m.err.textContent = e.message;
    }
  };
  const m = modal(
    f ? `Edit finding #${f.id}` : 'New finding',
    [
      hint ? h('div', { class: 'fhint', text: 'Plonix noticed: ' + hint.note }) : null,
      agentsOn() ? h('div', { class: 'fwrite' }, writeBtn, writeNote) : null,
      h('label', null, 'Title', t),
      h('label', null, 'Severity', sev),
      h('label', null, 'Description', desc),
      ex ? h('label', null, 'Evidence (request ids)', ex) : null,
    ],
    [h('button', { class: 'btn', text: 'Cancel', onclick: closeModal }), h('button', { class: 'btn primary', text: f ? 'Save' : 'Save finding', onclick: save })],
  );
  m.el.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) save();
  });
}

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
const SC = { host: null, hosts: [], suggest: null, suggestErr: null, picks: null, intrusive: false, running: false, report: null, crawl: null, crawling: false, crawlStart: '/', crawlBrowser: false, crawlClick: false };

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
      h('div', { class: 'pane' }, h('div', { class: 'stack', id: 'scanbody' }, h('div', { class: 'muted', text: 'Loading…' }))),
    ),
  );
  loadScans();
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
  SC.picks = new Set(sug.recommended.map((t) => t.id));
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
  const sel = h(
    'select',
    {
      onchange: () => {
        SC.host = sel.value;
        SC.suggest = null;
        SC.report = null;
        SC.crawl = null;
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
    h('p', { class: 'muted', text: 'Only accepted, in-scope hosts appear here.' }),
  );
  clear(box, targetCard, scanSuggestSection(), scanCrawlSection());
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
  const runBtn = h('button', { class: 'btn primary', disabled: SC.running || !picks.size, onclick: runScan }, SC.running ? 'Scanning…' : 'Run scan');
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
  try {
    SC.report = await api('/api/scan', { method: 'POST', body: { host: SC.host, tactics, include_intrusive: SC.intrusive } });
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
  return h(
    'div',
    { class: 'crawlreport' },
    h('div', { class: 'sechead' }, h('h4', { text: 'Crawl result' }), h('span', { class: 'muted', text: counts.join(' · ') }), h('span', { class: 'shacts' }, h('button', { class: 'btn sm', text: 'View on Map', onclick: () => go('map') }))),
    blocked.length
      ? h('p', { class: 'muted crawlblocked' }, 'Blocked, not in scope: ', blocked.map((b) => h('code', { text: b })))
      : null,
    r.forms.length
      ? h(
          'table',
          { class: 'grid scanforms' },
          h('thead', null, h('tr', null, h('th', { text: 'Method' }), h('th', { text: 'Action' }), h('th', { text: 'Fields' }))),
          h('tbody', null, r.forms.slice(0, 50).map((f) => h('tr', null, h('td', { text: f.method }), h('td', { text: f.action || '/' }), h('td', { text: f.fields.join(', ') || '—' })))),
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
    h('div', { class: 'sechead' }, h('h3', { text: 'Skills' }), h('button', { class: 'btn sm ghost', text: 'Get more in the Market', onclick: () => leaveTo('market') })),
    h('div', { class: 'card', id: 'agentskills' }, h('div', { class: 'ab muted', text: 'Loading skills…' })),
    h('div', { class: 'sechead' }, h('h3', { text: 'Try asking' })),
    h(
      'div',
      { class: 'card' },
      EXAMPLE_PROMPTS.map((p) => h('div', { class: 'prompt' }, h('span', { text: '“' + p + '”' }), h('button', { class: 'btn sm ghost', text: 'Copy', onclick: () => copyText(p) }))),
    ),
  );
  if (fresh) renderAgentSettings(settingsCard);
  loadAgentSkills();
}

/** Skills on the Agents screen: what agents are offered, and what is switched off. */
async function loadAgentSkills() {
  let r;
  try {
    r = await api('/api/skills');
  } catch (e) {
    return;
  }
  const box = $('#agentskills');
  if (!box) return;
  const skills = r.skills || [];
  clear(
    box,
    h('div', { class: 'ab muted', text: 'Playbooks agents follow for a job in Plonix. Claude Code offers them as slash commands; any MCP client sees them as prompts.' }),
    skills.map((sk) =>
      h(
        'div',
        { class: 'skillrow' + (sk.available ? '' : ' off') },
        h('div', { class: 'sk-main' }, h('b', { text: sk.title }), h('span', { class: 'muted', text: sk.description })),
        h('code', { class: 'sk-cmd', text: '/mcp__plonix__' + sk.name }),
        trustBadge(sk.verification, false),
        sk.available
          ? h('span', { class: 'tag in', text: 'offered' })
          : h('span', { class: 'tag out', title: 'Uses ' + sk.missing.map(groupLabel).join(', ') + ', which is switched off in Settings', text: 'off' }),
      ),
    ),
  );
}

/* ======================================================================
   Market: skills, rules, filters, bundles and extensions from a signed catalog
   ====================================================================== */

const KIND_INFO = {
  skill: { label: 'Skills', one: 'Skill', ico: '✦' },
  rules: { label: 'Rules', one: 'Rule pack', ico: '◎' },
  filters: { label: 'Filters', one: 'Filter pack', ico: '⧩' },
  list: { label: 'Lists', one: 'List pack', ico: '≣' },
  bundle: { label: 'Bundles', one: 'Bundle', ico: '❖' },
  extension: { label: 'Extensions', one: 'Extension', ico: '⬡' },
};

const GROUP_LABELS = { traffic: 'Traffic', insights: 'Insights', map: 'Map', scope: 'Scope', findings: 'Findings', scan: 'Scans' };
const groupLabel = (g) => GROUP_LABELS[g] || g;

const MK = { data: null, kind: 'all', q: '', sel: null, busy: null, ext: {} };

function renderMarket(main) {
  const q = h('input', {
    id: 'mq',
    placeholder: 'Search skills, rules, filters, bundles and extensions',
    spellcheck: 'false',
    autocomplete: 'off',
    value: MK.q,
    oninput: (e) => {
      MK.q = e.target.value;
      closePackage();
      drawMarket();
    },
  });
  clear(
    main,
    h(
      'div',
      { class: 'view market' },
      h(
        'div',
        { class: 'toolbar' },
        h('h2', { text: 'Market' }),
        h('div', { class: 'search' }, h('span', { class: 'mg', text: '⌕' }), q),
        h('button', { class: 'btn sm', id: 'mupdate', hidden: true, onclick: updateAll }),
        h('button', { class: 'btn sm', text: 'Add from a file or link', title: 'Add a skill, pack or extension from outside the Market. It is marked Not verified.', onclick: addExternal }),
        h('button', { class: 'iconbtn', title: 'Check the Market again', text: '↻', onclick: () => loadMarket(true) }),
      ),
      h('div', { class: 'mtrust', id: 'mtrust' }),
      h('div', { class: 'filterchips mkinds', id: 'mkinds' }),
      h('div', { class: 'mbody', id: 'mbody' }, h('div', { class: 'pane' }, h('div', { class: 'mgrid', id: 'mgrid' }, h('div', { class: 'muted', text: 'Loading the Market…' }))), h('div', { id: 'mdetail' })),
    ),
  );
  loadMarket(false);
}

async function loadMarket(refresh) {
  try {
    MK.data = await api('/api/market' + (refresh ? '?refresh=true' : ''));
    const ex = await api('/api/extensions').catch(() => ({ extensions: [] }));
    MK.ext = Object.fromEntries((ex.extensions || []).map((x) => [x.name, x]));
  } catch (e) {
    const grid = $('#mgrid');
    if (grid) clear(grid, h('div', { class: 'empty' }, h('h3', { text: 'The Market is not available' }), h('p', { text: e.message })));
    return;
  }
  if (S.view !== 'market') return;
  drawMarket();
  if (MK.sel) showPackage(MK.sel);
}

/** The trust mark shown next to every package: verified, built in, not verified, or changed. */
function trustBadge(v, full) {
  if (!v) return null;
  const cls = { verified: 'ok', built_in: 'in', unverified: 'warn', changed: 'bad' }[v.level] || 'warn';
  const mark = v.level === 'verified' || v.level === 'built_in' ? '✓' : '!';
  return h('span', { class: 'trust ' + cls, title: v.detail }, h('i', { text: mark }), full ? v.label : v.level === 'verified' ? 'Verified' : v.level === 'built_in' ? 'Built in' : v.level === 'changed' ? 'Changed' : 'Not verified');
}

const isUnverified = (p) => p.verification && ['unverified', 'changed'].includes(p.verification.level);

function marketStatus(p) {
  const st = p.status.state;
  if (st === 'built_in') return { text: 'Built in', cls: 'tag in', action: null };
  const x = p.kind === 'extension' && MK.ext[p.name];
  if (st === 'installed' && x && x.disabled_reason) return { text: 'Stopped', cls: 'tag bad', action: 'remove' };
  if (st === 'installed' && x && !x.enabled) return { text: 'Installed · off', cls: 'tag out', action: 'remove' };
  if (st === 'installed') return { text: 'Installed', cls: 'tag in', action: 'remove' };
  if (st === 'update') return { text: 'Update ' + p.status.installed + ' → ' + p.version, cls: 'tag upd', action: 'update' };
  if (st === 'needs_runtime') return { text: 'Coming soon', cls: 'tag out', action: null };
  return { text: 'Available', cls: 'tag out', action: 'install' };
}

function drawMarket() {
  const d = MK.data;
  if (!d) return;
  const trust = $('#mtrust');
  if (trust) {
    const ok = d.trust && d.trust.state === 'verified';
    clear(
      trust,
      h('span', { class: 'mshield' + (ok ? ' ok' : ' bad'), text: ok ? '✓' : '!' }),
      h('b', { text: ok ? 'Signed by ' + d.trust.publisher : 'Not signed' }),
      h('span', { class: 'muted', text: ok ? ' · every package is checked against the signed list before it installs' : ' · nobody vouches for this list' }),
      d.offline_reason ? h('span', { class: 'muted', title: d.offline_reason, text: ' · showing the copy built into Plonix' }) : null,
    );
  }
  const counts = { all: d.packages.length };
  for (const p of d.packages) counts[p.kind] = (counts[p.kind] || 0) + 1;
  counts.installed = d.packages.filter((p) => ['installed', 'update'].includes(p.status.state)).length;
  counts.unverified = d.packages.filter(isUnverified).length;
  const kinds = $('#mkinds');
  if (kinds) {
    const chip = (key, label) =>
      h('button', { class: 'chip' + (MK.kind === key ? ' on' : ''), onclick: () => ((MK.kind = key), drawMarket()) }, h('span', { text: label }), h('span', { class: 'n', text: counts[key] || 0 }));
    clear(kinds, chip('all', 'All'), Object.entries(KIND_INFO).map(([k, v]) => chip(k, v.label)), h('span', { class: 'fsep' }), chip('installed', 'Installed'), counts.unverified ? chip('unverified', 'Not verified') : null);
  }
  const updates = d.packages.filter((p) => p.status.state === 'update');
  const ub = $('#mupdate');
  if (ub) {
    ub.hidden = !updates.length;
    ub.textContent = updates.length === 1 ? 'Install 1 update' : `Install ${updates.length} updates`;
  }
  const q = MK.q.trim().toLowerCase();
  const list = d.packages.filter(
    (p) =>
      (MK.kind === 'all' || p.kind === MK.kind || (MK.kind === 'installed' && ['installed', 'update'].includes(p.status.state)) || (MK.kind === 'unverified' && isUnverified(p))) &&
      (!q || p.name.includes(q) || p.description.toLowerCase().includes(q) || (KIND_INFO[p.kind] || {}).one.toLowerCase().includes(q)),
  );
  const grid = $('#mgrid');
  if (!grid) return;
  if (!list.length) return clear(grid, h('div', { class: 'empty', text: MK.kind === 'installed' ? 'Nothing installed from the Market yet.' : 'Nothing matches.' }));
  clear(
    grid,
    list.map((p) => {
      const st = marketStatus(p);
      const k = KIND_INFO[p.kind] || { one: p.kind, ico: '•' };
      return h(
        'div',
        { class: 'mpkg' + (MK.sel === p.name ? ' sel' : '') + (isUnverified(p) ? ' unv' : ''), tabindex: 0, onclick: () => showPackage(p.name), onkeydown: (e) => e.key === 'Enter' && showPackage(p.name) },
        h('div', { class: 'mph' }, h('span', { class: 'mico k-' + p.kind, text: k.ico }), h('div', { class: 'mpn' }, h('b', { text: p.name }), h('span', { class: 'muted', text: k.one + ' · ' + p.version + (p.local ? ' · added by you' : '') })), h('span', { class: st.cls, text: st.text })),
        h('div', { class: 'mpd', text: p.description }),
        p.includes && p.includes.length ? h('div', { class: 'mpinc muted', text: 'Includes ' + p.includes.join(', ') }) : null,
        h(
          'div',
          { class: 'mpf' },
          trustBadge(p.verification, false),
          st.action ? marketButton(p, st.action, true) : null,
        ),
      );
    }),
  );
}

function marketButton(p, action, small) {
  const label = { install: p.kind === 'bundle' ? 'Install all' : 'Install', update: 'Update', remove: 'Remove' }[action];
  const busy = MK.busy === p.name;
  return h('button', {
    class: 'btn' + (small ? ' sm' : '') + (action === 'remove' ? ' danger ghost' : ' primary'),
    disabled: busy,
    text: busy ? 'Working…' : label,
    onclick: (e) => {
      e.stopPropagation();
      marketAction(p, action);
    },
  });
}

async function marketAction(p, action, consent) {
  if (action === 'remove' && p.kind === 'bundle' && !confirm(`Remove ${p.name} and the packages it installed?`)) return;
  if (action !== 'remove' && p.kind === 'extension' && !consent) return extensionConsent(p, action);
  MK.busy = p.name;
  drawMarket();
  try {
    const r = await api('/api/market/' + (action === 'remove' ? 'remove' : 'install'), { method: 'POST', body: { name: p.name, ...(consent || {}) } });
    const changed = (r.changes || []).filter((c) => c.action !== 'unchanged');
    const verb = action === 'remove' ? 'Removed' : action === 'update' ? 'Updated' : 'Installed';
    toast(changed.length > 1 ? `${verb} ${p.name} with ${changed.length - 1} more` : `${verb} ${p.name}`, 'ok');
  } catch (e) {
    toast(e.message, 'err');
  }
  MK.busy = null;
  await loadMarket(false);
  loadFacets();
}

/** Capabilities an extension asks for, with a checkbox for each sensitive one. Returns the boxes. */
function capabilityList(caps) {
  const boxes = [];
  const rows = caps.map((c) => {
    if (!c.sensitive) return h('div', { class: 'mcap' }, h('span', { text: '✓' }), c.what);
    const box = h('input', { type: 'checkbox', value: c.id });
    boxes.push(box);
    return h('div', { class: 'mcap warn' }, h('label', null, box, h('span', { text: '!' }), c.what + ' (only if you tick it)'));
  });
  return { rows, boxes };
}

/** Before an extension installs: what it may do, and an explicit yes for anything sensitive. */
async function extensionConsent(p, action) {
  let d;
  try {
    d = await api('/api/market/' + encodeURIComponent(p.name));
  } catch (e) {
    return toast(e.message, 'err');
  }
  const x = (d.detail && d.detail.extension) || {};
  if (!x.installable) return toast(x.why_not || `${p.name} cannot be installed in this version of Plonix`, 'err');
  const { rows, boxes } = capabilityList(x.capabilities || []);
  const go = h('button', { class: 'btn primary', text: action === 'update' ? 'Update' : 'Install' });
  modal(
    `${action === 'update' ? 'Update' : 'Install'} ${p.name}?`,
    [h('p', { class: 'muted mnote', text: 'It will be allowed to:' }), rows, h('p', { class: 'muted fine', text: x.sandbox })],
    [h('button', { class: 'btn', text: 'Cancel', onclick: closeModal }), go],
  );
  go.onclick = () => {
    closeModal();
    marketAction(p, action, { approve: true, grant: boxes.filter((b) => b.checked).map((b) => b.value) });
  };
}

/** The program an extension runs: installed on this Mac, or how to install it. */
function programNeeds(pr) {
  if (pr.found) return h('div', { class: 'mcap' }, h('span', { text: '✓' }), `${pr.id} is installed on this Mac.`);
  return [
    h('div', { class: 'mcap warn' }, h('span', { text: '!' }), `${pr.id} is not installed on this Mac yet. Install it in Terminal, then come back:`),
    h('div', { class: 'mneeds' }, h('code', { text: pr.install }), h('button', { class: 'btn sm', text: 'Copy', onclick: () => copyText(pr.install) })),
  ];
}

/** An installed extension: on or off, why Plonix stopped it, and a run over captured traffic. */
function extensionState(name) {
  const x = MK.ext[name];
  if (!x) return null;
  const box = h('div', { class: 'extstate' + (x.disabled_reason ? ' stopped' : '') });
  const draw = () => {
    const on = x.enabled;
    const toggle = h('button', {
      class: 'btn sm' + (on ? '' : ' primary'),
      text: on ? 'Switch off' : 'Switch on',
      onclick: async () => {
        try {
          const r = await api('/api/extensions/' + encodeURIComponent(name) + '/enabled', { method: 'PUT', body: { enabled: !on } });
          Object.assign(x, r.state);
          if (!r.state.disabled_reason) delete x.disabled_reason;
          toast(`${name} is ${x.enabled ? 'on' : 'off'}`, 'ok');
        } catch (e) {
          toast(e.message, 'err');
        }
        box.className = 'extstate' + (x.disabled_reason ? ' stopped' : '');
        draw();
        drawMarket();
      },
    });
    const run = h('button', {
      class: 'btn sm',
      text: 'Run on captured traffic',
      disabled: !on,
      title: 'Hand everything captured so far to this extension. New traffic reaches it on its own.',
      onclick: async () => {
        run.disabled = true;
        run.textContent = 'Running…';
        try {
          const r = await api('/api/extensions/' + encodeURIComponent(name) + '/run', { method: 'POST', body: {} });
          if (r.stopped) toast(`${name}: ${r.stopped}`, 'err');
          else if (r.problem) toast(`${name}: ${r.problem}`, 'err');
          else if (x.program) toast(`${name} checked ${r.exchanges} new request(s) and found ${r.notes} secret(s). They show in the Lens.`, 'ok');
          else toast(`${name} looked at ${r.exchanges} request(s): ${r.notes} note(s), ${r.proposed} new finding(s) to review`, 'ok');
        } catch (e) {
          toast(e.message, 'err');
        }
        await loadMarket(false);
      },
    });
    clear(
      box,
      h('div', { class: 'row' }, h('b', { text: x.disabled_reason ? 'Stopped' : on ? 'On' : 'Off' }), toggle, run),
      x.disabled_reason ? h('p', { text: x.disabled_reason }) : null,
      h('p', {
        class: 'muted',
        text: !on ? 'It is installed but does not run.' : x.program ? 'Secrets it finds show in the Lens as Spotted chips, marked with its name.' : 'Its notes show in the Lens, marked with its name. Findings it proposes stay open until you confirm them.',
      }),
    );
  };
  draw();
  return box;
}

/** Adds a file from outside the Market: look at it first, then confirm. It is always marked Not verified. */
function addExternal() {
  const input = h('input', { placeholder: 'https://example.com/skill.md  or  /path/to/pack.json  or  /path/to/extension', spellcheck: 'false', autocomplete: 'off' });
  let boxes = [];
  const preview = h('div', { class: 'xpreview' });
  const check = h('button', { class: 'btn', text: 'Look at it' });
  const confirmBtn = h('button', { class: 'btn primary', text: 'Add it, not verified', hidden: true });
  const m = modal(
    'Add from a file or link',
    [
      h('p', { class: 'muted mnote', text: 'A skill (Markdown), a rule, filter or list pack, or an extension (a .plonixext file or its folder). Plonix checks it in full, shows you what it does, and adds it only after you confirm. Nobody vouches for it, so it is marked Not verified.' }),
      h('label', null, 'Address or path', input),
      preview,
    ],
    [h('button', { class: 'btn', text: 'Cancel', onclick: closeModal }), check, confirmBtn],
  );
  const run = async (confirm) => {
    m.err.textContent = '';
    const source = input.value.trim();
    if (!source) return input.focus();
    check.disabled = confirmBtn.disabled = true;
    try {
      const grant = boxes.filter((b) => b.checked).map((b) => b.value);
      const r = await api('/api/market/add', { method: 'POST', body: { source, confirm, grant } });
      if (r.added) {
        closeModal();
        toast(`Added ${r.file.name} (not verified)`, 'ok');
        MK.kind = 'unverified';
        await loadMarket(true);
        return showPackage(r.file.name);
      }
      const f = r.file;
      const caps = f.capabilities ? capabilityList(f.capabilities) : { rows: [], boxes: [] };
      boxes = caps.boxes;
      clear(
        preview,
        h('div', { class: 'xhead' }, h('b', { class: 'mono', text: f.name }), h('span', { class: 'muted', text: ` ${KIND_INFO[f.kind].one} · ${f.version} · ${f.author}` })),
        h('p', { text: f.description }),
        f.kind === 'extension' ? [h('p', { class: 'muted', text: 'It will be allowed to:' }), caps.rows, h('p', { class: 'muted fine', text: f.effects[f.effects.length - 1] })] : f.effects.map((e) => h('div', { class: 'mcap' }, h('span', { text: '•' }), e)),
        f.replaces ? h('p', { class: 'muted', text: `This replaces ${f.name} ${f.replaces}, which is installed.` }) : null,
        h('div', { class: 'mtrustbox warn' }, h('span', { class: 'trust warn' }, h('i', { text: '!' }), 'Not verified'), h('p', { text: f.kind === 'extension' ? 'It did not come from a signed Market. Its code only runs in the sandbox, but nobody has reviewed what it does.' : 'It did not come from a signed Market. It is checked and cannot run code, but nobody has reviewed what it says or does.' })),
        h('p', { class: 'muted fine mono', text: 'sha256 ' + f.sha256 }),
      );
      confirmBtn.hidden = false;
    } catch (e) {
      m.err.textContent = e.message;
      confirmBtn.hidden = true;
      clear(preview);
    }
    check.disabled = confirmBtn.disabled = false;
  };
  check.onclick = () => run(false);
  confirmBtn.onclick = () => run(true);
  input.addEventListener('input', () => ((confirmBtn.hidden = true), clear(preview)));
  input.addEventListener('keydown', (e) => e.key === 'Enter' && run(false));
}

async function updateAll() {
  try {
    const r = await api('/api/market/update', { method: 'POST', body: {} });
    toast(`Updated ${(r.changes || []).length} package(s)`, 'ok');
  } catch (e) {
    toast(e.message, 'err');
  }
  loadMarket(false);
}

function closePackage() {
  MK.sel = null;
  const body = $('#mbody');
  if (body) body.classList.remove('paged');
  const slot = $('#mdetail');
  if (slot) clear(slot);
  for (const c of document.querySelectorAll('.mpkg')) c.classList.remove('sel');
}

/** An item's own page: what it is, what it does, what it needs, who made it and how far to trust it. */
async function showPackage(name) {
  MK.sel = name;
  const mbody = $('#mbody');
  if (mbody) mbody.classList.add('paged');
  for (const c of document.querySelectorAll('.mpkg')) c.classList.toggle('sel', c.querySelector('b').textContent === name);
  const slot = $('#mdetail');
  if (!slot) return;
  const p = MK.data && MK.data.packages.find((x) => x.name === name);
  if (!p) return;
  const panel = h('article', { class: 'mside mpage' }, h('div', { class: 'muted', text: 'Loading…' }));
  clear(slot, panel);
  let d;
  try {
    d = await api('/api/market/' + encodeURIComponent(name));
  } catch (e) {
    return clear(panel, h('div', { class: 'perr', text: e.message }));
  }
  if (MK.sel !== name) return;
  const k = KIND_INFO[p.kind] || { one: p.kind, ico: '•' };
  const st = marketStatus(p);
  const det = d.detail || {};
  const sec = (title, ...kids) => h('div', { class: 'msec' }, h('h4', { text: title }), kids);
  const parts = [];
  const about = p.about && p.about.length ? p.about : [p.description];
  parts.push(sec('About', about.map((t) => h('p', { class: 'mabout', text: t }))));
  const rows = [
    ['Type', k.one],
    ['Version', p.version + (st.action === 'update' ? ` (you have ${p.status.installed})` : '')],
    ['Made by', p.author + (p.local ? ' · added by you' : '')],
    ['Status', st.text],
    p.sha256 ? ['Checksum', h('span', { class: 'mono fine', text: 'sha256 ' + p.sha256 })] : null,
    p.url ? ['Source', h('span', { class: 'mono fine', text: p.url })] : null,
    p.homepage ? ['Homepage', h('a', { class: 'link', href: p.homepage, target: '_blank', rel: 'noopener', text: p.homepage })] : null,
  ].filter(Boolean);
  parts.push(sec('Details', h('dl', { class: 'mdl' }, rows.map(([a, b]) => [h('dt', { text: a }), h('dd', null, b)]))));
  const within = (MK.data.packages || []).filter((x) => x.kind === 'bundle' && (x.includes || []).includes(name) && x.name !== name);
  if (within.length) parts.push(sec('Part of', h('div', { class: 'mchips' }, within.map((b) => h('button', { class: 'chip', text: b.name, onclick: () => showPackage(b.name) })))));
  if (p.includes && p.includes.length) {
    parts.push(sec(p.kind === 'bundle' ? 'Installs' : 'Also installs', h('div', { class: 'mchips' }, p.includes.map((n) => h('button', { class: 'chip', text: n, onclick: () => showPackage(n) })))));
  }
  if (det.skill) {
    const sk = det.skill;
    parts.push(
      sec(
        'What agents can read with it',
        h('div', { class: 'mchips' }, sk.uses.map((g) => h('span', { class: 'chip' + (sk.missing.includes(g) ? ' k-neg' : ''), text: groupLabel(g) }))),
        h('p', { class: 'muted fine', text: sk.available ? 'Read-only, like every agent tool. A skill never gives an agent more than your agent settings allow.' : 'Agents will not be offered this skill: ' + sk.missing.map(groupLabel).join(', ') + ' is switched off in Settings › AI agents.' }),
      ),
    );
    if (sk.arguments.length) parts.push(sec('Asks for', sk.arguments.map((a) => h('div', { class: 'marg' }, h('code', { text: a.name }), h('span', { class: 'muted', text: (a.required ? '' : '(optional) ') + a.description })))));
    parts.push(sec('Instructions', h('pre', { class: 'mpre', text: sk.instructions })));
    parts.push(h('p', { class: 'muted fine' }, 'In Claude Code, run ', h('code', { text: '/mcp__plonix__' + p.name }), ' once it is installed.'));
  }
  if (det.extension) {
    const x = det.extension;
    const granted = x.installed ? x.installed.granted : null;
    if (x.installed) parts.unshift(sec('In this Plonix', extensionState(name)));
    if (x.program) parts.push(sec('Needs', programNeeds(x.program)));
    parts.push(
      sec(
        granted ? 'Allowed to' : 'Would be allowed to',
        x.capabilities.map((c) => {
          const off = granted && !granted.includes(c.id);
          return h('div', { class: 'mcap' + (off ? ' off' : c.sensitive ? ' warn' : '') }, h('span', { text: off ? '✗' : c.sensitive ? '!' : '✓' }), c.what + (off ? ' (not granted)' : c.sensitive && !granted ? ' (only if you say yes)' : ''));
        }),
        h('p', { class: 'muted fine', text: x.installable ? x.sandbox : x.why_not ? 'Not installable in this version: ' + x.why_not : 'Listed so you can see what is coming; its code is not published yet.' }),
      ),
    );
  }
  if (det.rules) parts.push(sec(`Detects ${det.rules.count} technologies`, h('div', { class: 'mchips' }, det.rules.detects.map((n) => h('span', { class: 'chip', text: n })))));
  if (det.filters) parts.push(sec('Filters', det.filters.map((f) => h('div', { class: 'marg' }, h('code', { text: 'is:' + f.id }), h('span', { class: 'muted', text: f.label + ' · ' + f.query })))));
  if (det.lists) parts.push(sec('Lists', det.lists.map((l) => h('div', { class: 'marg' }, h('code', { text: l.id }), h('span', { class: 'muted', text: `${l.title} · ${l.count} value${l.count === 1 ? '' : 's'}` })))));
  clear(
    panel,
    h('div', { class: 'mback' }, h('button', { class: 'btn sm', text: '← Market', title: 'Back to the list', onclick: () => (closePackage(), drawMarket()) })),
    h('div', { class: 'mside-h' }, h('span', { class: 'mico big k-' + p.kind, text: k.ico }), h('div', null, h('h3', { text: p.name }), h('div', { class: 'muted', text: `${k.one} · ${p.version} · ${p.author}` }))),
    h('p', { class: 'mdesc', text: p.description }),
    h('div', { class: 'mact' }, h('span', { class: st.cls, text: st.text }), st.action ? marketButton(p, st.action, false) : null, st.action === 'update' ? marketButton(p, 'remove', false) : null),
    h('div', { class: 'mtrustbox ' + ({ verified: 'ok', built_in: 'ok', unverified: 'warn', changed: 'bad' }[p.verification.level] || 'warn') }, trustBadge(p.verification, true), h('p', { text: p.verification.detail })),
    parts,
  );
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
const fmtDur = (ms) => {
  const s = Math.floor(ms / 1000);
  return Math.floor(s / 60) + ':' + String(s % 60).padStart(2, '0');
};
/** How much Claude has read and written so far, and for how long: "38k read · 120 written · 0:14". */
const claudeStats = (p) => [p.tokens_in ? fmtTok(p.tokens_in) + ' tokens read' : null, p.tokens_out ? fmtTok(p.tokens_out) + ' written' : null, fmtDur(p.elapsed_ms)].filter(Boolean).join(' · ');
/**
 * Claude's Markdown answer as DOM nodes: headings, paragraphs, lists, quotes,
 * tables, rules, fenced code (with Copy), and inline code, bold, italics and
 * links. Built with text nodes only, never innerHTML, since answers quote
 * captured traffic. An unclosed fence (mid-stream) runs to the end.
 */
function mdNodes(src) {
  const lines = String(src).replace(/\r\n?/g, '\n').split('\n');
  const out = [];
  const isRule = (l) => /^\s{0,3}([-*_])(\s*\1){2,}\s*$/.test(l);
  const isRow = (l) => /^\s*\|.*\|\s*$/.test(l);
  const cells = (l) => l.trim().replace(/^\||\|$/g, '').split('|').map((c) => c.trim());
  const listRe = /^(\s*)([-*+]|\d+[.)])\s+(.*)$/;
  const startsBlock = (l) => /^\s*(```|~~~|#{1,6}\s|>)/.test(l) || listRe.test(l) || isRule(l) || isRow(l);
  let i = 0;
  while (i < lines.length) {
    const l = lines[i];
    if (!l.trim()) {
      i++;
      continue;
    }
    const fence = l.match(/^\s*(```|~~~)\s*([\w+-]*)/);
    if (fence) {
      const body = [];
      for (i++; i < lines.length && !lines[i].trim().startsWith(fence[1]); i++) body.push(lines[i]);
      i++;
      const code = body.join('\n').replace(/\n+$/, '');
      const copy = h('button', { class: 'mdcopy', text: 'Copy', title: 'Copy to the clipboard' });
      copy.onclick = async () => {
        await copyText(code);
        copy.textContent = 'Copied';
        setTimeout(() => (copy.textContent = 'Copy'), 1200);
      };
      out.push(h('div', { class: 'mdcode' }, copy, h('pre', null, h('code', { text: code }))));
      continue;
    }
    const head = l.match(/^\s*(#{1,6})\s+(.*?)\s*#*\s*$/);
    if (head) {
      out.push(h('h' + Math.min(6, head[1].length + 2), { class: 'mdh' }, mdInline(head[2])));
      i++;
      continue;
    }
    if (isRule(l)) {
      out.push(h('hr'));
      i++;
      continue;
    }
    if (/^\s*>/.test(l)) {
      const body = [];
      for (; i < lines.length && /^\s*>/.test(lines[i]); i++) body.push(lines[i].replace(/^\s*>\s?/, ''));
      out.push(h('blockquote', null, mdNodes(body.join('\n'))));
      continue;
    }
    if (isRow(l) && i + 1 < lines.length && /^\s*\|?[\s:|-]+\|?\s*$/.test(lines[i + 1]) && lines[i + 1].includes('-')) {
      const headCells = cells(l);
      const rows = [];
      for (i += 2; i < lines.length && isRow(lines[i]); i++) rows.push(cells(lines[i]));
      out.push(
        h(
          'div',
          { class: 'mdtable' },
          h('table', null, h('thead', null, h('tr', null, headCells.map((c) => h('th', null, mdInline(c))))), h('tbody', null, rows.map((r) => h('tr', null, r.map((c) => h('td', null, mdInline(c))))))),
        ),
      );
      continue;
    }
    const li = l.match(listRe);
    if (li) {
      const ordered = /\d/.test(li[2]);
      const indent = li[1].length;
      const items = [];
      while (i < lines.length) {
        const m = lines[i].match(listRe);
        if (m && m[1].length <= indent + 1 && /\d/.test(m[2]) === ordered) {
          items.push({ text: [m[3]], start: Number.parseInt(m[2], 10) });
          i++;
        } else if (lines[i].trim() && items.length && (/^\s{2,}/.test(lines[i]) || !startsBlock(lines[i])) && !(m && m[1].length <= indent)) {
          // A wrapped line or a nested item: it belongs to the last item.
          items[items.length - 1].text.push(lines[i].replace(new RegExp('^\\s{0,' + (indent + 3) + '}'), ''));
          i++;
        } else if (!lines[i].trim() && i + 1 < lines.length && (/^\s{2,}\S/.test(lines[i + 1]) || ((lines[i + 1].match(listRe) || [])[1] || '').length === indent)) {
          i++;
        } else break;
      }
      const list = h(ordered ? 'ol' : 'ul', ordered && items[0].start !== 1 ? { start: items[0].start } : null);
      for (const it of items) list.append(h('li', null, it.text.length > 1 ? mdNodes(it.text.join('\n')) : mdInline(it.text[0])));
      out.push(list);
      continue;
    }
    const para = [];
    for (; i < lines.length && lines[i].trim() && (!para.length || !startsBlock(lines[i])); i++) para.push(lines[i].trim());
    const p = h('p');
    para.forEach((t, k) => {
      if (k) p.append(h('br'));
      append(p, mdInline(t));
    });
    out.push(p);
  }
  return out;
}

/** Inline Markdown: `code`, **bold**, *italics*, ~~strike~~ and [links](url). */
function mdInline(t) {
  const out = [];
  const re = /(`+)([\s\S]*?[^`])\1(?!`)|\*\*([\s\S]+?)\*\*|__([\s\S]+?)__|~~([\s\S]+?)~~|\[([^\]]+)\]\((https?:\/\/[^)\s]+)\)|(?<![\w*])\*(?!\s)([^*]+?)\*(?![\w*])|(?<!\w)_(?!\s)([^_]+?)_(?!\w)/g;
  let last = 0;
  for (let m; (m = re.exec(t)); ) {
    if (m.index > last) out.push(t.slice(last, m.index));
    if (m[1]) out.push(h('code', { text: m[2].replace(/^ (.*) $/, '$1') }));
    else if (m[3] || m[4]) out.push(h('strong', null, mdInline(m[3] || m[4])));
    else if (m[5]) out.push(h('s', null, mdInline(m[5])));
    else if (m[6]) out.push(h('a', { href: m[7], target: '_blank', rel: 'noopener noreferrer', title: m[7] }, mdInline(m[6])));
    else out.push(h('em', null, mdInline(m[8] || m[9])));
    last = re.lastIndex;
  }
  if (last < t.length) out.push(t.slice(last));
  return out;
}

/** Claude Code says nothing for this long: tell the user it is still waiting, not frozen. */
const CLAUDE_QUIET_MS = 15000;

/**
 * The Ask sheet: shows exactly what will be shared and how big it is, lets
 * the user edit the question, drop parts and shorten bodies, and asks for
 * an explicit confirmation when it is larger than their limit.
 */
async function askClaude(subject, opts = {}) {
  const st = { exclude: [], question: opts.question || null, max: null, bundle: null, confirmBig: false };
  // The live in-app conversation, if one has been started.
  const convo = { id: null, since: 0, sessionId: null, running: false, proposed: false };

  /* ---- compose view (what gets shared) ---- */
  const q = h('textarea', { class: 'askq', rows: 3, value: opts.question || '' });
  const partsBox = h('div', { class: 'askparts' });
  const meter = h('div', { class: 'askmeter' });
  const warn = h('div', { class: 'askwarn', hidden: true });
  const clipSel = h('select', { title: 'Each request and response body is clipped to this length' }, [1000, 2000, 4000, 8000, 16000, 50000].map((n) => h('option', { value: n, text: fmtTok(n) + ' chars' })));
  const cliHint = h('p', { class: 'muted fine', hidden: true });
  const composeView = h(
    'div',
    null,
    h('label', null, 'Your question', q),
    h('div', { class: 'askhead' }, h('span', { text: 'What Claude Code gets' }), h('label', { class: 'askclip' }, 'Bodies up to ', clipSel)),
    partsBox,
    meter,
    warn,
    h('p', { class: 'muted fine', text: 'Only what is ticked is sent, straight from this Mac to Claude Code. It may include passwords or session tokens from captured traffic.' }),
    cliHint,
  );

  /* ---- conversation view (the answer, in-app) ---- */
  const transcript = h('div', { class: 'convo' });
  const followIn = h('textarea', { class: 'cfollow', rows: 1, placeholder: 'Ask a follow-up…' });
  const sendBtn = h('button', { class: 'btn primary sm', text: 'Send' });
  const followRow = h('div', { class: 'cfollowrow', hidden: true }, followIn, sendBtn);
  const convoView = h('div', { hidden: true }, transcript, followRow);

  /* ---- footer buttons ---- */
  const copyBtn = h('button', { class: 'btn', text: 'Copy prompt' });
  const termBtn = h('button', { class: 'btn', text: 'Open in Terminal' });
  const askBtn = h('button', { class: 'btn primary', text: '✦ Ask Claude' });
  const stopBtn = h('button', { class: 'btn', text: 'Stop', hidden: true });
  const newBtn = h('button', { class: 'btn', text: 'New question', hidden: true });

  let askInApp = true;
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
    copyBtn.disabled = blocked;
    termBtn.disabled = blocked;
    askBtn.disabled = blocked || !askInApp;
  };

  /* ---- view switching ---- */
  const showCompose = () => {
    composeView.hidden = false;
    convoView.hidden = true;
    copyBtn.hidden = termBtn.hidden = askBtn.hidden = false;
    stopBtn.hidden = newBtn.hidden = true;
    sync();
  };
  const showConvo = () => {
    composeView.hidden = true;
    convoView.hidden = false;
    copyBtn.hidden = termBtn.hidden = askBtn.hidden = true;
    newBtn.hidden = false;
  };

  /* ---- transcript rendering ---- */
  const scroll = () => (transcript.scrollTop = transcript.scrollHeight);
  // While Claude works: what it is doing, tokens read and written, time.
  let thinking = null;
  let draft = null;
  const setThinking = (on) => {
    if (on && !thinking) {
      thinking = h(
        'div',
        { class: 'cthink' },
        h('span', { class: 'dots' }, h('span', { class: 'dot' }), h('span', { class: 'dot' }), h('span', { class: 'dot' })),
        h('span', { class: 'cstep', text: 'Starting Claude Code' }),
        h('span', { class: 'cstat' }),
        h('div', { class: 'cquiet', hidden: true }),
      );
      transcript.append(thinking);
      scroll();
    } else if (!on && thinking) {
      thinking.remove();
      thinking = null;
    }
    if (!on) dropDraft();
  };
  const dropDraft = () => {
    if (draft) draft.remove();
    draft = null;
  };
  const showProgress = (p) => {
    if (!p || !thinking) return;
    thinking.querySelector('.cstep').textContent = p.step;
    thinking.querySelector('.cstat').textContent = claudeStats(p);
    const quiet = thinking.querySelector('.cquiet');
    quiet.hidden = p.idle_ms < CLAUDE_QUIET_MS;
    quiet.textContent = `No word from Claude Code for ${Math.round(p.idle_ms / 1000)}s. It may be busy or slow to connect; it is stopped after 2 minutes of silence.`;
    // The answer as it is being written, replaced by the finished text.
    if (p.draft) {
      if (!draft) {
        draft = answer('', 'draft');
        add(draft);
      }
      const atEnd = transcript.scrollHeight - transcript.scrollTop - transcript.clientHeight < 40;
      if (draft.dataset.src !== p.draft) {
        draft.dataset.src = p.draft;
        clear(draft.firstChild, mdNodes(p.draft));
      }
      if (atEnd) scroll();
    }
  };
  const add = (node) => {
    if (thinking) transcript.insertBefore(node, thinking);
    else transcript.append(node);
    scroll();
  };
  const bubble = (role, text) => h('div', { class: 'cmsg ' + role }, h('div', { class: 'cbub', text }));
  // Claude answers in Markdown; show it formatted.
  const answer = (text, extra = '') => h('div', { class: 'cmsg bot ' + extra }, h('div', { class: 'cbub md' }, mdNodes(text)));
  const sayError = (text) => add(h('div', { class: 'cerr', text }));
  const offerFallback = () => {
    copyBtn.hidden = termBtn.hidden = false;
  };
  // Claude suggested an edit to the Bench draft: point to the review there.
  // Nothing has changed yet; the Bench shows the diff with Apply and Discard.
  const offerReview = async () => {
    convo.proposed = false;
    if (subject.kind !== 'draft' || !subject.draft_id) return;
    let list = [];
    try {
      list = (await api('/api/bench/proposals?draft=' + encodeURIComponent(subject.draft_id))).proposals || [];
    } catch (_) {}
    if (!list.length || !alive()) return;
    add(
      h(
        'div',
        { class: 'cprop' },
        h('span', { text: 'Claude suggested an edit to this request. Your draft is unchanged until you apply it.' }),
        h('button', {
          class: 'btn primary sm',
          text: 'Review on the Bench',
          onclick: () => {
            closeModal();
            if (S.view === 'bench') drawProposal(R.tabs[R.active], $('#main'));
            else leaveTo('bench');
          },
        }),
      ),
    );
  };

  const alive = () => document.body.contains(transcript);
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

  const pollLoop = async () => {
    while (convo.running && alive()) {
      let snap;
      try {
        snap = await api(`/api/agents/run/${convo.id}?since=${convo.since}`);
      } catch (e) {
        setThinking(false);
        sayError(e.message);
        convo.running = false;
        break;
      }
      for (const ev of snap.events) {
        convo.since = ev.seq + 1;
        if (ev.type === 'text') {
          dropDraft();
          add(answer(ev.text));
        } else if (ev.type === 'tool') {
          add(h('div', { class: 'ctool', text: '✦ ' + ev.text }));
        } else if (ev.type === 'proposal') {
          add(h('div', { class: 'ctool', text: '✦ ' + ev.text }));
          convo.proposed = true;
        } else if (ev.type === 'error') {
          setThinking(false);
          sayError(ev.text);
          offerFallback();
        }
      }
      if (snap.session_id) convo.sessionId = snap.session_id;
      if (snap.status !== 'running') {
        convo.running = false;
        setThinking(false);
        const p = snap.progress;
        if (snap.status === 'done' && p) add(h('div', { class: 'cdone', text: `Answered in ${claudeStats(p).replace(/^(.*) · ([\d:]+)$/, '$2 · $1')}` }));
        if (convo.proposed) offerReview();
        if (snap.status === 'done') {
          followRow.hidden = false;
          followIn.disabled = sendBtn.disabled = false;
        }
        break;
      }
      // Keep the working indicator alive between turns.
      if (!thinking) setThinking(true);
      showProgress(snap.progress);
      await sleep(500);
    }
    stopBtn.hidden = true;
  };

  const runTurn = async (prompt, resume) => {
    convo.running = true;
    convo.since = 0;
    stopBtn.hidden = false;
    followIn.disabled = sendBtn.disabled = true;
    m.err.textContent = '';
    setThinking(true);
    try {
      const { id } = await api('/api/agents/run', { method: 'POST', body: { prompt, resume } });
      convo.id = id;
    } catch (e) {
      setThinking(false);
      convo.running = false;
      stopBtn.hidden = true;
      sayError(e.message);
      offerFallback();
      return;
    }
    pollLoop();
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
  askBtn.onclick = () => {
    if (!st.bundle) return;
    showConvo();
    add(bubble('user', st.bundle.question));
    runTurn(st.bundle.prompt, null);
  };
  sendBtn.onclick = () => {
    const t = followIn.value.trim();
    if (!t || convo.running) return;
    followIn.value = '';
    followRow.hidden = true;
    add(bubble('user', t));
    runTurn(t, convo.sessionId);
  };
  followIn.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      sendBtn.onclick();
    }
  });
  stopBtn.onclick = async () => {
    if (!convo.id) return;
    convo.running = false;
    stopBtn.hidden = true;
    setThinking(false);
    try {
      await api(`/api/agents/run/${convo.id}`, { method: 'DELETE' });
    } catch (_) {}
    sayError('Stopped.');
    offerFallback();
  };
  newBtn.onclick = () => {
    if (convo.running) return;
    convo.id = convo.sessionId = null;
    clear(transcript);
    followRow.hidden = true;
    showCompose();
  };
  copyBtn.onclick = async () => {
    await copyText(st.bundle.prompt);
    toast('Prompt copied', 'ok');
  };
  termBtn.onclick = async () => {
    try {
      await api('/api/agents/launch', { method: 'POST', body: { prompt: st.bundle.prompt } });
      toast('Opened Claude Code in Terminal', 'ok');
    } catch (e) {
      if (e.code === 'unsupported') {
        await copyText(st.bundle.prompt);
        m.err.textContent = 'Opening a terminal works on macOS only. The prompt is on your clipboard: paste it into Claude Code.';
      } else m.err.textContent = e.message;
    }
  };

  const m = modal('Ask Claude Code', [composeView, convoView], [h('button', { class: 'btn', text: 'Close', onclick: closeModal }), newBtn, stopBtn, copyBtn, termBtn, askBtn]);
  m.el.querySelector('.mcard').classList.add('wide');

  // Is the Claude Code CLI installed here? If not, keep Terminal/Copy only.
  try {
    const pol = await api('/api/agents');
    askInApp = pol.ask_in_app !== false;
  } catch (_) {}
  if (!askInApp) {
    cliHint.hidden = false;
    cliHint.textContent = 'Claude Code is not installed on this machine, so the answer cannot run inside Plonix yet. Install it from claude.com/claude-code, or use Open in Terminal.';
  }
  await rebuild();
}

/** What agents may do, in one line, with the way to change it: Settings › AI agents. */
async function renderAgentSettings(box) {
  let cfg;
  try {
    cfg = await api('/api/agents/settings');
  } catch (e) {
    return clear(box, h('div', { class: 'ab rerr', text: e.message }));
  }
  S.agentSettings = cfg;
  const st = cfg.settings;
  const on = cfg.groups.filter((g) => g.on).length;
  const summary = st.enabled
    ? `Agents see ${st.data === 'all' ? 'everything captured' : 'in-scope hosts only'} · ${on} of ${cfg.groups.length} kinds of data · Ask Claude up to ${fmtTok(st.context_budget)} tokens`
    : 'Agent access is off: every agent request is refused.';
  const open = () => {
    S.settingsSection = 'agents';
    leaveTo('settings');
  };
  clear(
    box,
    h(
      'div',
      { class: 'ab setrow' },
      h('span', null, h('b', { text: 'Agent settings' }), h('br'), h('span', { class: 'muted', text: summary })),
      h('button', { class: 'btn sm', text: 'Change in Settings…', onclick: open }),
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
    if (S.view === 'traffic' && T.picked.size && !typing) return clearPicked();
    if ($('#inspector') && !typing) return closeInspector();
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
  const keys = Object.keys(VIEWS).filter((k) => !VIEWS[k].footer);
  if (/^[1-9]$/.test(e.key) && keys[Number(e.key) - 1]) return go(keys[Number(e.key) - 1]);
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
  else if (e.key === 'i') setIntercept(!IC.on);
  else if (e.key === 'f' && IC.sel != null) forwardHeld(IC.sel);
  else if (e.key === 'd' && IC.sel != null) dropHeld(IC.sel);
});

/* ---------- settings ---------- */

async function renderSettings(main) {
  const box = h('div', { class: 'view settingsview' }, h('div', { class: 'empty', text: 'Loading settings…' }));
  main.append(box);
  let data;
  try {
    data = await api('/api/settings');
  } catch (e) {
    clear(box, h('div', { class: 'empty', text: e.message }));
    return;
  }
  if (S.view !== 'settings') return;
  const host = h('div', { style: { flex: '1', minHeight: '0', display: 'flex' } });
  const back = backButton();
  clear(box, back ? h('div', { class: 'toolbar' }, back) : null, host);
  PlonixSettings.render(host, data, {
    select: S.settingsSection || 'proxy',
    onSelect: (id) => (S.settingsSection = id),
    save: async (section, values) => {
      const r = await api('/api/settings/' + section, { method: 'PUT', body: { values } });
      if (section === 'agents') loadAgentSettings();
      if (section === 'intercept') loadIntercept();
      if (section === 'proxy') {
        S.status = await api('/api/status');
        updateChrome();
        r.message = 'Saved and applied. The proxy listens on ' + r.proxy + '.';
      }
      return r;
    },
    extra: (section, el) => {
      if (section.id === 'proxy') el.append(proxyPanel());
      if (section.id === 'storage') el.append(storagePanel());
      if (section.id === 'replace') el.append(replacePanel());
      if (section.id === 'client-certs') el.append(clientCertPanel());
    },
  });
}

function proxyPanel() {
  const st = S.status || {};
  return h(
    'div',
    { class: 'spanel' },
    h('h4', { text: 'Listening now' }),
    h('p', null, 'This project\'s proxy is at ', h('b', { class: 'mono', text: st.proxy || '…' }), '. Other open projects have proxies of their own.'),
    h('p', null, 'Devices that should capture through it need the Plonix certificate, from ', h('span', { class: 'mono', text: 'http://' + (st.proxy || '') + '/ca.pem' }), ' through the proxy.'),
  );
}

function storagePanel() {
  const panel = h('div', { class: 'spanel' }, h('h4', { text: 'Out-of-scope traffic' }), h('p', { text: 'Counting…' }));
  api('/api/storage')
    .then((s) => {
      const st = s.stats;
      const rows = [
        h('h4', { text: 'Out-of-scope traffic' }),
        h('p', { text: `${st.out_of_scope} of ${st.total} captured request(s) are to hosts that are not in scope.` }),
      ];
      if (s.last_prune) {
        const r = s.last_prune;
        rows.push(h('p', { class: 'muted', text: r.skipped ? `Last time: ${r.skipped}.` : `Last time (${fmtDate(r.at)}): deleted ${r.removed}, kept ${r.kept}.` }));
      }
      if (!st.in_scope_rules) rows.push(h('p', { class: 'muted', text: 'Nothing is in scope yet, so nothing would be deleted.' }));
      const btn = h('button', { class: 'btn danger', text: 'Delete Out-of-Scope Traffic Now…', disabled: !st.in_scope_rules || !st.out_of_scope, onclick: () => confirmPrune(st) });
      rows.push(h('div', { class: 'row' }, btn));
      clear(panel, rows);
    })
    .catch((e) => clear(panel, h('p', { text: e.message })));
  return panel;
}

const REPLACE_TARGETS = [
  ['request_line', 'Request line'],
  ['request_header', 'Request headers'],
  ['request_body', 'Request body'],
  ['response_header', 'Response headers'],
  ['response_body', 'Response body'],
];

/** Match-and-replace rules: list, switch on or off, remove, add. */
function replacePanel() {
  const panel = h('div', { class: 'spanel replace' }, h('h4', { text: 'Rules' }), h('p', { text: 'Loading…' }));
  const label = (t) => (REPLACE_TARGETS.find(([k]) => k === t) || [t, t])[1];
  const call = async (path, opts) => {
    try {
      await api(path, opts);
      load();
      return true;
    } catch (e) {
      toast(e.message, 'err');
      return false;
    }
  };
  const row = (r) =>
    h(
      'div',
      { class: 'rrule' + (r.enabled ? '' : ' off') },
      h('input', { type: 'checkbox', checked: r.enabled, title: r.enabled ? 'On: switch off' : 'Off: switch on', onchange: (e) => call('/api/replace/' + r.id, { method: 'PATCH', body: { enabled: e.target.checked } }) }),
      h('span', { class: 'rtarget', text: label(r.target) }),
      h('span', { class: 'mono rpat', text: r.pattern, title: r.regex ? 'Regular expression' : 'Literal text' }),
      h('span', { class: 'muted', text: '→' }),
      h('span', { class: 'mono rpat', text: r.replace === '' ? '(removed)' : r.replace }),
      r.regex ? h('span', { class: 'tag', text: 'regex' }) : null,
      r.in_scope_only ? h('span', { class: 'tag', text: 'in scope only' }) : null,
      r.note ? h('span', { class: 'muted', text: r.note }) : null,
      h('button', { class: 'iconbtn', text: '✕', title: 'Remove this rule', onclick: () => call('/api/replace/' + r.id, { method: 'DELETE' }) }),
    );
  const form = () => {
    const target = h('select', null, REPLACE_TARGETS.map(([k, l]) => h('option', { value: k, text: l })));
    const pattern = h('input', { type: 'text', placeholder: 'Match, e.g. (?i)^user-agent: .*$', spellcheck: false });
    const replace = h('input', { type: 'text', placeholder: 'Replace with (empty removes the match)', spellcheck: false });
    const regex = h('input', { type: 'checkbox' });
    const scoped = h('input', { type: 'checkbox' });
    const note = h('input', { type: 'text', placeholder: 'Note (optional)' });
    const add = async () => {
      if (!pattern.value) return pattern.focus();
      // In a text field, \n stands for a line break, so header rules can add a header.
      const body = { target: target.value, match: pattern.value, replace: replace.value.replace(/\\n/g, '\n'), regex: regex.checked, in_scope_only: scoped.checked, note: note.value };
      if (await call('/api/replace', { method: 'POST', body })) toast('Rule added. It applies to traffic from now on.', 'ok');
    };
    return h(
      'div',
      { class: 'rform' },
      h('div', { class: 'row' }, target, pattern, replace),
      h(
        'div',
        { class: 'row' },
        h('label', null, regex, ' Regular expression ($1 inserts a capture)'),
        h('label', null, scoped, ' In-scope hosts only'),
        note,
        h('button', { class: 'btn primary', text: 'Add Rule', onclick: add }),
      ),
      h('p', { class: 'muted', text: 'Header rules see one "Name: value" line per header: replace a whole line with nothing to remove a header, or use \\n in the replacement to add one.' }),
    );
  };
  const load = () =>
    api('/api/replace')
      .then((v) => {
        const rows = [h('h4', { text: 'Rules, in the order they apply' })];
        if (!v.enabled) rows.push(h('p', { class: 'muted', text: 'Match and replace is switched off above; these rules change nothing until it is on.' }));
        if (!v.rules.length) rows.push(h('p', { class: 'muted', text: 'No rules yet.' }));
        rows.push(v.rules.map(row), form());
        clear(panel, rows);
      })
      .catch((e) => clear(panel, h('p', { text: e.message })));
  load();
  return panel;
}

/** Client certificates: list, remove, add from PEM or .p12 files. Keys never come back from the engine. */
function clientCertPanel() {
  const panel = h('div', { class: 'spanel replace certs' }, h('h4', { text: 'Certificates' }), h('p', { text: 'Loading…' }));
  const row = (c) => {
    const until = c.not_after ? new Date(c.not_after).toISOString().slice(0, 10) : '';
    return h(
      'div',
      { class: 'rrule' + (c.problem || c.expired ? ' off' : '') },
      h('span', { class: 'rtarget mono', text: c.host }),
      h('span', { text: c.subject || 'certificate #' + c.id, title: 'Issued by ' + (c.issuer || 'unknown') + '\nSHA-256 ' + c.fingerprint }),
      until ? h('span', { class: 'muted', text: (c.expired ? 'expired ' : 'until ') + until }) : null,
      c.chain > 1 ? h('span', { class: 'tag', text: c.chain + ' in chain' }) : null,
      c.problem ? h('span', { class: 'tag rej', text: 'cannot be used', title: c.problem }) : null,
      c.note ? h('span', { class: 'muted', text: c.note }) : null,
      h('button', {
        class: 'iconbtn',
        text: '✕',
        title: 'Remove this certificate',
        onclick: async () => {
          try {
            await api('/api/client-certs/' + c.id, { method: 'DELETE' });
            load();
          } catch (e) {
            toast(e.message, 'err');
          }
        },
      }),
    );
  };
  const fileText = (input, binary) =>
    new Promise((resolve, reject) => {
      const f = input.files && input.files[0];
      if (!f) return resolve(null);
      const r = new FileReader();
      r.onload = () => resolve(binary ? r.result.split(',')[1] || '' : r.result);
      r.onerror = () => reject(new Error('Could not read ' + f.name));
      if (binary) r.readAsDataURL(f);
      else r.readAsText(f);
    });
  const form = () => {
    const host = h('input', { type: 'text', placeholder: 'api.example.com or *.example.com', spellcheck: false });
    const cert = h('input', { type: 'file', accept: '.pem,.crt,.cer,.key,.p12,.pfx' });
    const key = h('input', { type: 'file', accept: '.pem,.key' });
    const password = h('input', { type: 'password', placeholder: '.p12 password', autocomplete: 'off' });
    const note = h('input', { type: 'text', placeholder: 'Note (optional)' });
    const keyRow = h('label', null, 'Key ', key);
    const passRow = h('label', { hidden: true }, password);
    cert.addEventListener('change', () => {
      const p12 = /\.(p12|pfx)$/i.test((cert.files[0] || {}).name || '');
      keyRow.hidden = p12;
      passRow.hidden = !p12;
    });
    const add = async () => {
      if (!host.value.trim()) return host.focus();
      if (!cert.files.length) return toast('Choose the certificate file (.pem, or .p12 with its key inside).', 'err');
      try {
        const p12 = !passRow.hidden;
        const body = { host: host.value.trim(), note: note.value };
        if (p12) {
          body.pkcs12_base64 = await fileText(cert, true);
          body.password = password.value;
        } else {
          body.cert_pem = await fileText(cert, false);
          const k = await fileText(key, false);
          if (k) body.key_pem = k;
        }
        const c = await api('/api/client-certs', { method: 'POST', body });
        toast(`Added. Plonix presents ${c.subject || 'it'} when ${c.host} asks for a certificate.`, 'ok');
        load();
      } catch (e) {
        toast(e.message, 'err');
      }
    };
    return h(
      'div',
      { class: 'rform' },
      h('div', { class: 'row' }, host),
      h('div', { class: 'row' }, h('label', null, 'Certificate ', cert), keyRow, passRow),
      h('div', { class: 'row' }, note, h('button', { class: 'btn primary', text: 'Add Certificate', onclick: add })),
      h('p', { class: 'muted', text: 'PEM: a certificate (chain) and an unencrypted key, in one file or two. PKCS#12: one .p12 or .pfx file and its password. The key is kept in this project and never shown again.' }),
    );
  };
  const load = () =>
    api('/api/client-certs')
      .then((v) => {
        const rows = [h('h4', { text: 'Certificates' })];
        if (!v.enabled) rows.push(h('p', { class: 'muted', text: 'Client certificates are switched off above; none is presented until it is on.' }));
        if (!v.certs.length) rows.push(h('p', { class: 'muted', text: 'No certificates yet.' }));
        rows.push(v.certs.map(row), form());
        clear(panel, rows);
      })
      .catch((e) => clear(panel, h('p', { text: e.message })));
  load();
  return panel;
}

function confirmPrune(st) {
  const m = modal(
    'Delete out-of-scope traffic?',
    [
      h('p', { text: `This permanently deletes ${st.out_of_scope} request(s) to hosts that are not in scope, then compacts the project file. Requests that findings point to are kept.` }),
      h('p', { class: 'muted', text: 'Scope suggestions that relied on that traffic go away too.' }),
    ],
    [
      h('button', { class: 'btn', text: 'Cancel', onclick: closeModal }),
      h('button', {
        class: 'btn primary',
        text: 'Delete',
        onclick: async () => {
          try {
            const r = await api('/api/storage/prune', { method: 'POST', body: { confirm: true } });
            closeModal();
            toast(r.skipped ? r.skipped : `Deleted ${r.removed} request(s). ${r.kept} kept.`, 'ok');
            exCache.clear();
            go('settings', true);
          } catch (e) {
            m.err.textContent = e.message;
          }
        },
      }),
    ],
  );
}

// Entry points for the Plonix app's menu bar.
window.plonix = {
  go: (view) => S.token && $('#main') && go(view),
  openTarget: () => S.token && $('#main') && openTarget(),
  toggleSidebar: () => S.token && $('#main') && toggleSidebar(),
  importHar: () => S.token && $('#main') && importHar(),
  exportHar: () => S.token && $('#main') && exportHar({ q: S.view === 'traffic' ? fullQuery() : '', ids: S.view === 'traffic' ? [...T.picked] : [] }),
};

boot();
