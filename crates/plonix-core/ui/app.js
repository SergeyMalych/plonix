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
  applyDensity(store('plonix.density') || 'dense');
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
  if (S.status.demo && !pstore('plonix.demoTourSeen')) autoTour();
}

/** Offers the walkthrough the first time the demo opens, once any
 * first-launch question has been answered. */
function autoTour() {
  if ($('.modal')) return setTimeout(autoTour, 400);
  if (!pstore('plonix.demoTourSeen') && !TOUR.el) startTour();
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
      h('button', { class: 'btn sm primary', text: 'Take the tour', title: 'A short walk through every part of Plonix  (about two minutes)', onclick: () => startTour() }),
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

/* ---------- the demo walkthrough ---------- */

/** A guided walk through every part of Plonix, over the demo's own data.
 * Each step opens a screen and rings the part it talks about; a step whose
 * part isn't on screen still shows its card, just without the ring. */
const TOUR_STEPS = [
  {
    title: 'Welcome to the Plonix demo',
    text: 'Brightcart is a made-up shop whose traffic was captured ahead of time. This walk takes about two minutes and stops on each part of Plonix. Use the arrow keys or the buttons, and leave whenever you like.',
    view: 'traffic',
  },
  {
    view: 'traffic',
    target: '#searchbox',
    title: 'Traffic',
    text: 'Every request your browser makes through Plonix lands here, live. Search any text, or type a filter such as host:, status:, path: or method: and press Enter.',
  },
  {
    view: 'traffic',
    target: '#chips',
    title: 'Filters',
    text: 'Filters are chips: + shows only what matches, − hides it. The Suggested chips come from this traffic, so the useful ones are a click away. Filters are saved with the project.',
    action: { label: 'See ready-made filters', run: () => filterTour() },
  },
  {
    view: 'traffic',
    target: '#groupseg',
    title: 'Grouping',
    text: 'The same request sent several times in a row folds into one ×N row with its time span. Click the row to expand it, or switch to Every request.',
  },
  {
    view: 'traffic',
    prep: async () => {
      const r = await api('/api/traffic?limit=1&q=' + encodeURIComponent('path:/v1/orders/1042 mime:json'));
      if (r.items.length) await openInspector(r.items[0].id);
    },
    target: '#inspector',
    title: 'Lens',
    text: 'Lens shows the request and response, and points out what matters in them: tokens, emails and card numbers in this order. Select any text in it to decode it, find it in other traffic or ask Claude about it.',
  },
  {
    view: 'traffic',
    target: ['.lenssugg:not([hidden])', '#inspector'],
    title: 'Suggestions',
    text: 'Plonix reads the request and suggests next steps that fit it, one click each, such as drafting a finding or getting ideas for this endpoint. Nothing is sent until you click.',
  },
  {
    view: 'scope',
    target: ['#scopebody .card.sugg', '#scopebody'],
    title: 'Scope',
    text: 'As you browse, Plonix spots domains that belong to your target and shows why. You accept or reject each one, and anything that sends requests stays inside what you accepted.',
  },
  {
    view: 'map',
    target: ['#hostlist', '#main .view'],
    title: 'Map',
    text: 'Hosts, endpoints and parameters learned from the traffic, with the technologies Plonix detected. Click any endpoint to see its requests in place.',
  },
  {
    view: 'bench',
    target: ['.reqbar', '#main .view'],
    title: 'Bench',
    text: 'Each tab is an experiment: edit a request, send it, branch it and compare the responses. The demo comes with three ready-made experiments.',
  },
  {
    view: 'bench',
    target: ['.runconf', '#main .view'],
    title: 'Run',
    text: 'Mark a value with • and Run sends the request once per value, with a sensible list picked for you. Here it walks the order id through nearby numbers so you can spot orders that aren’t yours.',
  },
  {
    view: 'findings',
    target: ['#findbody > .finding', '#findbody'],
    title: 'Findings',
    text: 'Write up what you found, each finding tied to the requests that prove it, then export the lot as a report.',
  },
  {
    view: 'scans',
    target: ['#scanbody .scansug', '#scanbody'],
    title: 'Scans',
    text: 'Plonix lists what it sees in an in-scope host and recommends checks that fit it. You pick the checks and press Run; nothing scans on its own.',
  },
  {
    view: 'rules',
    target: ['.rllist', '#main .view'],
    title: 'Rules',
    text: 'Rules change traffic as it passes: send a header on every request, change or remove one, or replace any text. Each rule says where it applies (your browser, the Bench, Scans) and can have a condition, written like a Traffic search.',
  },
  {
    view: 'rules',
    prep: () => {
      ruleDialog({ pattern: 'X-Bug-Bounty', replace: 'brightcart-researcher' });
      // Shown, not for typing yet: the walk's keys and buttons stay in charge.
      $('.modal').classList.add('tourmodal');
      document.activeElement.blur();
    },
    target: '.rldialog',
    title: 'Adding a rule',
    text: 'Pick what the rule does, fill in a name and a value, and the preview shows what every request will carry. You can also right-click any header in the Lens to start a rule from it. The green pill in Traffic shows when rules are on.',
  },
  {
    view: 'market',
    target: ['#mkinds', '#main .view'],
    title: 'Market',
    text: 'Extensions, skills, filter packs and word lists, each signed and checked before it installs. Tools such as Saved users and the Access check, which replays requests as each user and signed out, are switched on from here.',
  },
  {
    view: 'programs',
    target: ['#progbody', '#main .view'],
    title: 'Programs',
    text: 'Connect a bug bounty platform and follow a program: its assets become your scope, and its rules, such as rate limits and required headers, are kept for you.',
  },
  {
    view: 'agents',
    target: '#main .view > .toolbar',
    title: 'Agents',
    text: 'Let an AI agent such as Claude Code work with this project over MCP: ask about the traffic, the map and the findings, and see what each answer was based on.',
  },
  {
    title: 'That’s the tour',
    text: 'Press Open target to capture a site of your own. The demo stays on the Start screen, and you can take this walk again with Take the tour in the demo strip.',
    view: 'traffic',
  },
];

const TOUR = { i: -1, el: null, spot: null, timer: null, keys: null };

/** Starts the walk at the first step, or at `at`. */
function startTour(at = 0) {
  endTour();
  pstore('plonix.demoTourSeen', true);
  TOUR.spot = h('div', { class: 'tourspot', hidden: true });
  TOUR.el = h('div', { class: 'tourcard', role: 'dialog', 'aria-label': 'Demo walkthrough' });
  document.body.append(TOUR.spot, TOUR.el);
  TOUR.keys = (e) => {
    if ($('.modal:not(.tourmodal), .popover, .ctxmenu')) return;
    if (/^(INPUT|TEXTAREA|SELECT)$/.test(document.activeElement && document.activeElement.tagName)) return;
    const k = { ArrowRight: 1, ArrowLeft: -1, Escape: 0 }[e.key];
    if (k === undefined || e.metaKey || e.ctrlKey || e.altKey) return;
    e.preventDefault();
    e.stopPropagation();
    if (k === 0) endTour();
    else tourStep(TOUR.i + k);
  };
  document.addEventListener('keydown', TOUR.keys, true);
  window.addEventListener('resize', placeTour);
  // Screens draw parts of themselves late; keep the ring on its part.
  TOUR.timer = setInterval(placeTour, 300);
  tourStep(at);
}

function endTour() {
  clearInterval(TOUR.timer);
  if (TOUR.keys) document.removeEventListener('keydown', TOUR.keys, true);
  window.removeEventListener('resize', placeTour);
  if (TOUR.el) TOUR.el.remove();
  if (TOUR.spot) TOUR.spot.remove();
  Object.assign(TOUR, { i: -1, el: null, spot: null, timer: null, keys: null });
}

async function tourStep(i) {
  if (!TOUR.el || i < 0) return;
  if (i >= TOUR_STEPS.length) return endTour();
  TOUR.i = i;
  const s = TOUR_STEPS[i];
  const last = i === TOUR_STEPS.length - 1;
  closeModal();
  if (s.prep && s.view !== 'traffic') s.prep();
  if (s.view && (S.view !== s.view || s.view === 'settings')) go(s.view, true);
  if (s.prep && s.view === 'traffic') await Promise.resolve(s.prep()).catch(() => {});
  if (TOUR.i !== i || !TOUR.el) return;
  const dots = TOUR_STEPS.map((_, n) => h('span', { class: 'tdot' + (n === i ? ' on' : n < i ? ' done' : '') }));
  clear(
    TOUR.el,
    h('div', { class: 'tourhead' }, h('span', { class: 'tcount', text: i && !last ? `${i} of ${TOUR_STEPS.length - 2}` : 'Demo walkthrough' }), h('button', { class: 'iconbtn', title: 'End the walkthrough  (Esc)', text: '✕', onclick: endTour })),
    h('h4', { text: s.title }),
    h('p', { text: s.text }),
    s.action ? h('button', { class: 'linkbtn taction', text: s.action.label + ' →', onclick: s.action.run }) : null,
    h(
      'div',
      { class: 'tourfoot' },
      h('span', { class: 'tdots' }, dots),
      i === 0
        ? h('button', { class: 'btn sm', text: 'Not now', onclick: endTour })
        : last
          ? h('button', { class: 'btn sm', text: 'Start over', onclick: () => tourStep(1) })
          : h('button', { class: 'btn sm', text: 'Back', onclick: () => tourStep(i - 1) }),
      h('button', { class: 'btn sm primary', text: i === 0 ? 'Start the tour' : last ? 'Done' : 'Next', onclick: () => tourStep(i + 1) }),
    ),
  );
  TOUR.el.classList.remove('in');
  void TOUR.el.offsetWidth;
  TOUR.el.classList.add('in');
  placeTour();
}

/** The first of a step's targets that is on screen. */
function tourTarget(s) {
  for (const sel of [].concat(s.target || [])) {
    const el = $(sel);
    if (el && el.getClientRects().length) return el;
  }
  return null;
}

/** Rings the step's part of the screen and sets the card beside it: right,
 * left, below or above, whichever has room, else inside its lower corner. */
function placeTour() {
  if (!TOUR.el || TOUR.i < 0) return;
  const s = TOUR_STEPS[TOUR.i];
  const el = tourTarget(s);
  const vw = innerWidth;
  const vh = innerHeight;
  const cw = TOUR.el.offsetWidth;
  const ch = TOUR.el.offsetHeight;
  const gap = 14;
  const pad = 6;
  let x;
  let y;
  if (!el) {
    // A step about the whole app dims it; one whose part isn't on screen doesn't.
    TOUR.spot.hidden = !!s.target;
    TOUR.spot.classList.add('none');
    TOUR.el.classList.toggle('center', !s.target);
    if (!s.target) {
      x = (vw - cw) / 2;
      y = (vh - ch) / 2;
    } else {
      x = vw - cw - 24;
      y = vh - ch - 48;
    }
  } else {
    TOUR.el.classList.remove('center');
    TOUR.spot.classList.remove('none');
    // Keep the ring inside the window, even around a part that fills it.
    const r0 = el.getBoundingClientRect();
    const edge = pad + 3;
    const r = { left: Math.max(r0.left, edge), top: Math.max(r0.top, edge), right: Math.min(r0.right, vw - edge), bottom: Math.min(r0.bottom, vh - edge) };
    Object.assign(TOUR.spot.style, { left: r.left - pad + 'px', top: r.top - pad + 'px', width: r.right - r.left + pad * 2 + 'px', height: r.bottom - r.top + pad * 2 + 'px' });
    TOUR.spot.hidden = false;
    const clampY = (v) => Math.min(Math.max(v, 12), vh - ch - 12);
    const clampX = (v) => Math.min(Math.max(v, 12), vw - cw - 12);
    if (vw - r.right - pad >= cw + gap * 2) [x, y] = [r.right + pad + gap, clampY(r.top)];
    else if (r.left - pad >= cw + gap * 2) [x, y] = [r.left - pad - gap - cw, clampY(r.top)];
    else if (vh - r.bottom - pad >= ch + gap * 2) [x, y] = [clampX(r.left), r.bottom + pad + gap];
    else if (r.top - pad >= ch + gap * 2) [x, y] = [clampX(r.left), r.top - pad - gap - ch];
    else [x, y] = [clampX(r.right - cw - 20), clampY(r.bottom - ch - 20)];
  }
  TOUR.el.style.left = Math.round(x) + 'px';
  TOUR.el.style.top = Math.round(y) + 'px';
}

/* ---------- theme ---------- */

function applyTheme(t) {
  if (t === 'auto') document.documentElement.removeAttribute('data-theme');
  else document.documentElement.setAttribute('data-theme', t);
  S.theme = t;
}

/** Dense (the default) fits more on screen; Roomy gives rows and panels more air. */
function applyDensity(d) {
  document.documentElement.setAttribute('data-density', d === 'roomy' ? 'roomy' : 'dense');
  S.density = d === 'roomy' ? 'roomy' : 'dense';
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
  callbacks: { label: 'Callbacks', ico: '↩', render: renderCallbacks, tool: 'callbacks' },
  findings: { label: 'Findings', ico: '⚑', render: renderFindings },
  agents: { label: 'Agents', ico: '✦', render: renderAgents },
  market: { label: 'Market', ico: '⬢', render: renderMarket },
  scans: { label: 'Scans', ico: '⌖', render: renderScans },
  programs: { label: 'Programs', ico: '◈', render: renderPrograms },
  rules: { label: 'Rules', ico: '⇄', render: renderRules },
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
  // New notes and leads from Claude's watcher.
  const unread = st.agent_inbox_unread || 0;
  const ctAgents = $('#ct-agents');
  if (ctAgents) {
    ctAgents.textContent = unread || '';
    ctAgents.classList.toggle('hot', unread > 0);
    ctAgents.title = unread ? unread + ' new from Claude' : '';
  }
  $('#ct-bench').textContent = R.tabs.length || '';
  const ctCb = $('#ct-callbacks');
  if (ctCb) {
    const fresh = S.view === 'callbacks' ? 0 : Math.max(0, (st.callbacks || 0) - cbSeen());
    ctCb.textContent = fresh || '';
    ctCb.classList.toggle('hot', fresh > 0);
    ctCb.title = fresh ? fresh + (fresh === 1 ? ' new callback' : ' new callbacks') : '';
  }
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
    if (S.view === 'agents' && (Date.now() - (S.agentsAt || 0) > 4000 || st.agent_inbox_unread !== prev.agent_inbox_unread)) loadAgents();
    if (st.intercept && st.intercept.seq !== IC.seq) loadIntercept();
    if (st.callbacks !== prev.callbacks || (S.view === 'callbacks' && CB.data && CB.data.phase === 'starting')) callbacksChanged();
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

const T = { text: '', filters: [], items: [], total: 0, sel: null, live: true, maxId: 0, pretty: true, inspH: store('plonix.inspH'), picked: new Set(), pickAnchor: null, group: store('plonix.groupAlike') !== false, open: new Set(), visible: null };

const sameKey = (ex) => `${ex.method} ${ex.host}:${ex.port} ${target(ex)}`;

/** Runs of the same request sent one right after another, in list order.
 *  The same request again after something else starts a new run. */
function repeatRuns(items) {
  const runs = [];
  for (const ex of items) {
    const last = runs[runs.length - 1];
    if (last && sameKey(last[0]) === sameKey(ex)) last.push(ex);
    else runs.push([ex]);
  }
  return runs;
}
/** A run's id stays put while it grows: its oldest member does not change. */
const runId = (run) => Math.min(...run.map((x) => x.id));

function setGrouping(on) {
  T.group = on;
  T.open.clear();
  // Grouped is the default, so only turning it off is remembered.
  store('plonix.groupAlike', on ? null : false);
  drawGroupToggle();
  drawRows(Infinity);
}

/** "21:35:58–36:13" for a run (newest first in the list), or one time if they match. */
function timeSpan(run) {
  const ts = run.map((x) => x.ts);
  const a = fmtTime(Math.min(...ts)), b = fmtTime(Math.max(...ts));
  if (a === b) return a;
  // Drop the hour the end shares with the start: 21:35:58–36:13.
  return a + '–' + b.slice(a.slice(0, 3) === b.slice(0, 3) ? 3 : 0);
}

/** Grouped (repeats folded into one row each) or every request on its own row. */
function drawGroupToggle() {
  const box = $('#groupseg');
  if (!box) return;
  clear(
    box,
    h('button', { class: T.group ? 'on' : '', text: 'Grouped', title: 'Fold the same request sent several times in a row into one row. Click it to see each one.', onclick: () => setGrouping(true) }),
    h('button', { class: T.group ? '' : 'on', text: 'Every request', title: 'Show every request on its own row', onclick: () => setGrouping(false) }),
  );
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
  { key: 'time', label: 'Time', w: 112 },
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
    placeholder: 'Search any text, or start a filter: host, status, path, method…',
    spellcheck: 'false',
    autocomplete: 'off',
    oninput: () => {
      T.text = input.value;
      clearTimeout(T.qt);
      T.qt = setTimeout(() => {
        // Half-typed field values wait for Tab or Enter, so the list does not flash empty.
        if (qa.typingValue()) return;
        saveTrafficView();
        T.refresh(true);
      }, 220);
    },
    onkeydown: (e) => {
      if (qa.key(e)) return;
      if (e.key === 'Enter') {
        liftTyped();
        T.refresh(true);
      }
      if (e.key === 'Escape') input.blur();
    },
    onblur: () => liftTyped(),
  });
  const qa = queryAssist(input, {
    anchor: () => $('#searchbox') || input,
    apply: () => {
      liftTyped();
      T.refresh(true);
    },
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
        h('span', { class: 'seg groupseg', id: 'groupseg' }),
        h('span', { id: 'rulespill' }),
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
  drawGroupToggle();
  loadRules();
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
    // Labelled so one-click suggestions are not mistaken for active filters.
    sugg.length ? h('span', { class: 'chipslbl', text: 'Suggested' }) : null,
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

/* ---------- search autocomplete ----------
 * Suggests filter fields as the person types, then values for the field
 * from this project's own traffic, so nobody has to remember the syntax.
 * Tab completes the text, Enter applies, the arrows choose, Esc closes.
 */
const QA_FIELDS = [
  ['host', 'Host and its subdomains', ['domain', 'site', 'server']],
  ['status', 'Status code or class', ['code', 'response', 'error']],
  ['method', 'Request method', ['verb', 'get', 'post']],
  ['path', 'Path starts with', ['url', 'endpoint', 'route']],
  ['mime', 'Response content type', ['type', 'content', 'json', 'html']],
  ['ext', 'File extension', ['extension', 'file']],
  ['kind', 'Static files', ['static', 'assets']],
  ['scope', 'In scope or out of scope', ['target']],
  ['source', 'Captured, sent from Bench or imported', ['bench', 'replay', 'har', 'import']],
  ['is', 'Named filter from a filter pack', ['named', 'pack', 'filter']],
];
const QA_STATUS = { '1xx': 'Informational', '2xx': 'Success', '3xx': 'Redirects', '4xx': 'Client errors', '5xx': 'Server errors', none: 'No response' };
const QA_ID = /^(\d+|[0-9a-f]{8,}|[0-9a-f-]{32,36})$/i;

/** Values for a field with counts, from the traffic facets and the rows on screen. */
function qaValues(field) {
  const f = S.facets || {};
  const items = T.items || [];
  const out = new Map();
  const add = (value, count, note) => {
    if (value == null || value === '') return;
    const k = String(value).toLowerCase();
    const was = out.get(k);
    if (was) {
      if (!was.count && count) was.count = count;
      if (!was.note && note) was.note = note;
    } else out.set(k, { value: String(value), count: count || 0, note: note || '' });
  };
  const tally = (list) => {
    const m = new Map();
    for (const v of list) if (v) m.set(v, (m.get(v) || 0) + 1);
    return [...m].sort((a, b) => b[1] - a[1]);
  };
  switch (field) {
    case 'host':
      for (const c of f.hosts || []) add(c.value, c.count, 'in scope');
      for (const c of f.other_hosts || []) add(c.value, c.count, 'out of scope');
      for (const [v, n] of tally(items.map((i) => i.host))) add(v, n);
      break;
    case 'path': {
      for (const c of f.paths || []) add(c.value, c.count);
      // Prefixes of what is on screen, up to three segments, stopping at ids.
      const prefixes = [];
      for (const i of items) {
        const segs = (i.path || '').split('?')[0].split('/').filter(Boolean);
        let p = '';
        for (const s of segs.slice(0, 3)) {
          if (QA_ID.test(s) || s.length > 40) break;
          p += '/' + s;
          prefixes.push(p);
        }
      }
      for (const [v, n] of tally(prefixes)) add(v, n);
      break;
    }
    case 'method':
      for (const c of f.methods || []) add(c.value, c.count);
      for (const m of ['GET', 'POST', 'PUT', 'PATCH', 'DELETE', 'OPTIONS', 'HEAD']) add(m);
      break;
    case 'status':
      for (const c of (f.statuses || []).filter((c) => c.value !== 'other')) add(c.value, c.count, QA_STATUS[c.value]);
      for (const [v, n] of tally(items.map((i) => (i.status == null ? null : String(i.status))))) add(v, n);
      for (const k of ['2xx', '3xx', '4xx', '5xx', 'none']) add(k, 0, QA_STATUS[k]);
      break;
    case 'mime':
      for (const c of f.kinds || []) add(c.value, c.count);
      for (const k of ['json', 'html', 'javascript', 'xml', 'css', 'image', 'font']) add(k);
      break;
    case 'ext':
      for (const [v, n] of tally(items.map((i) => ((i.path || '').split('?')[0].match(/\.([a-z0-9]{1,6})$/i) || [])[1]?.toLowerCase()))) add(v, n);
      for (const x of ['js', 'css', 'json', 'html', 'png', 'svg', 'woff2', 'map', 'php']) add(x);
      break;
    case 'kind':
      add('static', 0, 'images, fonts, styles, scripts and media');
      break;
    case 'scope':
      add('in', f.in_scope, 'in scope');
      add('out', f.out_of_scope, 'out of scope');
      break;
    case 'source':
      add('proxy', 0, 'captured');
      add('replay', f.replays, 'sent from Bench');
      add('import', 0, 'imported from HAR');
      break;
    case 'is':
      for (const n of S.named) add(n.id, 0, n.label);
      break;
  }
  return [...out.values()];
}

/** Values that start with `partial` first, then values that contain it. */
function qaMatch(list, partial, skip = []) {
  const p = partial.toLowerCase();
  const seen = new Set(skip.map((s) => s.toLowerCase()));
  const ranked = [];
  list.forEach((v, i) => {
    const s = v.value.toLowerCase();
    if (seen.has(s)) return;
    const note = (v.note || '').toLowerCase();
    const at = p ? s.indexOf(p) : 0;
    const rank = !p ? 1 : s === p ? 0 : at === 0 ? 1 : at > 0 ? 2 : note.includes(p) ? 3 : -1;
    if (rank >= 0) ranked.push({ ...v, rank, i });
  });
  return ranked.sort((a, b) => a.rank - b.rank || a.i - b.i);
}

/** The whitespace separated term the caret is in. */
function qaToken(input) {
  const v = input.value;
  const caret = input.selectionStart ?? v.length;
  const before = v.slice(0, caret);
  // Inside a quoted phrase: that is text, nothing to suggest.
  if ((before.match(/"/g) || []).length % 2) return null;
  const start = before.search(/\S*$/);
  const after = v.slice(caret).search(/\s|$/);
  return { start, end: caret + after, text: v.slice(start, caret + after) };
}

/**
 * Attaches the suggestion list to a search input.
 * opts.field: a function naming the one field being typed (values only),
 *   otherwise the input takes whole query terms.
 * opts.apply(text): called after a value is picked with Enter or a click.
 */
function queryAssist(input, opts = {}) {
  const menu = h('div', { class: 'qsugg', role: 'listbox', id: 'qsugg-' + Math.random().toString(36).slice(2, 8) });
  const st = { items: [], active: -1, tok: null, mode: 'key' };
  input.setAttribute('role', 'combobox');
  input.setAttribute('aria-autocomplete', 'list');
  input.setAttribute('aria-expanded', 'false');
  input.setAttribute('aria-controls', menu.id);

  const build = () => {
    if (opts.field) {
      // One field's value; a comma list completes its last value.
      const field = opts.field();
      const list = input.value.split(',');
      const partial = list.pop().trim();
      const prefix = list.length ? list.join(',') + ',' : '';
      st.tok = { start: 0, end: input.value.length };
      st.mode = 'value';
      return qaMatch(qaValues(field), partial, list.map((x) => x.trim()))
        .slice(0, 8)
        .map((v) => ({ kind: 'value', key: '', shown: v.value, partial, note: v.note, count: v.count, text: prefix + v.value }));
    }
    const tok = qaToken(input);
    st.tok = tok;
    if (!tok) return [];
    const neg = tok.text.startsWith('-') ? '-' : '';
    const body = tok.text.slice(neg.length);
    const colon = body.indexOf(':');
    if (colon > 0) {
      const field = body.slice(0, colon).toLowerCase();
      if (!QA_FIELDS.some(([k]) => k === field)) return [];
      const list = body.slice(colon + 1).replace(/^"|"$/g, '').split(',');
      const partial = list.pop().trim();
      const done = list.map((x) => x.trim()).filter(Boolean);
      st.mode = 'value';
      return qaMatch(qaValues(field), partial, done)
        .slice(0, 8)
        .map((v) => ({ kind: 'value', key: neg + field + ':', shown: v.value, partial, note: v.note, count: v.count, text: neg + field + ':' + [...done, v.value].join(',') }));
    }
    st.mode = 'key';
    const p = body.toLowerCase();
    const keys = QA_FIELDS.filter(([k, , alias]) => !p || k.startsWith(p) || alias.some((a) => a.startsWith(p)) || FILTER_FIELDS[k].toLowerCase().startsWith(p)).map(([k, hint]) => ({
      kind: 'key',
      key: '',
      shown: neg + k + ':',
      partial: neg + p,
      note: hint,
      text: neg + k + ':',
    }));
    // Two letters in: values anywhere that contain them, as whole filters.
    const vals = [];
    if (p.length >= 2) {
      for (const field of ['host', 'path', 'status', 'method', 'mime', 'is']) {
        for (const v of qaMatch(qaValues(field), p).filter((v) => v.rank < 3).slice(0, field === 'host' || field === 'path' ? 3 : 2)) {
          vals.push({ kind: 'value', key: neg + field + ':', shown: v.value, partial: p, note: v.note, count: v.count, text: neg + field + ':' + v.value, rank: v.rank });
        }
      }
      vals.sort((a, b) => a.rank - b.rank);
    }
    return [...keys.slice(0, p ? 4 : keys.length), ...vals.slice(0, 6)];
  };

  const close = () => {
    menu.remove();
    st.items = [];
    st.active = -1;
    input.setAttribute('aria-expanded', 'false');
    input.removeAttribute('aria-activedescendant');
  };

  const mark = (text, partial) => {
    const p = (partial || '').replace(/^-/, '').toLowerCase();
    const at = p ? text.toLowerCase().indexOf(p) : -1;
    if (at < 0) return text;
    return [text.slice(0, at), h('b', { text: text.slice(at, at + p.length) }), text.slice(at + p.length)];
  };

  const draw = () => {
    const rows = [];
    let section = null;
    st.items.forEach((it, i) => {
      const sec = it.kind === 'key' ? 'Filter by' : st.mode === 'value' ? 'Values in this project' : 'Matching filters';
      if (sec !== section) rows.push(h('div', { class: 'qmh', text: (section = sec) }));
      rows.push(
        h(
          'div',
          {
            class: 'qrow' + (i === st.active ? ' on' : ''),
            role: 'option',
            id: menu.id + '-' + i,
            'aria-selected': i === st.active ? 'true' : 'false',
            onmousedown: (e) => {
              e.preventDefault();
              e.stopPropagation();
              pick(it, 'enter');
            },
            onmousemove: () => {
              if (st.active === i) return;
              st.active = i;
              draw();
            },
          },
          h('span', { class: 'qterm' }, it.key ? h('span', { class: 'qk', text: it.key }) : null, h('span', { class: 'qv' }, mark(it.shown, it.partial))),
          h('span', { class: 'qnote', text: it.note || '' }),
          it.count ? h('span', { class: 'qn', text: it.count }) : null,
        ),
      );
    });
    const hint = (k, what) => h('span', { class: 'qhint' }, h('kbd', { text: k }), what);
    clear(
      menu,
      h('div', { class: 'qlist' }, rows),
      h(
        'div',
        { class: 'qfoot' },
        hint('↑↓', 'choose'),
        hint('Tab', 'complete'),
        hint('↵', 'apply'),
        hint('Esc', 'close'),
        st.mode === 'key' && !opts.field ? h('span', { class: 'qtip', text: 'Start with − to hide' }) : null,
      ),
    );
    if (!menu.isConnected) document.body.append(menu);
    const box = (opts.anchor ? opts.anchor() : input).getBoundingClientRect();
    menu.style.left = box.left + 'px';
    menu.style.top = box.bottom + 4 + 'px';
    menu.style.width = Math.max(opts.field ? box.width : Math.min(box.width, 520), opts.field ? 380 : 300) + 'px';
    input.setAttribute('aria-expanded', 'true');
    if (st.active >= 0) {
      input.setAttribute('aria-activedescendant', menu.id + '-' + st.active);
      $('#' + menu.id + '-' + st.active, menu)?.scrollIntoView({ block: 'nearest' });
    } else input.removeAttribute('aria-activedescendant');
  };

  const update = () => {
    if (document.activeElement !== input) return close();
    st.items = build();
    // Typing a field's value: the best match is ready for Enter.
    st.active = st.items.length && st.mode === 'value' && st.items[0].kind === 'value' ? 0 : -1;
    if (!st.items.length) return close();
    draw();
  };

  const pick = (it, how) => {
    const v = input.value;
    const { start, end } = st.tok;
    const tail = v.slice(end);
    const done = it.kind === 'value';
    const insert = it.text + (done && !opts.field && !/^\s/.test(tail) ? ' ' : '');
    input.value = v.slice(0, start) + insert + tail;
    const caret = start + insert.length;
    input.setSelectionRange(caret, caret);
    input.dispatchEvent(new Event('input', { bubbles: true }));
    if (done && how === 'enter' && opts.apply) {
      close();
      opts.apply(input.value);
      return;
    }
    if (done) close();
    else update();
  };

  // Clicks on headings or the key hints keep the caret in the box.
  menu.addEventListener('mousedown', (e) => e.preventDefault());
  input.addEventListener('input', update);
  input.addEventListener('focus', update);
  input.addEventListener('click', update);
  input.addEventListener('blur', close);

  return {
    close,
    update,
    /** True while a field's value is being typed with suggestions showing. */
    typingValue: () => menu.isConnected && st.mode === 'value',
    /** Handles a keydown; true when the list used it. */
    key(e) {
      if (!st.items.length || !menu.isConnected) return false;
      const n = st.items.length;
      if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
        st.active = e.key === 'ArrowDown' ? (st.active + 1) % n : (st.active <= 0 ? n : st.active) - 1;
        draw();
      } else if (e.key === 'Tab' && !e.shiftKey) {
        pick(st.items[Math.max(st.active, 0)], 'tab');
      } else if (e.key === 'Enter' && st.active >= 0) {
        pick(st.items[st.active], 'enter');
      } else if (e.key === 'Escape') {
        close();
      } else return false;
      e.preventDefault();
      e.stopPropagation();
      return true;
    },
  };
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
  const value = h('input', { placeholder: 'value', spellcheck: 'false', autocomplete: 'off', value: preset.value || '' });
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
  // Values for the chosen field from this project's traffic; free text has none.
  const qa = queryAssist(value, { field: () => (field.value === 'text' ? '' : field.value), apply: () => add() });
  value.addEventListener('keydown', (e) => !qa.key(e) && e.key === 'Enter' && add());
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
    h('div', { class: 'prow' }, field, value),
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
  for (const p of document.querySelectorAll('.popover, .ctxmenu, .qsugg')) p.remove();
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
  const idT = idTargetOf(ex);
  const actions = [{ label: 'Send to Bench', run: () => sendToBench(ex.id) }, { label: 'Copy as curl', run: () => copyCurl(ex.id) }];
  if (decide(ex.host) === 'accepted' && toolOn('access-check')) {
    if (idT) actions.push({ label: 'Check this id across users', run: () => startAccessCheck({ targets: [ex.id], sourceLabel: `${ex.method} ${ex.path}` }) });
    actions.push({ label: 'Replay signed out', run: () => startAccessCheck({ targets: [ex.id], sourceLabel: `${ex.method} ${ex.path}`, onlyAnon: true }) });
  }
  if (decide(ex.host) === 'accepted') {
    const leads = scanLeadsFor(ex);
    actions.push({ label: leads.length ? leads[0].chip : 'Scan this endpoint', run: () => scanEndpoint(ex, leads[0] || null) });
    for (const { d, cap } of detectorLeadsSync(ex)) {
      actions.push({ label: fillTemplate(d.suggest.chip, cap), run: () => runDetectorLead(ex, d, cap) });
    }
  }
  const groups = [
    actions,
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
  const shown = [];
  if (T.group) {
    for (const run of repeatRuns(T.items)) {
      if (run.length < 2) shown.push({ ex: run[0] });
      else {
        // An open run lists the rest of its requests right under its first row.
        shown.push({ ex: run[0], group: run });
        if (T.open.has(runId(run))) for (const m of run.slice(1)) shown.push({ ex: m, member: true });
      }
    }
  } else for (const ex of T.items) shown.push({ ex });
  T.visible = T.group ? shown.map((r) => r.ex.id) : null;
  const rows = shown.map(({ ex, group, member }) => {
    const tags = [];
    const k = group && runId(group);
    const isOpen = group && T.open.has(k);
    if (group) {
      tags.push(
        h('button', {
          class: 'tag alike' + (isOpen ? ' on' : ''),
          text: (isOpen ? '▾ ' : '▸ ') + '×' + group.length,
          title: `Sent ${group.length} times in a row. Click to ${isOpen ? 'fold them' : 'show each one'}.`,
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
  if (ex.replaced) tags.push(h('span', { class: 'tag edited', text: 'changed', title: 'Changed by rules on the way' }));
    const tr = h(
      'tr',
      {
        'data-id': ex.id,
        class: [ex.id === T.sel ? 'sel' : '', T.picked.has(ex.id) ? 'picked' : '', ex.in_scope ? '' : 'out', ex.id > freshAbove ? 'fresh' : '', member ? 'member' : ''].join(' ').trim(),
        onclick: (e) => {
          if (e.metaKey || e.ctrlKey || e.shiftKey) return pickRow(ex.id, e.shiftKey);
          // Clicking a folded group opens it, so its members show right under it.
          if (group && !isOpen) {
            T.open.add(k);
            drawRows(Infinity);
          }
          openInspector(ex.id);
        },
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
      h('td', { class: 'num', text: group && !isOpen ? timeSpan(group) : fmtTime(ex.ts), title: group && !isOpen ? 'First to last: ' + fmtTime(Math.min(...group.map((x) => x.ts))) + ' – ' + fmtTime(Math.max(...group.map((x) => x.ts))) : null }),
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
      h('div', { class: 'selslot' }),
      rawPre(responseText(ex, T.pretty)),
      msgBox,
    );
    wireLensSelection(respCol, ex, 'response');
    if (ex.insights) drawInsights(respCol, ex.insights.filter((i) => i.side === 'response'));
  };
  drawResp();
  if (msgBox) drawMessages(msgBox, ex);
  // Changed by rules on the way out: the Lens can show it as sent, as it was, or both.
  const ruled = !ex.edited && ex.original_request && ex.replaced && ex.replaced.length;
  const reqBody = h('div', { class: 'reqbody' });
  const drawReq = () => {
    const view = ruled ? T.ruleView || 'sent' : 'sent';
    clear(reqBody, view === 'sent' ? rawPre(requestText(ex)) : ruleCompare(ex, view));
    if (ruleSeg) for (const b of ruleSeg.children) b.classList.toggle('on', b.dataset.v === view);
  };
  const ruleSeg = ruled
    ? h(
        'span',
        { class: 'seg r', title: 'Rules changed this request on its way out' },
        [['sent', 'As sent'], ['original', 'Original'], ['both', 'Both']].map(([v, l]) => h('button', { 'data-v': v, text: l, onclick: () => ((T.ruleView = v), drawReq()) })),
      )
    : null;
  const reqCol = h(
    'div',
    { class: 'col' },
    h('div', { class: 'lbl' }, 'Request', cutTag(ex.req_truncated, ex.req_size, ex.req_body), ruleSeg, h('span', { class: 'r', text: '#' + ex.id + ' · ' + fmtTime(ex.ts) + ' · ' + (ex.initiator || ex.source || 'proxy') })),
    h('div', { class: 'spotslot' }),
    h('div', { class: 'selslot' }),
    reqBody,
  );
  drawReq();
  reqCol.addEventListener('contextmenu', (e) => {
    const pre = e.target.closest('pre.raw');
    if (pre && !pre.classList.contains('rlcmp')) lensHeaderMenu(e, pre, 'request');
  });
  respCol.addEventListener('contextmenu', (e) => {
    const pre = e.target.closest('pre.raw');
    if (pre) lensHeaderMenu(e, pre, 'response');
  });
  wireLensSelection(reqCol, ex, 'request');
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
        ex.replaced && ex.replaced.length ? h('button', { class: 'tag edited', text: 'changed', title: 'Changed by rules:\n' + ex.replaced.join('\n') + '\n\nClick to open Rules', onclick: () => leaveTo('rules') }) : null,
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

/* ---------- decode a selection in the Lens ----------
   Select any text in a request or response and a small bar offers to decode
   it (JWT, URL-encoding, Base64, hex), find it in traffic or ask Claude about
   it. Read-only: the Lens shows what was captured. */
function wireLensSelection(col, ex, side) {
  const bar = h('div', { class: 'qbar lensqbar' });
  col.appendChild(bar);
  const picked = () => {
    const sel = window.getSelection();
    if (!sel || sel.isCollapsed || !sel.rangeCount) return '';
    const pre = col.querySelector('pre.raw');
    return pre && pre.contains(sel.anchorNode) && pre.contains(sel.focusNode) ? sel.toString() : '';
  };
  const show = () => {
    const t = picked();
    if (!t.trim()) return bar.classList.remove('show');
    clear(
      bar,
      h('button', { text: '◇ Decode', onmousedown: (e) => e.preventDefault(), onclick: () => (bar.classList.remove('show'), decodeSelection(col, t)) }),
      t.trim().length >= 4 ? h('button', { text: 'Find in traffic', onmousedown: (e) => e.preventDefault(), onclick: () => setQuery('"' + t.trim().replace(/"/g, '').slice(0, 120) + '"') }) : null,
      agentsOn() ? h('span', { class: 'qsep' }) : null,
      agentsOn()
        ? h('button', { class: 'ai', onmousedown: (e) => e.preventDefault(), onclick: () => askClaude({ kind: 'request', id: ex.id }, { question: `In the ${side} of this request, what is this value and is it worth testing?\n\n${t.slice(0, 600)}` }) }, '✦ Ask Claude')
        : null,
    );
    bar.classList.add('show');
  };
  col.addEventListener('mouseup', () => setTimeout(show, 0));
  col.addEventListener('keyup', (e) => e.shiftKey && setTimeout(show, 0));
}
document.addEventListener('selectionchange', () => {
  const sel = window.getSelection();
  if (!sel || sel.isCollapsed) for (const b of document.querySelectorAll('.lensqbar.show')) b.classList.remove('show');
});

function decodeSelection(col, t) {
  const slot = col.querySelector('.selslot');
  if (!slot) return;
  const it = genericDecode(t);
  const close = h('button', { class: 'blx', text: '✕', title: 'Close', onclick: () => clear(slot) });
  const head = h('div', { class: 'sdh' }, h('b', { text: it.kind === 'text' ? 'Selection' : it.label }), h('span', { class: 'muted', text: ' from selection' }), h('span', { class: 'sdact' }), close);
  const notes = it.notes && it.notes.length ? h('div', { class: 'sdnotes' }, it.notes.map((n) => h('span', { class: 'sdnote' + (/^(unsigned|expired)/.test(n) ? ' warn' : ''), text: n }))) : null;
  const block = (label, text) => [h('div', { class: 'sdk' }, label), h('pre', { class: 'sdv', text })];
  const body =
    it.kind === 'jwt'
      ? [block('Claims', JSON.stringify(it.jwt.payload, null, 2)), block('Header', JSON.stringify(it.jwt.header, null, 2))]
      : it.kind === 'text'
        ? h('div', { class: 'dnote blnote', text: 'This does not look encoded (JWT, URL, Base64 or hex).' })
        : block('Decoded', bl_pretty(it.decode()));
  clear(slot, h('div', { class: 'spotdetail' }, head, notes, body));
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

/* ======================================================================
   Mind Reader — context-aware quick actions
   Each reader looks at one exchange and works out the single most useful
   next move for what the researcher is looking at, or returns nothing. The
   chips they drive only SUGGEST: nothing is sent until a click, and anything
   that sends is scope-gated by the engine. Surfaced in the Lens "Suggested"
   row and the Traffic row menu.
   ====================================================================== */

const REDIRECT_PARAMS = /^(url|uri|next|returnurl|return_to|return|redirect_uri|redirect_url|redirect|dest|destination|continue|goto|forward|callback|rurl)$/i;
const LOGIN_PATH = /log-?in|sign-?in|sign-?on|\/auth|session|token|oauth|sso/i;
const SESSION_COOKIE = /^(sess|sid|session|auth|token|jwt|connect\.sid|jsessionid|phpsessid|asp\.net|_session)/i;

/** The name=value pairs of a request's query string. */
function queryPairsOf(ex) {
  const out = [];
  const q = ex.query || (ex.url && ex.url.includes('?') ? ex.url.split('?')[1] : '') || '';
  for (const part of q.split('&')) {
    if (!part) continue;
    const i = part.indexOf('=');
    const dec = (s) => { try { return decodeURIComponent(s.replace(/\+/g, ' ')); } catch (_) { return s; } };
    out.push([dec(i < 0 ? part : part.slice(0, i)), i < 0 ? '' : dec(part.slice(i + 1))]);
  }
  return out;
}

/** An id-shaped value in the path or query — the "could I read someone else's?" smell. */
function idTargetOf(ex) {
  for (const s of (ex.path || '').split('/').filter(Boolean)) {
    if (/^\d{1,15}$/.test(s)) return { kind: 'number', value: s, where: 'the path' };
    if (/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(s)) return { kind: 'uuid', value: s, where: 'the path' };
  }
  for (const [k, v] of queryPairsOf(ex)) {
    if (/(^id$|_id$|^uid$|^uuid$|guid)/i.test(k) && v) return { kind: 'param', value: v, where: `the "${k}" parameter`, name: k };
  }
  return null;
}

/** Request values that come straight back in an HTML/text response, verbatim. */
function reflectedValues(ex) {
  const body = ex.resp_text;
  if (!body || body.length > 2_000_000) return [];
  if (!/html|xml|text\/plain/i.test(header(ex.resp_headers, 'content-type') || '')) return [];
  const hits = [];
  const seen = new Set();
  const consider = (name, value, where) => {
    const v = (value || '').trim();
    if (v.length < 5 || v.length > 200 || seen.has(v)) return;
    if (/^[\d.\s,-]+$/.test(v)) return; // bare numbers reflect everywhere
    if (body.includes(v)) { hits.push({ name, value: v, where }); seen.add(v); }
  };
  for (const [k, v] of queryPairsOf(ex)) consider(k, v, 'query');
  const rb = ex.req_text || '';
  if (rb.length < 100_000 && /[=&]/.test(rb) && !/^[[{]/.test(rb.trim())) {
    for (const part of rb.split('&')) {
      const i = part.indexOf('=');
      if (i <= 0) continue;
      const dec = (s) => { try { return decodeURIComponent(s.replace(/\+/g, ' ')); } catch (_) { return s; } };
      consider(dec(part.slice(0, i)), dec(part.slice(i + 1)), 'body');
    }
  }
  return hits.slice(0, 3);
}

/** A permissive cross-origin policy on the response. */
function corsIssue(ex) {
  const acao = (header(ex.resp_headers, 'access-control-allow-origin') || '').trim();
  if (!acao) return null;
  const creds = /true/i.test(header(ex.resp_headers, 'access-control-allow-credentials') || '');
  const origin = (header(ex.req_headers, 'origin') || '').trim();
  if (acao === '*' && creds) return { severity: 'medium', note: 'The response sets Access-Control-Allow-Origin to * while allowing credentials, so any site could read it on behalf of a signed-in user.' };
  if (origin && acao === origin && creds) return { severity: 'medium', note: `The response echoes the request Origin (${origin}) into Access-Control-Allow-Origin with credentials allowed, so an attacker-chosen origin may be trusted.` };
  if (acao === '*') return { severity: 'low', note: 'The response sets Access-Control-Allow-Origin to *, so any site can read it.' };
  return null;
}

/** A parameter that carries a URL or path the server might follow. */
function redirectParam(ex) {
  for (const [k, v] of queryPairsOf(ex)) {
    if (REDIRECT_PARAMS.test(k) && /^(https?:\/\/|\/\/|\/)[^\s]/i.test(v)) return { name: k, value: v };
  }
  return null;
}

/** Whether this request looks like GraphQL. */
function isGraphql(ex) {
  if (/\/graphql\b|\/gql\b/i.test(ex.path || '')) return true;
  if (/application\/graphql/i.test(header(ex.req_headers, 'content-type') || '')) return true;
  const rb = ex.req_text || '';
  return /"query"\s*:/.test(rb) && /\b(query|mutation|subscription)\b/.test(rb);
}

/** A login/session handed back in a response — the makings of a saved user. */
function sessionGrant(ex) {
  const sc = (ex.resp_headers || []).filter(([k]) => /^set-cookie$/i.test(k)).map(([, v]) => v);
  if (!sc.length) return null;
  const sessiony = sc.some((v) => SESSION_COOKIE.test(v));
  if (!LOGIN_PATH.test(ex.path || '') && !sessiony) return null;
  const pairs = sc.map((v) => v.split(';')[0].trim()).filter(Boolean);
  if (!pairs.length) return null;
  return { cookie: pairs.join('; '), count: pairs.length };
}

/** A friendly default name for a user captured from a request. */
function suggestUserName(ex) {
  for (const [k, v] of queryPairsOf(ex)) if (/^(user|username|login|email|account|name)$/i.test(k) && v) return v.split('@')[0].slice(0, 40);
  const rb = ex.req_text || '';
  const m = rb.match(/"?(user(name)?|email|login)"?\s*[:=]\s*"?([^"&,}\s]{2,40})/i);
  if (m) return m[3].split('@')[0];
  return 'User from ' + (ex.host || 'capture');
}

/** Opens the Saved users sheet with a new user pre-filled from a captured login. */
function saveUserFrom(ex) {
  const g = sessionGrant(ex);
  if (!g) return toast('No session cookie found on this response.', 'err');
  manageUsers(() => toast('Saved — pick this user in the Bench “As…” menu.', 'ok'), { name: suggestUserName(ex), note: `Captured from ${ex.method} ${ex.path}`, headers: [['Cookie', g.cookie]] });
}

const IDEAS_QUESTION =
  'Suggest up to three things worth trying next on this endpoint. For each, say in plain words what to change, what result would mean there is a problem, and give the exact request to send from the Plonix Bench. Start with the most promising one.';

/** A quick look at whether a response is an API description (OpenAPI or Swagger). */
function looksLikeApiSpec(ex) {
  const t = ex.resp_text;
  return !!t && t.length < 8_000_000 && /"(openapi|swagger)"\s*:/.test(t.slice(0, 4000)) && /"paths"\s*:/.test(t);
}

/* ---------- Leads into the Scans tab ----------
   Some endpoint shapes have a natural follow-up that lives in Scans: a file
   upload, an endpoint that takes inputs. Rather than "scan the whole host",
   these leads hand exactly the one endpoint to Scans with the fitting checks
   pre-picked, so the researcher reviews and runs — the human decides, nothing
   fires on its own. Each lead is a label plus the OWASP categories to focus;
   coverage stays at the category level, concrete checks come from the Market. */

const UPLOAD_PATH = /\/(upload|uploads|file|files|attachment|attachments|media|document|documents|import|avatar|avatars|photo|photos|image|images)(?:\/|$|\?)/i;

/** An endpoint that takes a file — the "what does it accept, and what happens then?" smell. */
function uploadTarget(ex) {
  if (!/^(POST|PUT|PATCH)$/i.test(ex.method || '')) return null;
  const ct = (header(ex.req_headers, 'content-type') || '').toLowerCase();
  if (ct.includes('multipart/form-data')) return { how: 'sends a multipart form, the usual shape of a file upload' };
  if (/\bfilename\s*=/.test(ex.req_text || '')) return { how: 'carries a filename in the body' };
  if (ct.includes('application/octet-stream')) return { how: 'posts a raw file body' };
  if (UPLOAD_PATH.test(ex.path || '')) return { how: 'is a write to an upload-shaped path' };
  return null;
}

/** Whether a request carries inputs worth checking how the server handles. */
function hasInputs(ex) {
  if (queryPairsOf(ex).length) return true;
  const rb = ex.req_text || '';
  return rb.length < 200_000 && /[^&=]=[^&=]/.test(rb) && !/^[[{]/.test(rb.trim());
}

/** Does a value look like a hostname, URL or IP the server might fetch? */
function looksLikeHostOrUrl(v) {
  const s = (v || '').trim();
  if (s.length < 4 || s.length > 2048 || /\s|@/.test(s)) return null;
  if (/^(https?:)?\/\/[^/\s]/i.test(s)) return 'a URL';
  if (/^\d{1,3}(\.\d{1,3}){3}(:\d+)?(\/|$)/.test(s)) return 'an IP address';
  // A bare hostname: dotted, ending in an alphabetic label, not a file name.
  if (/^[a-z0-9][a-z0-9.-]*\.[a-z]{2,}(:\d+)?(\/|$)/i.test(s) && !/\.(js|css|png|jpe?g|gif|svg|webp|woff2?|map|json|xml|txt|ico)$/i.test(s)) return 'a hostname';
  return null;
}

/** An input whose value points at another server — the "does it fetch this?" smell. */
function ssrfTarget(ex) {
  for (const [k, v] of queryPairsOf(ex)) {
    const what = looksLikeHostOrUrl(v);
    if (what) return { name: k, where: `the "${k}" parameter`, what, value: v };
  }
  const rb = ex.req_text || '';
  if (rb.length < 100_000 && /[=&]/.test(rb) && !/^[[{]/.test(rb.trim())) {
    for (const part of rb.split('&')) {
      const i = part.indexOf('=');
      if (i <= 0) continue;
      const dec = (s) => { try { return decodeURIComponent(s.replace(/\+/g, ' ')); } catch (_) { return s; } };
      const k = dec(part.slice(0, i));
      const what = looksLikeHostOrUrl(dec(part.slice(i + 1)));
      if (what) return { name: k, where: `the "${k}" field`, what, value: dec(part.slice(i + 1)) };
    }
  }
  return null;
}

/**
 * The single most specific follow-up for this endpoint that belongs in Scans,
 * as a one-item list (or none). Self-describing so the chip and the Scans focus
 * banner can explain exactly what will run. `categories` are OWASP ids to
 * pre-pick; an empty array pre-picks none. Order: the most specific wins.
 */
function scanLeadsFor(ex) {
  if (!ex || decide(ex.host) !== 'accepted') return [];
  const where = `${ex.method} ${ex.path}`;
  const up = uploadTarget(ex);
  if (up) {
    return [{
      kind: 'upload',
      chip: 'Check this upload in Scans',
      focus: 'file handling',
      categories: [],
      title: `an upload endpoint (${where})`,
      why: `This ${up.how}. Open it in Scans to check how the upload is handled — allowed types, where files land, what the server does with them.`,
      note: 'Plonix has no built-in file-handling check yet, so nothing is pre-picked here. Add a file-handling check from the Market, or experiment with the upload field on the Bench.',
    }];
  }
  const ssrf = ssrfTarget(ex);
  if (ssrf) {
    return [{
      kind: 'ssrf',
      chip: 'Scan this input for SSRF',
      focus: 'server-side requests (SSRF)',
      categories: ['A10'],
      title: `server-side requests on ${where}`,
      why: `${ssrf.where} carries ${ssrf.what} ("${ssrf.value.slice(0, 40)}") that the server may fetch. Open it in Scans to check whether it can be pointed at somewhere it shouldn't reach.`,
      note: 'Plonix has no built-in SSRF check yet, so nothing is pre-picked here. Add one from the Market, or change the value on the Bench and watch where the request goes.',
    }];
  }
  if (hasInputs(ex)) {
    return [{
      kind: 'inputs',
      chip: "Scan this endpoint's inputs",
      focus: 'input handling',
      categories: ['A03'],
      title: `input handling on ${where}`,
      why: 'This endpoint takes inputs. Open it in Scans to run the input-handling checks that fit, scoped to just this endpoint.',
      note: '',
    }];
  }
  return [];
}

/* ---------- Detector packs (Mind Reader suggestions as data) ----------
   A detector is declarative: it recognizes a shape in the exchange and offers
   a chip that hands off to another tab — an upload to check in Scans, a token
   to tweak on the Bench, a cookie to write up as a finding. The engine only
   serves the definitions; the matching runs here, next to the chips. A
   detector can't run code or send anything: a match produces one suggestion
   chip, and only for a host already in scope. Community packs plug in the same
   way, so these grow without an app release. */

let DETECTORS = null;
let DETECTORS_PENDING = null;
/** Loads the detectors in effect once, then serves them from memory. */
async function loadDetectors() {
  if (DETECTORS) return DETECTORS;
  if (!DETECTORS_PENDING) {
    DETECTORS_PENDING = api('/api/detectors')
      .then((d) => ((DETECTORS = (d && d.detectors) || []), DETECTORS))
      .catch(() => ((DETECTORS = []), DETECTORS));
  }
  return DETECTORS_PENDING;
}

const DET_RE_CACHE = new Map();
/** A case-insensitive RegExp for a pattern, compiled once; null if invalid. */
function detRe(p) {
  if (DET_RE_CACHE.has(p)) return DET_RE_CACHE.get(p);
  let re = null;
  try {
    re = new RegExp(p, 'i');
  } catch (_) {
    re = null;
  }
  DET_RE_CACHE.set(p, re);
  return re;
}

/** Does a value look like a file path or name (and not a host or URL)? */
function looksLikePathOrFile(v) {
  const s = (v || '').trim();
  if (s.length < 2 || s.length > 2048 || /\s/.test(s)) return null;
  if (looksLikeHostOrUrl(s)) return null;
  if (/(^|[/\\])\.\.([/\\]|$)/.test(s)) return 'a path that climbs directories';
  if (/[/\\]/.test(s)) return 'a file path';
  if (/^[\w.-]+\.[a-z0-9]{1,8}$/i.test(s)) return 'a file name';
  return null;
}

/** Does a value look like a JWT (three base64url segments, the middle a claims set)? */
function looksLikeJwt(v) {
  return /eyj[a-z0-9_-]+\.eyj[a-z0-9_-]+\.[a-z0-9_-]+/i.test((v || '').trim()) ? 'a token' : null;
}

const DET_VALUE_CLASS = { host_or_url: looksLikeHostOrUrl, path_or_file: looksLikePathOrFile, jwt: looksLikeJwt };

/** The request body as key/value pairs, when it is a urlencoded form. */
function formPairsOf(ex) {
  const rb = ex.req_text || '';
  if (rb.length > 200_000 || !/[=&]/.test(rb) || /^[[{]/.test(rb.trim())) return [];
  const dec = (s) => {
    try {
      return decodeURIComponent(s.replace(/\+/g, ' '));
    } catch (_) {
      return s;
    }
  };
  const out = [];
  for (const part of rb.split('&')) {
    const i = part.indexOf('=');
    if (i <= 0) continue;
    out.push([dec(part.slice(0, i)), dec(part.slice(i + 1))]);
  }
  return out;
}

/** Whether a header condition holds. The header must exist; then any sub-test given must pass. */
function headerCondMatch(headers, c) {
  const vals = (headers || []).filter(([k]) => (k || '').toLowerCase() === c.name.toLowerCase()).map(([, v]) => v || '');
  if (!vals.length) return false;
  if (c.contains && !vals.some((v) => v.toLowerCase().includes(c.contains.toLowerCase()))) return false;
  if (c.regex) {
    const re = detRe(c.regex);
    if (!re || !vals.some((v) => re.test(v))) return false;
  }
  if (c.absent_regex) {
    const re = detRe(c.absent_regex);
    // The header is present, but at least one value lacks the pattern
    // (e.g. one Set-Cookie has HttpOnly and another does not).
    if (!re || !vals.some((v) => !re.test(v))) return false;
  }
  return true;
}

/** Runs a detector's `when` over an exchange. Returns a capture (to fill the
 *  chip text) when every condition holds, or null. */
function detectorMatch(ex, when) {
  const cap = { method: ex.method || '', path: ex.path || '' };
  if (when.method && when.method.length && !when.method.some((m) => m.toUpperCase() === (ex.method || '').toUpperCase())) return null;
  if (when.req_content_type) {
    const ct = (header(ex.req_headers, 'content-type') || '').toLowerCase();
    if (!ct.includes(when.req_content_type.toLowerCase())) return null;
    cap.ct = ct;
  }
  if (when.resp_content_type) {
    const ct = (header(ex.resp_headers, 'content-type') || '').toLowerCase();
    if (!ct.includes(when.resp_content_type.toLowerCase())) return null;
    cap.ct = ct;
  }
  if (when.path_regex) {
    const re = detRe(when.path_regex);
    if (!re || !re.test(ex.path || '')) return null;
  }
  if (when.req_body_regex) {
    const re = detRe(when.req_body_regex);
    if (!re || !re.test((ex.req_text || '').slice(0, 200_000))) return null;
  }
  if (when.resp_body_regex) {
    const re = detRe(when.resp_body_regex);
    if (!re || !re.test((ex.resp_text || '').slice(0, 200_000))) return null;
  }
  if (when.req_header && !headerCondMatch(ex.req_headers, when.req_header)) return null;
  if (when.resp_header && !headerCondMatch(ex.resp_headers, when.resp_header)) return null;
  if (when.status) {
    const s = ex.status || 0;
    if (when.status.min != null && s < when.status.min) return null;
    if (when.status.max != null && s > when.status.max) return null;
  }
  if (when.param) {
    const test = DET_VALUE_CLASS[when.param.value_class];
    if (!test) return null;
    const places = when.param.in && when.param.in.length ? when.param.in : ['query', 'body'];
    const pairs = [];
    if (places.includes('query')) pairs.push(...queryPairsOf(ex));
    if (places.includes('body')) pairs.push(...formPairsOf(ex));
    let hit = null;
    for (const [k, v] of pairs) {
      const what = test(v);
      if (what) {
        hit = { param: k, value: v, what };
        break;
      }
    }
    if (!hit) return null;
    cap.param = hit.param;
    cap.value = hit.value;
    cap.what = hit.what;
  }
  return cap;
}

/** Fills `{method}`, `{path}`, `{param}`, `{value}`, `{ct}`, `{what}` in a string. */
function fillTemplate(s, cap) {
  if (!s) return s;
  return s.replace(/\{(method|path|param|value|ct|what)\}/g, (_, k) => {
    const v = cap[k] != null ? String(cap[k]) : '';
    return k === 'value' ? v.slice(0, 60) : v;
  });
}

/** Detector suggestions for this exchange, already in priority order. Each is
 *  the served detector plus the capture used to fill its text. In-scope only. */
async function detectorLeads(ex) {
  if (!ex || decide(ex.host) !== 'accepted') return [];
  const defs = await loadDetectors();
  const out = [];
  for (const d of defs) {
    // An access hand-off only makes sense when the Access check is switched on.
    if (d.suggest.handler === 'access' && !toolOn('access-check')) continue;
    const cap = detectorMatch(ex, d.when || {});
    if (cap) out.push({ d, cap });
  }
  return out;
}

/** Detector suggestions without awaiting: used where a menu is built on the
 *  spot. Returns nothing until the detectors have loaded (kicking that off),
 *  which they have after the first Lens draw. */
function detectorLeadsSync(ex) {
  if (!ex || decide(ex.host) !== 'accepted') return [];
  if (!DETECTORS) {
    loadDetectors();
    return [];
  }
  const out = [];
  for (const d of DETECTORS) {
    if (d.suggest.handler === 'access' && !toolOn('access-check')) continue;
    const cap = detectorMatch(ex, d.when || {});
    if (cap) out.push({ d, cap });
  }
  return out;
}

const DET_CHIP_CLASS = { scan: 'k-scan', bench: 'k-bench', finding: 'k-warn', access: 'k-access' };

/** Acts on a detector suggestion: pre-fills the handler's tab, nothing is sent. */
function runDetectorLead(ex, d, cap) {
  const s = d.suggest;
  const title = fillTemplate(s.title, cap);
  const note = fillTemplate(s.note, cap);
  switch (s.handler) {
    case 'scan':
      scanEndpoint(ex, {
        kind: d.id,
        chip: fillTemplate(s.chip, cap),
        focus: s.focus,
        categories: Array.isArray(s.categories) ? s.categories : [],
        title,
        why: fillTemplate(s.why, cap),
        note,
      });
      break;
    case 'bench':
      benchWithNote(ex.id, note);
      break;
    case 'finding':
      findingForm(null, [ex.id], title, { severity: s.severity, note });
      break;
    case 'access':
      startAccessCheck({ targets: [ex.id], sourceLabel: `${ex.method} ${ex.path}`, onlyAnon: s.mode === 'anon' });
      break;
  }
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
  // An error or stack trace you're looking at usually isn't the only one.
  if ((ex.status >= 500 || (list || []).some((i) => i.kind === 'stack-trace')) && ex.host) {
    const cls = Math.floor((ex.status || 500) / 100) + 'xx';
    chips.push(h('button', { class: 'chip', title: `Show every ${cls} response from ${ex.host} in Traffic, so you can see how far this reaches.`, onclick: () => setQuery(`host:${ex.host} status:${cls}`) }, h('span', { text: 'Find others like this' })));
  }
  // A permissive cross-origin policy — one header combo that's easy to miss.
  const cors = corsIssue(ex);
  if (cors) {
    chips.push(h('button', { class: 'chip k-warn', title: cors.note + ' Record it as a finding.', onclick: () => findingForm(null, [ex.id], `Permissive cross-origin policy on ${ex.method} ${ex.path}`, { severity: cors.severity, note: cors.note }) }, h('span', { text: '+ Finding: open CORS policy' })));
  }
  // A login handed back a session — offer to save it as a user for the cookie jar.
  if (toolOn('saved-users') && sessionGrant(ex)) {
    chips.push(h('button', { class: 'chip k-user', title: 'Save the session this response just set as a reusable user, ready in the Bench “As…” picker and the Access check.', onclick: () => saveUserFrom(ex) }, h('span', { text: 'Save login as a user' })));
  }
  if (inScope) {
    // An id in the path or a param — check whether other users' records answer too.
    const id = idTargetOf(ex);
    if (id && toolOn('access-check')) {
      chips.push(h('button', { class: 'chip k-access', title: `Replay this request as each saved user and signed out, to see if ${id.where} (${id.value.slice(0, 24)}) lets you reach records that aren’t yours.`, onclick: () => startAccessCheck({ targets: [ex.id], sourceLabel: `${ex.method} ${ex.path}` }) }, h('span', { text: 'Check this id across users' })));
    }
    // An authenticated request — does it still work with the login removed?
    if (toolOn('access-check') && authHeadersOf(ex.req_headers || []).length && ex.status >= 200 && ex.status < 300) {
      chips.push(h('button', { class: 'chip k-access', title: 'Replay this request with your login removed, to see whether it needs you signed in at all.', onclick: () => startAccessCheck({ targets: [ex.id], sourceLabel: `${ex.method} ${ex.path}`, onlyAnon: true }) }, h('span', { text: 'Replay signed out' })));
    }
    // A value that comes straight back — set it up as a Bench experiment.
    const refl = reflectedValues(ex);
    if (refl.length) {
      const r = refl[0];
      chips.push(h('button', { class: 'chip k-bench', title: `The ${r.where} value “${r.value.slice(0, 32)}” comes back unescaped in the response. Open this request on the Bench to vary it and compare.`, onclick: () => benchWithNote(ex.id, `“${r.name}” is reflected in the response — vary it and compare.`) }, h('span', { text: 'Reflected value → Bench' })));
    }
    // A redirect-shaped parameter — open it ready to follow.
    const rd = redirectParam(ex);
    if (rd) {
      chips.push(h('button', { class: 'chip k-bench', title: `The “${rd.name}” parameter carries a URL the server may follow. Open this request on the Bench to change it and watch where it lands.`, onclick: () => benchWithNote(ex.id, `“${rd.name}” carries a redirect target — change it and follow where it goes.`) }, h('span', { text: 'Trace this redirect' })));
    }
    // GraphQL — open it on the Bench like any other request to explore.
    if (isGraphql(ex)) {
      chips.push(h('button', { class: 'chip k-bench', title: 'Open this GraphQL request on the Bench to edit the operation and explore the schema.', onclick: () => benchWithNote(ex.id, 'GraphQL endpoint — edit the operation to explore what it exposes.') }, h('span', { text: 'GraphQL → Bench' })));
    }
    // Something on this endpoint is worth taking into Scans, scoped to it.
    for (const lead of scanLeadsFor(ex)) {
      chips.push(h('button', { class: 'chip k-scan', title: lead.why, onclick: () => scanEndpoint(ex, lead) }, h('span', { text: lead.chip })));
    }
    // Detector packs: the same idea as data, so the community can add more.
    for (const { d, cap } of await detectorLeads(ex)) {
      const title = fillTemplate(d.suggest.why || d.suggest.note || d.suggest.title || d.suggest.chip, cap);
      chips.push(
        h('button', { class: `chip ${DET_CHIP_CLASS[d.suggest.handler] || ''}`, title, onclick: () => runDetectorLead(ex, d, cap) }, h('span', { text: fillTemplate(d.suggest.chip, cap) })),
      );
    }
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
  if (m !== 'GET' || (body && m !== 'POST')) parts.push('-X ' + shq(m));
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

/** A handle that drags `box`'s height: moving it up makes `box` taller.
 *  `get`/`set` read and apply the height so the caller decides what it
 *  means (a fixed height or a cap); the last size is remembered under `key`
 *  and a double-click forgets it so the box fits its content again. */
function heightGrip(key, { get, set, fit, min = 80, max }) {
  return h('div', {
    class: 'hgrip',
    title: 'Drag to resize · double-click to fit',
    ondblclick: () => {
      store(key, null);
      fit();
    },
    onmousedown: (e) => {
      e.preventDefault();
      const startY = e.clientY;
      const startH = get();
      document.body.classList.add('rowresize');
      const move = (ev) => {
        const px = Math.round(Math.max(min, Math.min(max(), startH - (ev.clientY - startY))));
        set(px);
        store(key, px);
      };
      const up = () => {
        document.body.classList.remove('rowresize');
        window.removeEventListener('mousemove', move);
        window.removeEventListener('mouseup', up);
      };
      window.addEventListener('mousemove', move);
      window.addEventListener('mouseup', up);
    },
  });
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
  shares_session: 'Shares a session',
  shares_certificate: 'Shares a certificate',
  redirected_from: 'Redirected from',
  requested_from: 'Called from',
  linked_from: 'Linked from',
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
  const label = (ev && EV[ev.kind]) || '';
  clear(
    slot,
    h(
      'div',
      { class: 'scopequeue' },
      h('button', { class: 'qcount', title: 'Review every suggestion on the Scope screen', onclick: () => go('scope') }, h('b', { text: all.length }), all.length === 1 ? ' scope decision' : ' scope decisions'),
      h('span', { class: 'qdom', text: s.domain, title: s.domain }),
      ev ? h('span', { class: 'qev', title: s.evidence.map((e) => e.summary).join('\n') }, label, ' ', ev.via) : null,
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

/**
 * Accepts (each host only) or rejects every pending suggestion, after asking.
 * Every domain in the sheet has a checkbox, so any of them can be left out.
 */
function decideAll(action) {
  const list = stillPending(S.scope.suggestions);
  if (!list.length) return;
  const accept = action === 'accept';
  const shown = (s) => (accept ? suggestionBase(s.domain) : s.domain);
  const picked = new Set(list.map((s) => s.domain));
  const run = async () => {
    const chosen = list.filter((s) => picked.has(s.domain));
    if (!chosen.length) return;
    closeModal();
    let done = 0;
    for (const s of chosen) {
      try {
        await api('/api/scope/' + action, { method: 'POST', body: { domain: shown(s), include_subdomains: false } });
        done++;
      } catch (e) {
        toast(e.message, 'err');
      }
    }
    toast(accept ? `✓ ${done} domain${done === 1 ? '' : 's'} accepted into scope` : `✗ ${done} domain${done === 1 ? '' : 's'} kept out of scope`, accept ? 'ok' : '');
    await loadScope();
    if (S.view === 'scope') renderScopeBody();
  };
  const goBtn = h('button', { class: 'btn ' + (accept ? 'primary' : 'danger'), onclick: run });
  const toggleAll = h('button', { class: 'link', onclick: () => (picked.size === list.length ? picked.clear() : list.forEach((s) => picked.add(s.domain)), boxes.forEach((b) => (b.checked = picked.has(b.value))), sync()) });
  const boxes = list.map((s) => h('input', { type: 'checkbox', value: s.domain, checked: true, onchange: (e) => (e.target.checked ? picked.add(s.domain) : picked.delete(s.domain), sync()) }));
  const sync = () => {
    const n = picked.size;
    const all = n === list.length;
    $('.modal .mcard h3').textContent = (accept ? 'Accept ' : 'Reject ') + (all ? `all ${n} suggested domains?` : `${n} of ${list.length} suggested domains?`);
    goBtn.textContent = n === 0 ? 'Nothing picked' : all ? (accept ? 'Accept all' : 'Reject all') : `${accept ? 'Accept' : 'Reject'} ${n}`;
    goBtn.disabled = n === 0;
    toggleAll.textContent = all ? 'Uncheck all' : 'Check all';
  };
  modal(
    '',
    [
      h('p', { class: 'muted', text: (accept ? 'Each host is accepted on its own, without its subdomains.' : 'They stay captured, but Bench sends to them are refused.') + ' Uncheck any domain to leave it for later.' }),
      h('div', { class: 'allbar' }, toggleAll),
      h('div', { class: 'alllist' }, list.map((s, i) => h('label', { class: 'allrow' }, boxes[i], h('span', { class: 'mono', text: shown(s), title: shown(s) })))),
    ],
    [h('button', { class: 'btn', text: 'Cancel', onclick: closeModal }), goBtn],
  );
  sync();
}

function evidenceList(evidence) {
  return h(
    'div',
    { class: 'evlist' },
    evidence.map((e) => {
      return h(
        'div',
        { class: 'ev' },
        h('span', { class: 'k', text: EV[e.kind] || e.kind }),
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

/** Sends a request to the Bench and shows a one-line note about why — used by the Mind Reader chips. */
async function benchWithNote(id, note) {
  await sendToBench(id);
  if (note) toast(note, 'ok');
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
    id: 'benchurl',
    value: tab.url,
    spellcheck: 'false',
    onfocus: () => (R.cbField = 'url'),
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
    id: 'bencheditor',
    value: tab.raw,
    spellcheck: 'false',
    onfocus: () => (R.cbField = 'editor'),
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
          toolOn('callbacks')
            ? [
                h('button', {
                  class: 'link',
                  text: 'Insert callback host',
                  title: 'Make a callback host for this request and put it where the cursor is',
                  onmousedown: (e) => e.preventDefault(),
                  onclick: () => insertCallbackHost(tab, R.cbField || 'editor', main),
                }),
                ' · ',
              ]
            : null,
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
    lensStrip ? lensGrip(lensStrip) : null,
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

/* ---- sizing ----
   An open value card gets room to show all of it: the strip grows to fit its
   content, leaving the request editor a few lines, unless the user dragged
   it to a size of their own, which is kept from then on. */
const LENS_H = 'plonix.bench.lensH';
const EDITOR_MIN = 110;
function lensRoom(strip) {
  const col = strip.parentElement;
  if (!col) return 400;
  const lbl = col.querySelector('.lbl');
  return Math.max(120, col.clientHeight - (lbl ? lbl.offsetHeight : 0) - EDITOR_MIN);
}
function lensOpen(strip) {
  strip.classList.add('open');
  strip.parentElement?.classList.add('lensopen');
  requestAnimationFrame(() => {
    for (const ta of strip.querySelectorAll('textarea.blv')) fitText(ta);
    strip.style.height = 'auto';
    const want = store(LENS_H) || strip.scrollHeight + 1;
    // Short of room: make the Request and Response columns taller (the Bench
    // scrolls) rather than squeeze the card, up to most of the window.
    const split = strip.closest('.rsplit');
    const short = want - lensRoom(strip);
    if (split && short > 0) split.style.minHeight = Math.min(split.clientHeight + short, Math.round(window.innerHeight * 0.85)) + 'px';
    strip.style.height = Math.min(want, lensRoom(strip)) + 'px';
    strip.scrollIntoView({ block: 'nearest' });
  });
}
function lensClose(strip) {
  strip.classList.remove('open');
  strip.parentElement?.classList.remove('lensopen');
  strip.style.height = '';
  const split = strip.closest('.rsplit');
  if (split) split.style.minHeight = '';
}
function lensGrip(strip) {
  return heightGrip(LENS_H, {
    get: () => strip.getBoundingClientRect().height,
    set: (px) => (strip.style.height = px + 'px'),
    fit: () => lensOpen(strip),
    min: 90,
    max: () => lensRoom(strip),
  });
}
/** Grow a textarea to show all of its text, so it never scrolls on its own. */
function fitText(ta) {
  ta.style.height = 'auto';
  ta.style.height = ta.scrollHeight + 2 + 'px';
}

function drawBenchLens(tab, editor, strip, main) {
  lensClose(strip);
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
        if (was) return clear(slot), lensClose(strip);
        chip.classList.add('on');
        clear(slot, buildValueCard(it, { start: it.start, end: it.end }, editor, tab, strip, main, true));
        lensOpen(strip);
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
    h('button', {
      class: 'blx',
      text: '✕',
      title: 'Close',
      onclick: () => {
        clear(strip._slot);
        for (const c of strip.querySelectorAll('.spot.on')) c.classList.remove('on');
        lensClose(strip);
      },
    }),
  );
  const notes = it.notes && it.notes.length ? h('div', { class: 'sdnotes' }, it.notes.map((n) => h('span', { class: 'sdnote' + (/^(unsigned|expired)/.test(n) ? ' warn' : ''), text: n }))) : null;
  if (it.kind === 'jwt') return h('div', { class: 'spotdetail' }, head, notes, jwtEditor(it, span, editor, tab));
  const live = h('span', { class: 'bllive', text: '✓ re-encoded' });
  const ta = h('textarea', { class: 'sdv blv', spellcheck: 'false', value: it.decode() });
  if (!editable) ta.readOnly = true;
  ta.addEventListener('input', () => {
    fitText(ta);
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
  const load = () => {
    ta.value = JSON.stringify(st.part === 'payload' ? it.jwt.payload : it.jwt.header, null, 2);
    if (ta.isConnected) fitText(ta);
  };
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
    fitText(ta);
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
    lensOpen(strip);
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
  // Shows up to a dozen sends at once; drag the top edge for more or fewer.
  const list = h('div', { class: 'histrows' }, rows.length ? rows : h('div', { class: 'histrow muted', text: 'No sends yet.' }));
  const saved = store(HIST_H);
  if (saved) list.style.maxHeight = saved + 'px';
  const grip = heightGrip(HIST_H, {
    get: () => list.getBoundingClientRect().height,
    set: (px) => (list.style.maxHeight = px + 'px'),
    fit: () => (list.style.maxHeight = ''),
    min: 40,
    max: () => Math.max(120, window.innerHeight - 260),
  });
  return h(
    'div',
    { class: 'hist' },
    rows.length > 1 ? grip : null,
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
    list,
  );
}
const HIST_H = 'plonix.bench.histH';

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
async function manageUsers(afterSave, prefill) {
  const users = (await loadUsers(true)).map((u) => ({ ...u, headers: (u.headers || []).map((h) => [...h]) }));
  if (prefill) users.push({ id: '', name: prefill.name || '', note: prefill.note || '', headers: prefill.headers || [] });
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
function startAccessCheck({ targets = [], host = null, prefix = '/', sourceLabel = '', onlyAnon = false } = {}) {
  AC.targets = targets;
  AC.host = host;
  AC.prefix = prefix || '/';
  AC.sourceLabel = sourceLabel;
  AC.report = null;
  AC.err = null;
  AC.anon = onlyAnon ? true : AC.anon;
  AC.picks = onlyAnon ? new Set() : null; // onlyAnon: signed-out only; otherwise every saved user
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

/* ---- the Callbacks screen ---- */

// What the Callbacks screen knows: the listener's state, its hosts and the
// callbacks seen so far (newest last), and which host and callback are open.
const CB = { data: null, list: [], seq: 0, host: null, sel: null, busy: false, draft: '' };

const cbProto = (p) => ({ dns: 'DNS', http: 'HTTP', https: 'HTTPS', smtp: 'SMTP', smtps: 'SMTP', ldap: 'LDAP', ftp: 'FTP', smb: 'SMB', responder: 'SMB' })[p] || (p || '').toUpperCase();
const cbHostOf = (id) => ((CB.data && CB.data.payloads) || []).find((p) => p.id === id) || null;
const cbSeen = () => Number(pstore('plonix.callbacksSeen') || 0);

/** Fetches what changed. Callbacks only ever arrive, so only new ones are asked for. */
async function loadCallbacks(full) {
  const since = full ? 0 : CB.seq;
  const d = await api('/api/callbacks?since=' + since);
  if (full || d.seq < CB.seq) CB.list = [];
  CB.list.push(...d.interactions);
  if (CB.list.length > 500) CB.list = CB.list.slice(-500);
  CB.seq = d.seq;
  CB.data = d;
  return d;
}

/** The status poll noticed a new callback (or none) — refresh the screen if it is open. */
function callbacksChanged() {
  if (S.view === 'callbacks') loadCallbacks().then(() => S.view === 'callbacks' && drawCallbacks()).catch(() => {});
}

async function renderCallbacks(main) {
  clear(main, h('div', { class: 'view' }, h('div', { class: 'toolbar' }, backButton(), h('h2', { text: 'Callbacks' }), h('span', { id: 'cbstate' })), h('div', { class: 'cbbody', id: 'cbbody' })));
  try {
    await loadCallbacks(true);
  } catch (e) {
    return clear($('#cbbody'), h('div', { class: 'rerr', text: e.message }));
  }
  drawCallbacks();
}

function drawCallbacks() {
  const d = CB.data;
  const body = $('#cbbody');
  if (!d || !body) return;
  pstore('plonix.callbacksSeen', d.seq);
  updateChrome();
  drawCallbacksState();
  if (!d.installed) return clear(body, callbacksInstallCard());
  const hosts = h('div', { class: 'cbhosts' });
  const feed = h('div', { class: 'cbfeed' });
  clear(body, hosts, feed);
  drawCallbackHosts(hosts);
  drawCallbackFeed(feed);
}

function drawCallbacksState() {
  const box = $('#cbstate');
  if (!box) return;
  const d = CB.data;
  const listening = d.phase === 'listening';
  const starting = d.phase === 'starting';
  const server = d.base ? d.base.split('.').slice(1).join('.') : d.config.server || 'public server';
  const pill = h(
    'span',
    { class: 'cbpill ' + d.phase },
    h('span', { class: 'cbdot' }),
    listening ? 'Listening on ' + server : starting ? 'Connecting…' : d.phase === 'failed' ? 'Stopped' : 'Not listening',
  );
  const toggle = h('button', {
    class: 'btn sm' + (listening || starting ? '' : ' primary'),
    text: listening || starting ? 'Stop' : 'Start listening',
    disabled: !d.installed || CB.busy,
    title: listening ? 'Stop collecting callbacks. Hosts already handed out keep working and their callbacks arrive when you start again.' : 'Register with the callback server and start collecting callbacks',
    onclick: () => callbacksToggle(listening || starting),
  });
  clear(
    box,
    h('span', { class: 'hint', text: 'Put a host in a request; anything that later reaches it shows up here.' }),
    pill,
    h('button', { class: 'btn sm', text: 'Server…', title: 'Use the public servers or your own', disabled: !d.installed, onclick: callbacksServer }),
    toggle,
  );
  box.className = 'cbstate';
}

async function callbacksToggle(stop) {
  CB.busy = true;
  drawCallbacksState();
  try {
    CB.data = { ...CB.data, ...(await api('/api/callbacks/' + (stop ? 'stop' : 'start'), { method: 'POST' })), interactions: [] };
  } catch (e) {
    toast(e.message, 'err');
  }
  CB.busy = false;
  drawCallbacks();
}

function callbacksInstallCard() {
  const cmd = CB.data.install;
  return h(
    'div',
    { class: 'empty cbinstall' },
    h('h3', { text: 'One thing to install' }),
    h('p', { class: 'mnote', text: 'Callbacks are collected by interactsh, the open-source callback tool by ProjectDiscovery, which runs on this Mac. Install it once:' }),
    h('div', { class: 'cbcmd' }, h('code', { text: cmd }), h('button', { class: 'btn sm', text: 'Copy', onclick: () => copyText(cmd) })),
    h('div', { class: 'cbacts' }, h('button', { class: 'btn primary', text: 'Check again', onclick: () => renderCallbacks($('#main')) })),
  );
}

function drawCallbackHosts(box) {
  const d = CB.data;
  const ready = !!d.base;
  // Redrawn whenever a callback arrives, so keep what is being typed.
  const typing = document.activeElement && document.activeElement.classList.contains('cbnew');
  const label = h('input', {
    class: 'cbnew',
    value: CB.draft,
    placeholder: ready ? 'What it’s for, e.g. avatar URL on /profile' : 'Start listening to make hosts',
    disabled: !ready,
    oninput: () => (CB.draft = label.value),
    onkeydown: (e) => e.key === 'Enter' && make(),
  });
  const make = async () => {
    try {
      const p = await api('/api/callbacks/payloads', { method: 'POST', body: { label: label.value.trim() } });
      CB.draft = '';
      await loadCallbacks();
      CB.host = p.id;
      drawCallbacks();
      copyText(p.host);
    } catch (e) {
      toast(e.message, 'err');
    }
  };
  const all = h(
    'button',
    { class: 'cbhost all' + (CB.host ? '' : ' on'), onclick: () => ((CB.host = null), (CB.sel = null), drawCallbacks()) },
    h('span', { class: 'cblabel', text: 'All callbacks' }),
    h('span', { class: 'cbhits', text: String(CB.list.length) }),
  );
  const rows = d.payloads.map((p) =>
    h(
      'div',
      { class: 'cbhost' + (CB.host === p.id ? ' on' : '') + (p.live ? '' : ' stale'), onclick: () => ((CB.host = CB.host === p.id ? null : p.id), (CB.sel = null), drawCallbacks()) },
      h('div', { class: 'cbhrow' }, h('span', { class: 'cblabel', text: p.label || 'Untitled host' }), h('span', { class: 'cbhits' + (p.hits ? ' hot' : ''), text: String(p.hits), title: p.hits + (p.hits === 1 ? ' callback' : ' callbacks') })),
      h('div', { class: 'cbhrow' }, h('span', { class: 'mono cbname', text: p.host, title: p.host }), p.live ? h('span', { class: 'cbtime', text: fmtTime(p.created_at) }) : h('span', { class: 'cbtime', text: 'other server', title: 'Made on another callback server; switch back to it to hear from this host again' })),
      h(
        'div',
        { class: 'cbhacts', onclick: (e) => e.stopPropagation() },
        h('button', { class: 'link', text: 'Copy', onclick: () => copyText(p.host) }),
        h('button', { class: 'link', text: 'Find in traffic', title: 'Show the requests that carried this host', onclick: () => setQuery('"' + p.id + '"') }),
        h('button', { class: 'link', text: 'Rename', onclick: () => renameCallbackHost(p) }),
        h('button', { class: 'link danger', text: 'Remove', onclick: () => removeCallbackHost(p) }),
      ),
    ),
  );
  clear(
    box,
    h('div', { class: 'cbnewrow' }, label, h('button', { class: 'btn sm primary', text: 'New host', disabled: !ready, onclick: make })),
    h('div', { class: 'cbhint muted', text: 'Each test gets a host of its own, so a callback points at the test it came from. A new host is copied for you.' }),
    h('div', { class: 'cbhostlist' }, all, rows.length ? rows : h('div', { class: 'cbnone muted', text: ready ? 'No hosts yet. Name one above, or insert one from the Bench.' : 'Hosts appear once listening has started.' })),
    h('div', { class: 'cbcredit muted', text: 'Uses interactsh by ProjectDiscovery · MIT license' }),
  );
  if (typing) label.focus();
}

function renameCallbackHost(p) {
  const input = h('input', { value: p.label });
  const save = async () => {
    try {
      await api('/api/callbacks/payloads/' + p.id, { method: 'PATCH', body: { label: input.value } });
      closeModal();
      await loadCallbacks();
      drawCallbacks();
    } catch (e) {
      m.err.textContent = e.message;
    }
  };
  input.onkeydown = (e) => e.key === 'Enter' && save();
  const m = modal('Rename host', [h('label', null, 'What it’s for', input)], [h('button', { class: 'btn', text: 'Cancel', onclick: closeModal }), h('button', { class: 'btn primary', text: 'Save', onclick: save })]);
}

async function removeCallbackHost(p) {
  try {
    await api('/api/callbacks/payloads/' + p.id, { method: 'DELETE' });
    if (CB.host === p.id) CB.host = null;
    await loadCallbacks();
    drawCallbacks();
  } catch (e) {
    toast(e.message, 'err');
  }
}

/** One line on what a callback was: the record asked for, the request line, the sender. */
function callbackSummary(i) {
  if (i.protocol === 'dns') return (i.q_type || 'A') + ' lookup of ' + i.full_id;
  if (i.protocol === 'smtp' || i.protocol === 'smtps') return i.smtp_from ? 'Mail from ' + i.smtp_from : 'Mail';
  const first = (i.raw_request || '').split(/\r?\n/)[0];
  return first || i.full_id;
}

function drawCallbackFeed(box) {
  const d = CB.data;
  const list = CB.list.filter((i) => !CB.host || i.payload === CB.host).slice().reverse();
  const host = CB.host && cbHostOf(CB.host);
  const head = h(
    'div',
    { class: 'cbfeedhead' },
    h('b', { text: host ? host.label || 'Untitled host' : 'All callbacks' }),
    h('span', { class: 'muted', text: ' · ' + list.length + (list.length === 1 ? ' callback' : ' callbacks') }),
    CB.list.length ? h('button', { class: 'link', style: { marginLeft: 'auto' }, text: 'Clear all', onclick: clearCallbacks }) : null,
  );
  if (!list.length) {
    const why =
      d.phase === 'listening'
        ? host
          ? 'Nothing has reached this host yet. Callbacks arrive within a few seconds of the call.'
          : 'Waiting for callbacks. Put a host in a request, send it, and anything that reaches the host shows up here.'
        : d.phase === 'starting'
          ? 'Connecting to the callback server…'
          : d.error || 'Press Start listening to register with a callback server. Nothing is sent to any target.';
    return clear(box, head, h('div', { class: 'empty cbempty' + (d.phase === 'failed' ? ' bad' : ''), text: why }));
  }
  if (!CB.sel || !list.some((i) => i.seq === CB.sel)) CB.sel = list[0].seq;
  const rows = list.map((i) => {
    const p = i.payload && cbHostOf(i.payload);
    return h(
      'tr',
      { class: CB.sel === i.seq ? 'on' : '', onclick: () => ((CB.sel = i.seq), drawCallbackFeed(box)) },
      h('td', { class: 'cbt mono', text: fmtTime(i.at) }),
      h('td', null, h('span', { class: 'cbproto p-' + i.protocol, text: cbProto(i.protocol) })),
      h('td', { class: 'cbfor', text: p ? p.label || 'Untitled host' : 'Unlisted host', title: i.full_id }),
      h('td', { class: 'mono cbfrom', text: i.remote }),
      h('td', { class: 'mono cbwhat', text: callbackSummary(i), title: callbackSummary(i) }),
    );
  });
  const table = h(
    'div',
    { class: 'cbtablewrap' },
    h(
      'table',
      { class: 'cbtable' },
      h('colgroup', null, h('col', { class: 'c-t' }), h('col', { class: 'c-p' }), h('col', { class: 'c-f' }), h('col', { class: 'c-r' }), h('col')),
      h('thead', null, h('tr', null, ['Time', 'Type', 'Host', 'From', 'What arrived'].map((t) => h('th', { text: t })))),
      h('tbody', null, rows),
    ),
  );
  const detail = h('div', { class: 'cbdetail' });
  clear(box, head, table, detail);
  drawCallbackDetail(detail, list.find((i) => i.seq === CB.sel));
}

function drawCallbackDetail(box, i) {
  if (!i) return clear(box);
  const p = i.payload && cbHostOf(i.payload);
  const facts = [
    ['Received', new Date(i.at).toLocaleString()],
    ['From', i.remote],
    ['Name', i.full_id],
    p ? ['Host', p.host] : null,
  ].filter(Boolean);
  const raw = (title, text) => (text ? h('div', { class: 'cbraw' }, h('div', { class: 'lbl', text: title }), h('pre', { class: 'raw', text: text })) : null);
  clear(
    box,
    h(
      'div',
      { class: 'cbdhead' },
      h('span', { class: 'cbproto p-' + i.protocol, text: cbProto(i.protocol) }),
      h('b', { class: 'cbdtitle', text: p ? p.label || 'Untitled host' : 'Unlisted host' }),
      h(
        'span',
        { class: 'cbdacts' },
        p ? h('button', { class: 'btn sm', text: 'Find the request', title: 'Show the captured requests that carried this host', onclick: () => setQuery('"' + p.id + '"') }) : null,
        h('button', { class: 'btn sm', text: 'Copy', onclick: () => copyText(i.raw_request || i.full_id) }),
        h('button', { class: 'btn sm primary', text: '+ Finding', onclick: () => findingFromCallback(i, p) }),
      ),
    ),
    h('div', { class: 'cbfacts' }, facts.map(([k, v]) => h('div', null, h('span', { class: 'muted', text: k }), h('span', { class: 'mono', text: v })))),
    h('div', { class: 'cbraws' }, raw('What arrived', i.raw_request), raw('What the server answered', i.raw_response)),
  );
}

/** Opens a new finding with the callback as evidence, and the request that carried its host when Plonix captured one. */
async function findingFromCallback(i, p) {
  let ids = [];
  if (p) {
    try {
      const r = await api('/api/traffic?limit=3&q=' + encodeURIComponent('"' + p.id + '"'));
      ids = r.items.map((x) => x.id);
    } catch (_) {}
  }
  const what = cbProto(i.protocol) + (i.protocol === 'dns' ? ' lookup' : i.protocol.startsWith('smtp') ? ' mail' : ' request');
  const description = [
    `The server made an outbound ${what} to a host it was only given in a request${p && p.label ? ` (${p.label})` : ''}.`,
    '',
    `Received: ${new Date(i.at).toISOString()}`,
    `From: ${i.remote}`,
    `Name: ${i.full_id}`,
    i.raw_request ? '\n' + i.raw_request.slice(0, 4000) : '',
  ].join('\n');
  findingForm(null, ids, `Server makes an outbound ${what} to a host supplied in a request`, {
    severity: 'medium',
    note: ids.length ? `request #${ids[0]} carried this host.` : 'no captured request carries this host; add the one that sent it as evidence.',
    description,
  });
}

async function clearCallbacks() {
  try {
    await api('/api/callbacks/clear', { method: 'POST' });
    CB.sel = null;
    await loadCallbacks(true);
    drawCallbacks();
  } catch (e) {
    toast(e.message, 'err');
  }
}

function callbacksServer() {
  const c = CB.data.config;
  const server = h('input', { class: 'mono', value: c.server, placeholder: 'Public servers', spellcheck: 'false' });
  const token = h('input', { class: 'mono', type: 'password', placeholder: c.has_token ? 'Saved — type to replace' : 'Only for a server that asks for one', autocomplete: 'off' });
  const save = async () => {
    const body = { server: server.value.trim() };
    if (token.value.trim() || (!server.value.trim() && c.has_token)) body.token = token.value.trim();
    try {
      CB.data = { ...CB.data, ...(await api('/api/callbacks/config', { method: 'PUT', body })), interactions: [] };
      closeModal();
      toast(CB.data.phase === 'listening' ? 'Saved. Stop and start listening to switch servers.' : 'Saved', 'ok');
      drawCallbacks();
    } catch (e) {
      m.err.textContent = e.message;
    }
  };
  const m = modal(
    'Callback server',
    [
      h('p', { class: 'mnote muted', text: 'Leave the address empty to use the public servers. A server you host yourself keeps callbacks private to you.' }),
      h('label', null, 'Server address', server),
      h('label', null, 'Token', token),
      h('p', { class: 'mnote muted', text: 'Changing the server gives out new hosts; ones handed out earlier stop reaching this project.' }),
    ],
    [h('button', { class: 'btn', text: 'Cancel', onclick: closeModal }), h('button', { class: 'btn primary', text: 'Save', onclick: save })],
  );
}

const urlPath = (u) => {
  try {
    const x = new URL(u);
    return x.host + x.pathname;
  } catch (_) {
    return u;
  }
};

/** Makes a callback host for this Bench request and puts it where the cursor was. */
async function insertCallbackHost(tab, field, main) {
  const el = field === 'url' ? $('#benchurl') : $('#bencheditor');
  if (!el) return;
  const start = el.selectionStart ?? el.value.length;
  const end = el.selectionEnd ?? start;
  let p;
  try {
    p = await api('/api/callbacks/payloads', { method: 'POST', body: { label: `${tab.method} ${urlPath(tab.url)}`.slice(0, 120) } });
  } catch (e) {
    if (e.code === 'not_listening') {
      toast('Start listening in Callbacks first, then insert a host.', 'err');
      return leaveTo('callbacks');
    }
    return toast(e.message, 'err');
  }
  el.value = el.value.slice(0, start) + p.host + el.value.slice(end);
  if (field === 'url') tab.url = el.value;
  else tab.raw = el.value;
  saveBench();
  renderBench(main);
  toast('Callback host inserted. Its callbacks show up in Callbacks.', 'ok');
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
  const desc = h('textarea', { placeholder: 'What happens, how to reproduce it, and why it matters.', value: f ? f.description : (hint && hint.description) || '' });
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
const SC = { host: null, hosts: [], suggest: null, suggestErr: null, picks: null, intrusive: false, running: false, report: null, focus: null, crawl: null, crawling: false, crawlStart: '/', crawlBrowser: false, crawlClick: false };

/**
 * Hands one endpoint to Scans, focused: the scan is narrowed to this endpoint
 * (the engine aims injecting checks only here) and the checks that fit the
 * lead are pre-picked. Nothing runs until the researcher presses Run scan.
 */
function scanEndpoint(ex, lead) {
  SC.host = ex.host;
  SC.focus = {
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

/** Drops the endpoint focus and goes back to scanning the whole host. */
function clearScanFocus() {
  SC.focus = null;
  if (SC.suggest) {
    SC.picks = new Set(SC.suggest.recommended.map((t) => t.id));
    SC.intrusive = false;
  }
  drawScans();
}

const INTRU_LABEL = { passive: 'Passive', safe: 'Safe', active: 'Active', intrusive: 'Intrusive' };
const INTRU_TAG = { passive: 'in', safe: 'in', active: 'upd', intrusive: 'rej' };

/* ---------- programs ---------- */

/** The Programs screen: bring in a bug bounty or disclosure program, review
 * what it changes, and follow its rules. */
const PG = { platforms: [], program: null, cats: {}, source: null, draft: null, preview: null, filter: '', mode: 'programs', bountyOnly: false, poll: null, busy: false };

const KIND_LABEL = { web: 'Web', wildcard: 'Wildcard', ip: 'IP', cidr: 'IP range', mobile: 'Mobile app', source: 'Source code', other: 'Other' };

function renderPrograms(main) {
  clear(
    main,
    h(
      'div',
      { class: 'view' },
      h(
        'div',
        { class: 'toolbar' },
        h('h2', { text: 'Programs' }),
        h('span', { class: 'hint', text: 'Bring in a bug bounty or disclosure program. Plonix sets your scope from it and follows its rules on every request it sends.' }),
      ),
      h('div', { class: 'pane' }, h('div', { class: 'stack progs', id: 'progbody' }, h('div', { class: 'muted', text: 'Loading…' }))),
    ),
  );
  loadPrograms();
}

async function loadPrograms() {
  try {
    const [cur, plats] = await Promise.all([api('/api/program'), api('/api/platforms')]);
    PG.program = cur.program;
    PG.platforms = plats.platforms || [];
  } catch (e) {
    return toast(e.message, 'err');
  }
  if (!PG.source) PG.source = (PG.platforms.find((p) => p.connected) || PG.platforms[0] || { name: 'paste' }).name;
  drawPrograms();
  const p = PG.platforms.find((x) => x.name === PG.source);
  if (p && p.connected) loadPlatformPrograms(p.name);
}

function drawPrograms() {
  const box = $('#progbody');
  if (!box || S.view !== 'programs') return;
  if (PG.preview) return drawProgramReview(box);
  clear(box, PG.program ? followingCard(PG.program) : null, h('div', { class: 'sechead' }, h('h3', { text: PG.program ? 'Switch to another program' : 'Bring in a program' })), sourcePicker());
}

function ruleChips(r) {
  const chips = [];
  if (r.rate_per_second) chips.push(h('span', { class: 'chip', text: `At most ${fmtRate(r.rate_per_second)}` }));
  for (const hd of r.headers || []) chips.push(h('span', { class: 'chip k-host' + (hd.needs_value ? ' k-warn' : ''), title: hd.needs_value ? 'Fill in the value to start sending it' : 'Added to every request to the program', text: `${hd.name}: ${hd.value}` }));
  if (r.no_automation) chips.push(h('span', { class: 'chip k-bad', title: 'Scans, crawls and Bench runs are off for this project', text: 'No automated testing' }));
  if (r.no_intrusive) chips.push(h('span', { class: 'chip', title: 'Intrusive scan checks are off and cannot be picked', text: 'No disruptive tests' }));
  return chips;
}

function fmtRate(n) {
  if (n >= 1) return `${+n.toFixed(2)} request${n === 1 ? '' : 's'} per second`;
  const perMin = n * 60;
  return `${+perMin.toFixed(1)} requests per minute`;
}

function followingCard(p) {
  const inScope = p.assets.filter((a) => a.in_scope);
  const out = p.assets.filter((a) => !a.in_scope);
  const platform = PG.platforms.find((x) => x.name === p.platform);
  return h(
    'div',
    { class: 'card progcur' },
    h(
      'div',
      { class: 'top' },
      h('div', { class: 'ttl' }, h('span', { class: 'tag in', text: 'Following' }), h('b', { text: p.name }), h('span', { class: 'meta', text: `${platform ? platform.title : p.platform === 'pasted' ? 'Pasted policy' : p.platform} · synced ${fmtDate(p.synced_at)}` })),
      h(
        'span',
        { class: 'acts' },
        p.url ? h('a', { class: 'btn sm', href: p.url, target: '_blank', rel: 'noopener', text: 'Program page' }) : null,
        platform && platform.connected ? h('button', { class: 'btn sm', text: 'Sync again', onclick: () => reviewFromPlatform(p.platform, { handle: p.id, name: p.name, bounty: p.bounty }) }) : null,
        h('button', { class: 'btn sm', text: 'Edit rules', onclick: () => reviewDraft(structuredClone(p)) }),
        h('button', { class: 'btn sm danger', text: 'Stop following', onclick: () => stopFollowing(p) }),
      ),
    ),
    scopeDrift(p),
    h('div', { class: 'chips progrules' }, ruleChips(p.rules)),
    h('div', { class: 'progsum muted', text: `${inScope.length} in scope · ${out.length} out of scope${p.rules.not_accepted && p.rules.not_accepted.length ? ` · ${p.rules.not_accepted.length} kinds of report not accepted` : ''}` }),
    assetTable(p.assets),
    p.rules.not_accepted && p.rules.not_accepted.length
      ? h('details', { class: 'prognot' }, h('summary', { text: 'What this program does not accept' }), h('ul', null, p.rules.not_accepted.map((x) => h('li', { text: x }))))
      : null,
  );
}

// The followed program as the last sync saw it, when its scope has changed since.
function scopeDrift(p) {
  const cat = PG.cats[p.platform] && PG.cats[p.platform].catalog;
  const e = cat && cat.programs.find((x) => x.program.id === p.id);
  if (!e || e.program.synced_at <= p.synced_at) return null;
  const key = (list) => list.map((a) => `${a.in_scope ? '+' : '-'}${a.identifier}`).sort().join('\n');
  if (key(e.program.assets) === key(p.assets)) return null;
  const platform = PG.platforms.find((x) => x.name === p.platform);
  return h(
    'div',
    { class: 'progdrift' },
    h('span', { text: `${p.name} changed its scope on ${platform ? platform.title : p.platform} since you started following it.` }),
    h('button', { class: 'btn sm', text: 'Review the changes', onclick: () => reviewDraft({ ...structuredClone(e.program), rules: structuredClone(p.rules) }) }),
  );
}

function assetTable(assets) {
  if (!assets.length) return h('div', { class: 'empty', text: 'No assets listed.' });
  const sorted = assets.slice().sort((a, b) => Number(b.in_scope) - Number(a.in_scope));
  return h(
    'table',
    { class: 'grid' },
    h('thead', null, h('tr', null, h('th', { text: 'Asset' }), h('th', { text: 'Type' }), h('th', { text: 'Scope' }), h('th', { text: 'Bounty' }), h('th', { text: 'Notes' }))),
    h(
      'tbody',
      null,
      sorted.map((a) =>
        h(
          'tr',
          null,
          h('td', { class: 'mono', text: a.identifier }),
          h('td', { text: KIND_LABEL[a.kind] || a.kind }),
          h('td', null, h('span', { class: 'tag ' + (a.in_scope ? 'in' : 'rej'), text: a.in_scope ? 'in scope' : 'out of scope' })),
          h('td', { class: 'muted', text: a.bounty ? (a.max_severity ? `up to ${a.max_severity}` : 'yes') : '' }),
          h('td', { class: 'muted pnote', text: a.instruction || (['mobile', 'source', 'other'].includes(a.kind) ? 'Outside Plonix scope rules' : '') }),
        ),
      ),
    ),
  );
}

async function stopFollowing(p) {
  const keep = h('input', { type: 'checkbox', checked: true });
  const m = modal(
    `Stop following ${p.name}?`,
    h('div', null, h('p', { text: 'Its rate limit, headers and testing rules stop applying to this project.' }), h('label', { class: 'frow-inline' }, keep, ' Keep the scope rules it added')),
    [
      h('button', { class: 'btn', text: 'Cancel', onclick: closeModal }),
      h('button', {
        class: 'btn danger',
        text: 'Stop following',
        onclick: async () => {
          try {
            await api('/api/program/clear', { method: 'POST', body: { remove_scope: !keep.checked } });
            closeModal();
            toast(`No longer following ${p.name}`);
            await loadScope();
            loadPrograms();
          } catch (e) {
            m.err.textContent = e.message;
          }
        },
      }),
    ],
  );
}

function sourcePicker() {
  const sources = [...PG.platforms.map((p) => ({ key: p.name, label: p.title })), { key: 'paste', label: 'Paste a policy' }, { key: 'domain', label: 'From a domain' }];
  const body = h('div', { class: 'progsrc' });
  const p = PG.platforms.find((x) => x.name === PG.source);
  if (p) append(body, [p.connected ? platformPrograms(p) : connectForm(p)]);
  else if (PG.source === 'domain') append(body, [domainForm()]);
  else append(body, [pasteForm()]);
  return h(
    'div',
    { class: 'card' },
    h(
      'div',
      { class: 'progtabs' },
      h(
        'div',
        { class: 'seg-ctl' },
        sources.map((s) =>
          h('button', {
            class: 'segbtn' + (PG.source === s.key ? ' on' : ''),
            text: s.label,
            onclick: () => {
              PG.source = s.key;
              drawPrograms();
              const pl = PG.platforms.find((x) => x.name === s.key);
              if (pl && pl.connected) loadPlatformPrograms(pl.name);
            },
          }),
        ),
      ),
      h('span', { class: 'muted small', text: 'More platforms come from the Market.' }),
    ),
    body,
  );
}

function connectForm(p) {
  const user = p.auth.kind === 'basic' ? h('input', { type: 'text', id: 'pg-user', placeholder: p.auth.user_label, autocomplete: 'off', spellcheck: 'false' }) : null;
  const secret = h('input', { type: 'password', id: 'pg-secret', placeholder: p.auth.secret_label, autocomplete: 'off' });
  const status = h('span', { class: 'fstatus' });
  const go = async () => {
    status.className = 'fstatus';
    status.textContent = `Checking with ${p.title}…`;
    try {
      const r = await api(`/api/platforms/${encodeURIComponent(p.name)}/connect`, { method: 'POST', body: { user: user ? user.value : '', secret: secret.value } });
      toast(`Connected to ${p.title}. Pulling ${plural(r.programs, 'program')} with their scope and rules…`);
      p.connected = true;
      PG.cats[p.name] = { sync: r.sync || { running: true } };
      drawPrograms();
      loadPlatformPrograms(p.name);
    } catch (e) {
      status.className = 'fstatus bad';
      status.textContent = e.message;
    }
  };
  secret.addEventListener('keydown', (e) => e.key === 'Enter' && go());
  return h(
    'div',
    { class: 'progform' },
    h('p', null, `Connect ${p.title} and Plonix pulls every program you can work on there, with its scope and rules. `, p.auth.help, ' ', p.auth.token_url ? h('a', { href: p.auth.token_url, target: '_blank', rel: 'noopener', text: 'Get a token' }) : null),
    h('div', { class: 'progconnect' }, user, secret, h('button', { class: 'btn primary', text: 'Connect', onclick: go })),
    h('div', { class: 'progfoot' }, status, h('span', { class: 'muted small', text: IN_APP ? 'The token is kept in your Keychain and never shown to AI agents.' : 'The token stays on this computer and is never shown to AI agents.' })),
  );
}

// Every program at a connected platform, pulled once with the token and kept
// on this computer. PG.cats[name] = { catalog, sync, error }.
async function loadPlatformPrograms(name, { start = false } = {}) {
  const enc = encodeURIComponent(name);
  try {
    let r = await api(`/api/platforms/${enc}/catalog`);
    const stale = r.catalog && Date.now() - r.catalog.synced_at > 24 * 3600 * 1000;
    if (!r.sync.running && (start || (!r.catalog && !r.sync.error) || (stale && !r.sync.error))) {
      const s = await api(`/api/platforms/${enc}/sync`, { method: 'POST', body: {} });
      r = { ...r, sync: s.sync };
    }
    PG.cats[name] = { catalog: r.catalog, sync: r.sync };
  } catch (e) {
    PG.cats[name] = { ...(PG.cats[name] || {}), error: e.message, code: e.code };
  }
  if (S.view !== 'programs') return;
  drawCatalog(name);
  clearTimeout(PG.poll);
  if (PG.cats[name].sync && PG.cats[name].sync.running) PG.poll = setTimeout(() => loadPlatformPrograms(name), 1200);
}

// Redraws just the platform tab, so the rest of the screen stays put.
function drawCatalog(name) {
  const box = $('#pg-cat');
  if (!box || PG.source !== name || PG.preview) return drawPrograms();
  const p = PG.platforms.find((x) => x.name === name);
  if (p) box.replaceWith(platformPrograms(p));
  const cur = $('.progcur');
  if (cur && PG.program) cur.replaceWith(followingCard(PG.program));
}

const agoText = (ms) => {
  const s = (Date.now() - ms) / 1000;
  return s < 60 ? 'just now' : s < 3600 ? `${Math.round(s / 60)} min ago` : s < 86400 ? `${Math.round(s / 3600)} h ago` : fmtDate(ms);
};

const plural = (n, one, many = one + 's') => `${n.toLocaleString()} ${n === 1 ? one : many}`;

function platformPrograms(p) {
  const c = PG.cats[p.name] || {};
  const cat = c.catalog;
  const sync = c.sync || {};
  const disconnect = h('button', {
    class: 'btn sm',
    text: 'Disconnect',
    onclick: async () => {
      await api(`/api/platforms/${encodeURIComponent(p.name)}/disconnect`, { method: 'POST', body: {} }).catch((e) => toast(e.message, 'err'));
      p.connected = false;
      delete PG.cats[p.name];
      drawPrograms();
    },
  });
  const syncBtn = h('button', { class: 'btn sm', text: sync.running ? 'Syncing…' : 'Sync now', disabled: !!sync.running, onclick: () => loadPlatformPrograms(p.name, { start: true }) });

  let state;
  if (sync.running) {
    const pct = sync.total ? Math.round((sync.done / sync.total) * 100) : 0;
    state = h(
      'div',
      { class: 'progsync' },
      h('span', { text: sync.total ? `Pulling programs from ${p.title}: ${sync.done.toLocaleString()} of ${sync.total.toLocaleString()}` : `Listing your ${p.title} programs…` }),
      h('div', { class: 'pbar' + (sync.total ? '' : ' busy') }, h('div', { class: 'pbar-fill', style: { width: `${pct}%` } })),
    );
  } else if (cat) {
    const inScope = cat.programs.reduce((n, e) => n + e.program.assets.filter((a) => a.in_scope).length, 0);
    state = h('span', { class: 'muted small', text: `${plural(cat.programs.length, 'program')} · ${plural(inScope, 'in-scope asset')} · synced ${agoText(cat.synced_at)}` });
  } else state = h('span');

  const err = c.error || sync.error;
  const head = h('div', { class: 'progcathead' }, state, h('span', { class: 'spacer' }), syncBtn, disconnect);
  if (!cat)
    return h(
      'div',
      { id: 'pg-cat' },
      head,
      err ? h('div', { class: 'progpad' }, h('p', { class: 'ferr', text: err })) : sync.running ? null : h('div', { class: 'muted progpad', text: `Getting ready to pull your ${p.title} programs…` }),
    );

  const search = h('input', { type: 'search', id: 'pg-filter', placeholder: PG.mode === 'assets' ? 'Find an asset or program' : 'Find a program or one of its assets', value: PG.filter, spellcheck: 'false' });
  const body = h('div', { class: 'proglist', id: 'pg-list' });
  const fill = () => clear(body, PG.mode === 'assets' ? catalogAssets(p, cat) : catalogPrograms(p, cat));
  search.addEventListener('input', () => {
    PG.filter = search.value;
    fill();
  });
  const modes = h(
    'div',
    { class: 'seg-ctl' },
    [
      ['programs', 'Programs'],
      ['assets', 'Assets'],
    ].map(([k, label]) =>
      h('button', {
        class: 'segbtn' + (PG.mode === k ? ' on' : ''),
        text: label,
        onclick: () => {
          PG.mode = k;
          drawCatalog(p.name);
        },
      }),
    ),
  );
  const bountyOnly = h('label', { class: 'progcheck' }, h('input', { type: 'checkbox', checked: PG.bountyOnly, onchange: (e) => ((PG.bountyOnly = e.target.checked), fill()) }), 'Bounty only');
  fill();
  return h(
    'div',
    { id: 'pg-cat' },
    head,
    err ? h('div', { class: 'progpad' }, h('p', { class: 'ferr', text: `The last sync stopped: ${err}` })) : null,
    h('div', { class: 'progcatbar' }, modes, search, bountyOnly),
    body,
    cat.failed && cat.failed.length
      ? h('details', { class: 'prognot' }, h('summary', { text: `${plural(cat.failed.length, 'program')} could not be read this time` }), h('ul', null, cat.failed.map((f) => h('li', { text: `${f.name}: ${f.error}` }))))
      : null,
  );
}

// Programs taking reports first, then bounty programs, then by name.
function catalogOrder(cat) {
  const open = (e) => (!e.state || e.state === 'open' ? 0 : 1);
  return cat.programs.slice().sort((a, b) => open(a) - open(b) || Number(b.program.bounty) - Number(a.program.bounty) || a.program.name.localeCompare(b.program.name));
}

function kindCounts(assets) {
  const n = {};
  for (const a of assets) n[a.kind] = (n[a.kind] || 0) + 1;
  return Object.entries(n)
    .sort((a, b) => b[1] - a[1])
    .map(([k, c]) => `${c} ${(KIND_LABEL[k] || k).toLowerCase()}`)
    .join(', ');
}

function shortRate(n) {
  return n >= 1 ? `${+n.toFixed(2)} req/s` : `${+(n * 60).toFixed(1)} req/min`;
}

function catalogPrograms(p, cat) {
  const q = PG.filter.trim().toLowerCase();
  const rows = [];
  for (const e of catalogOrder(cat)) {
    const x = e.program;
    if (PG.bountyOnly && !x.bounty) continue;
    let hit = null;
    if (q && !x.name.toLowerCase().includes(q) && !x.id.toLowerCase().includes(q)) {
      hit = x.assets.find((a) => a.identifier.toLowerCase().includes(q));
      if (!hit) continue;
    }
    rows.push([e, hit]);
  }
  if (!rows.length) return h('div', { class: 'empty', text: cat.programs.length ? 'Nothing matches.' : `${p.title} lists no programs for this account yet.` });
  const shown = rows.slice(0, 200).map(([e, hit]) => {
    const x = e.program;
    const inScope = x.assets.filter((a) => a.in_scope);
    const r = x.rules;
    return h(
      'div',
      { class: 'progrow progcatrow', onclick: () => reviewDraft(structuredClone(x)) },
      h(
        'div',
        { class: 'pcmain' },
        h('div', { class: 'pcname' }, h('b', { text: x.name }), x.bounty ? h('span', { class: 'tag in', text: 'bounty' }) : h('span', { class: 'tag out', text: 'no bounty' }), e.state && e.state !== 'open' ? h('span', { class: 'tag rej', text: e.state.replace(/_/g, ' ') }) : null),
        h('div', { class: 'muted small', text: hit ? `Has ${hit.identifier}` : inScope.length ? `${plural(inScope.length, 'asset')} in scope: ${kindCounts(inScope)}` : 'No assets in scope listed' }),
      ),
      h(
        'div',
        { class: 'pcrules' },
        r.rate_per_second ? h('span', { class: 'chip', title: 'Rate limit from the policy', text: shortRate(r.rate_per_second) }) : null,
        r.headers && r.headers.length ? h('span', { class: 'chip', title: r.headers.map((x) => `${x.name}: ${x.value}`).join('\n'), text: plural(r.headers.length, 'header') }) : null,
        r.no_automation ? h('span', { class: 'chip k-bad', title: 'The policy forbids automated testing', text: 'No automation' }) : null,
      ),
      h('button', { class: 'btn sm primary', text: 'Review', onclick: (ev) => (ev.stopPropagation(), reviewDraft(structuredClone(x))) }),
    );
  });
  if (rows.length > 200) shown.push(h('div', { class: 'muted small progpad', text: `Showing 200 of ${rows.length.toLocaleString()}. Search to narrow it down.` }));
  return shown;
}

function catalogAssets(p, cat) {
  const q = PG.filter.trim().toLowerCase();
  const rows = [];
  for (const e of catalogOrder(cat)) {
    if (PG.bountyOnly && !e.program.bounty) continue;
    for (const a of e.program.assets) {
      if (!a.in_scope || (PG.bountyOnly && !a.bounty)) continue;
      if (q && !a.identifier.toLowerCase().includes(q) && !e.program.name.toLowerCase().includes(q)) continue;
      rows.push([a, e]);
    }
  }
  if (!rows.length) return h('div', { class: 'empty', text: 'No in-scope asset matches.' });
  const table = h(
    'table',
    { class: 'grid' },
    h('thead', null, h('tr', null, h('th', { text: 'Asset' }), h('th', { text: 'Type' }), h('th', { text: 'Program' }), h('th', { text: 'Bounty' }), h('th'))),
    h(
      'tbody',
      null,
      rows.slice(0, 500).map(([a, e]) =>
        h(
          'tr',
          null,
          h('td', { class: 'mono', text: a.identifier, title: a.instruction || '' }),
          h('td', { text: KIND_LABEL[a.kind] || a.kind }),
          h('td', { text: e.program.name }),
          h('td', { class: 'muted', text: a.bounty ? (a.max_severity ? `up to ${a.max_severity}` : 'yes') : '' }),
          h('td', { class: 'pcact' }, h('button', { class: 'btn sm', text: 'Review', onclick: () => reviewDraft(structuredClone(e.program)) })),
        ),
      ),
    ),
  );
  return rows.length > 500 ? [table, h('div', { class: 'muted small progpad', text: `Showing 500 of ${rows.length.toLocaleString()}. Search to narrow it down.` })] : table;
}

function pasteForm() {
  const name = h('input', { type: 'text', id: 'pg-name', placeholder: 'Program name (optional)', spellcheck: 'false' });
  const text = h('textarea', { id: 'pg-text', rows: 10, placeholder: 'Paste the program’s policy or scope here, or its address (https://…). Plonix reads the in-scope and out-of-scope assets, the request rate, required headers and what is not allowed.', spellcheck: 'false' });
  const status = h('span', { class: 'fstatus' });
  const go = async () => {
    const v = text.value.trim();
    if (!v) return text.focus();
    const body = /^https?:\/\/\S+$/.test(v) ? { url: v, name: name.value } : { text: v, name: name.value };
    await readProgram(body, status);
  };
  return h('div', { class: 'progform' }, name, text, h('div', { class: 'progfoot' }, status, h('button', { class: 'btn primary', text: 'Read it', onclick: go })));
}

function domainForm() {
  const dom = h('input', { type: 'text', id: 'pg-domain', placeholder: 'example.com', spellcheck: 'false' });
  const status = h('span', { class: 'fstatus' });
  const go = async () => {
    if (!dom.value.trim()) return dom.focus();
    await readProgram({ domain: dom.value.trim() }, status);
  };
  dom.addEventListener('keydown', (e) => e.key === 'Enter' && go());
  return h(
    'div',
    { class: 'progform' },
    h('p', { class: 'muted', text: 'For a disclosure program without a platform: Plonix reads the domain’s security.txt and the policy it links to.' }),
    h('div', { class: 'addrule' }, dom, h('button', { class: 'btn primary', text: 'Look it up', onclick: go })),
    h('div', { class: 'progfoot' }, status),
  );
}

async function readProgram(body, status) {
  status.className = 'fstatus';
  status.textContent = 'Reading…';
  try {
    const r = await api('/api/program/read', { method: 'POST', body });
    status.textContent = '';
    await reviewDraft(r.program);
  } catch (e) {
    status.className = 'fstatus bad';
    status.textContent = e.message;
  }
}

async function reviewFromPlatform(platform, summary) {
  toast(`Reading ${summary.name}…`);
  try {
    const q = new URLSearchParams({ name: summary.name || '', bounty: summary.bounty ? 'true' : 'false' });
    const r = await api(`/api/platforms/${encodeURIComponent(platform)}/programs/${encodeURIComponent(summary.handle)}?${q}`);
    await reviewDraft(r.program);
  } catch (e) {
    toast(e.message, 'err');
  }
}

async function reviewDraft(program) {
  PG.draft = program;
  try {
    PG.preview = await api('/api/program/preview', { method: 'POST', body: { program } });
  } catch (e) {
    PG.preview = { program, scope: [], not_scoped: [], error: e.message };
  }
  drawPrograms();
}

const CHANGE_TAG = { add: ['in', 'new'], update: ['upd', 'changes'], same: ['out', 'already set'], remove: ['bad', 'removed'] };

function drawProgramReview(box) {
  const p = PG.draft;
  const pv = PG.preview;
  const r = p.rules;
  const accepted = pv.scope.filter((c) => c.decision === 'accepted' && c.change !== 'remove').length;
  const rejected = pv.scope.filter((c) => c.decision === 'rejected' && c.change !== 'remove').length;
  const summary = [`${accepted} in scope`, `${rejected} excluded`, r.rate_per_second ? fmtRate(r.rate_per_second) : 'no rate limit', `${(r.headers || []).length} required header${(r.headers || []).length === 1 ? '' : 's'}`];

  const rate = h('input', { type: 'number', id: 'pg-rate', min: '0', step: '0.1', value: r.rate_per_second || '', placeholder: 'no limit' });
  rate.addEventListener('change', () => {
    const n = parseFloat(rate.value);
    r.rate_per_second = n > 0 ? n : null;
    reviewDraft(p);
  });
  const toggle = (key, label, hint) => {
    const c = h('input', { type: 'checkbox', class: 'switch', id: 'pg-' + key, checked: !!r[key] });
    c.addEventListener('change', () => {
      r[key] = c.checked;
    });
    return h('label', { class: 'progtoggle' }, c, h('span', null, h('b', { text: label }), h('span', { class: 'muted', text: hint })));
  };
  const headerRows = (r.headers || []).map((hd, i) => {
    const v = h('input', { type: 'text', value: hd.value, spellcheck: 'false', class: hd.needs_value ? 'needs' : '' });
    v.addEventListener('input', () => {
      hd.value = v.value;
      hd.needs_value = /<[^>]*>|\[[^\]]*\]|\{[^}]*\}/.test(v.value);
      v.classList.toggle('needs', hd.needs_value);
    });
    return h(
      'div',
      { class: 'proghdr' },
      h('span', { class: 'mono', text: hd.name + ':' }),
      v,
      h('button', {
        class: 'btn sm',
        text: 'Remove',
        onclick: () => {
          r.headers.splice(i, 1);
          drawPrograms();
        },
      }),
    );
  });
  const addHeader = h('button', {
    class: 'btn sm',
    text: 'Add a header',
    onclick: () => {
      const name = h('input', { type: 'text', placeholder: 'X-Bug-Bounty', spellcheck: 'false' });
      const value = h('input', { type: 'text', placeholder: 'your username', spellcheck: 'false' });
      modal('Add a required header', h('div', { class: 'progform' }, name, value), [
        h('button', { class: 'btn', text: 'Cancel', onclick: closeModal }),
        h('button', {
          class: 'btn primary',
          text: 'Add',
          onclick: () => {
            if (!name.value.trim()) return name.focus();
            (r.headers = r.headers || []).push({ name: name.value.trim(), value: value.value.trim(), needs_value: false });
            closeModal();
            drawPrograms();
          },
        }),
      ]);
    },
  });
  const notAccepted = h('textarea', { id: 'pg-not', rows: 4, value: (r.not_accepted || []).join('\n'), spellcheck: 'false', placeholder: 'One per line, e.g. Missing security headers' });
  notAccepted.addEventListener('input', () => {
    r.not_accepted = notAccepted.value.split('\n').map((x) => x.trim()).filter(Boolean);
  });
  const status = h('span', { class: 'fstatus' + (pv.error ? ' bad' : ''), text: pv.error || '' });
  const pending = (r.headers || []).filter((x) => x.needs_value);
  const apply = async () => {
    status.className = 'fstatus';
    status.textContent = 'Applying…';
    try {
      await api('/api/program/apply', { method: 'POST', body: { program: p } });
      PG.preview = null;
      PG.draft = null;
      toast(`Now following ${p.name}`);
      await loadScope();
      loadPrograms();
    } catch (e) {
      status.className = 'fstatus bad';
      status.textContent = e.message;
    }
  };

  clear(
    box,
    h(
      'div',
      { class: 'card progreview' },
      h(
        'div',
        { class: 'top' },
        h('div', { class: 'ttl' }, h('span', { class: 'tag pend', text: 'Review' }), h('b', { text: p.name }), p.url ? h('a', { href: p.url, target: '_blank', rel: 'noopener', class: 'meta', text: p.url }) : null),
        h('span', { class: 'acts' }, h('button', { class: 'btn sm', text: 'Back', onclick: () => ((PG.preview = null), drawPrograms()) })),
      ),
      h('div', { class: 'progsum', text: summary.join(' · ') }),
      pv.replaces ? h('div', { class: 'prognote', text: `This project follows ${pv.replaces} now. Applying switches it to ${p.name} and replaces the scope rules ${pv.replaces} added.` }) : null,
    ),
    h('div', { class: 'sechead' }, h('h3', { text: 'Scope' })),
    h(
      'div',
      { class: 'card' },
      pv.scope.length
        ? h(
            'table',
            { class: 'grid' },
            h('thead', null, h('tr', null, h('th', { text: 'Domain' }), h('th', { text: 'Decision' }), h('th', { text: 'Change' }))),
            h(
              'tbody',
              null,
              pv.scope.map((c) =>
                h(
                  'tr',
                  null,
                  h('td', { class: 'mono', text: (c.include_subdomains ? '*.' : '') + c.pattern }),
                  h('td', null, h('span', { class: 'tag ' + scopeTag(c.decision), text: c.decision })),
                  h('td', null, h('span', { class: 'tag ' + CHANGE_TAG[c.change][0], text: CHANGE_TAG[c.change][1] })),
                ),
              ),
            ),
          )
        : h('div', { class: 'empty', text: 'Nothing here becomes a scope rule. Add the hosts you may test in Scope.' }),
      pv.not_scoped.length
        ? h('div', { class: 'progpad muted' }, h('b', { text: 'Not scope rules: ' }), pv.not_scoped.map((a) => `${a.identifier} (${KIND_LABEL[a.kind] || a.kind})`).join(', '), '. They stay listed on the program for reference.')
        : null,
    ),
    h('div', { class: 'sechead' }, h('h3', { text: 'Rules Plonix will follow' })),
    h(
      'div',
      { class: 'card progform' },
      h('label', { class: 'prograte' }, h('b', { text: 'Requests per second, at most' }), rate, h('span', { class: 'muted', text: 'Applies to everything Plonix sends: Bench, runs, scans and crawls.' })),
      h('div', { class: 'proghdrs' }, h('b', { text: 'Headers on every request' }), headerRows.length ? headerRows : h('span', { class: 'muted', text: 'None required.' }), h('div', null, addHeader)),
      pending.length ? h('div', { class: 'prognote', text: `Fill in ${pending.map((x) => x.name).join(', ')}: a header with a placeholder is not sent until it has a real value.` }) : null,
      toggle('no_automation', 'No automated testing', 'Scans, crawls and Bench runs are off for this project. Browsing and single Bench requests still work.'),
      toggle('no_intrusive', 'No disruptive tests', 'Intrusive scan checks are off and cannot be picked.'),
      h('label', { class: 'proglong' }, h('b', { text: 'Reports this program does not accept' }), notAccepted),
    ),
    h('div', { class: 'card progform' }, h('div', { class: 'progfoot' }, status, h('button', { class: 'btn', text: 'Cancel', onclick: () => ((PG.preview = null), drawPrograms()) }), h('button', { class: 'btn primary', text: PG.program && PG.program.platform === p.platform && PG.program.id === p.id ? 'Apply changes' : 'Follow this program', disabled: !!pv.error, onclick: apply }))),
  );
}

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
      h('button', { class: 'btn sm', text: 'Program rules', onclick: () => leaveTo('programs') }),
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
  const chosen = !SC.focus || cats === null ? sug.recommended : sug.recommended.filter(fits);
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
    h('p', { class: 'muted', text: 'Only accepted, in-scope hosts appear here.' }),
  );
  clear(box, scanFocusBanner(), targetCard, scanSuggestSection(), scanCrawlSection());
}

/** When a focus is set, a banner naming the one endpoint the scan is scoped to. */
function scanFocusBanner() {
  const f = SC.focus;
  if (!f) return null;
  return h(
    'div',
    { class: 'card scanfocus' },
    h('div', { class: 'sechead' }, h('h3', { text: 'Focused on one endpoint' }), h('span', { class: 'shacts' }, h('button', { class: 'btn sm', text: 'Scan the whole host', onclick: clearScanFocus }))),
    h('div', { class: 'focusrow' }, h('span', { class: 'focustag', text: f.method }), h('code', { class: 'focuspath', text: f.path }), f.label ? h('span', { class: 'focuscat', text: f.label }) : null),
    h('p', { class: 'muted focusnote', text: f.note || 'The scan is aimed at this endpoint. The checks below are pre-picked for it — review and run.' }),
  );
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
  const body = { host: SC.host, tactics, include_intrusive: SC.intrusive };
  if (SC.focus) body.endpoints = [{ method: SC.focus.method, path: SC.focus.path }];
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
   Agents: ask Claude about the project, pick up past conversations, and
   follow what agents read. Setup lives behind the Setup button.
   ====================================================================== */

// An agent counts as connected while its MCP server checks in (every 20 s).
const AGENT_LIVE_MS = 60000;
// Requests one agent makes this close together read as one burst in the feed.
const AGENT_BURST_MS = 120000;

const EXAMPLE_PROMPTS = [
  'Use Plonix to find in-scope API endpoints that returned errors, then read the most interesting request and tell me what stands out.',
  'Using Plonix, list every endpoint on the target that takes an id parameter and group them by host.',
  'Look at Plonix scope suggestions and explain which ones really belong to the target and why.',
  'Summarize my Plonix findings and point to the requests that prove each one.',
];

// Questions offered under the ask box when nothing has been typed yet.
const STARTER_QUESTIONS = [
  'What should I look at next in this project?',
  'Which endpoints take an id, and which look worth checking for access control?',
  'Summarize what this application does from its traffic.',
];

const AG = { chat: null, cv: null, setup: null, chats: [], policy: null, hits: [], skills: [], watch: null, inboxAll: false, lookAsked: 0 };

function renderAgents(main) {
  AG.cv = null;
  const setupBtn = h('button', { class: 'btn sm', id: 'agsetupbtn', text: 'Setup', onclick: () => toggleAgentSetup() });
  clear(
    main,
    h(
      'div',
      { class: 'view' },
      h('div', { class: 'toolbar' }, h('h2', { text: 'Agents' }), h('span', { class: 'hint', text: 'Ask Claude about this project, pick up past conversations, and see what agents looked at.' }), setupBtn),
      h(
        'div',
        { class: 'pane' },
        h(
          'div',
          { class: 'agws' },
          h('div', { class: 'agmain' }, h('div', { class: 'card aginbox', id: 'aginbox' }), h('div', { class: 'card agask', id: 'agask' }), h('div', { id: 'agbody' })),
          h('aside', { class: 'agside' }, h('div', { class: 'card agfeed', id: 'agfeed' }, h('div', { class: 'ab muted', text: 'Loading…' }))),
        ),
        h('div', { class: 'stack agsetup', id: 'agentsbody', hidden: true }),
      ),
    ),
  );
  drawAgentAsk();
  drawAgentBody();
  loadAgents(true);
}

function toggleAgentSetup(open) {
  AG.setup = open === undefined ? !AG.setup : open;
  const box = $('#agentsbody');
  const btn = $('#agsetupbtn');
  if (!box) return;
  box.hidden = !AG.setup;
  if (btn) btn.classList.toggle('on', !!AG.setup);
  if (AG.setup) {
    drawAgentSetup();
    box.scrollIntoView({ block: 'start', behavior: 'smooth' });
  }
}

/** Refreshes what changes on its own: who is connected, the feed, the conversations. */
async function loadAgents(first) {
  S.agentsAt = Date.now();
  let a, act, list, watch;
  try {
    [a, act, list, watch] = await Promise.all([api('/api/agents'), api('/api/agents/activity?limit=200'), api('/api/agents/chats'), api('/api/agents/watch')]);
  } catch (e) {
    return toast(e.message, 'err');
  }
  if (S.view !== 'agents') return;
  AG.policy = a;
  AG.hits = act.hits || [];
  AG.chats = list.chats || [];
  AG.watch = watch;
  if (first) {
    // Nothing has ever happened here: show how to get started.
    if (AG.setup === null) AG.setup = !AG.chats.length && !(a.clients || []).length && a.ask_in_app === false;
    toggleAgentSetup(AG.setup);
    drawAgentAsk();
    loadAgentStarters();
  } else if (AG.setup) drawAgentSetup();
  drawAgentInbox();
  drawAgentFeed();
  if (!AG.chat) drawAgentBody();
}

/* ---- the ask box ---- */

function drawAgentAsk() {
  const box = $('#agask');
  if (!box) return;
  const a = AG.policy;
  if (a && !a.enabled) {
    return clear(box, h('div', { class: 'ab' }, h('b', { text: 'Agent access is off.' }), ' Turn it on in Settings › AI agents to ask Claude about this project. ', h('button', { class: 'link', text: 'Open Settings', onclick: () => ((S.settingsSection = 'agents'), leaveTo('settings')) })));
  }
  if (a && a.ask_in_app === false) {
    return clear(
      box,
      h(
        'div',
        { class: 'ab' },
        h('b', { text: 'Claude Code is not installed on this Mac.' }),
        ' Install it from claude.com/claude-code to ask about this project right here. Other agents can still connect: ',
        h('button', { class: 'link', text: 'see Setup', onclick: () => toggleAgentSetup(true) }),
        '.',
      ),
    );
  }
  const q = h('textarea', { class: 'agq', id: 'agq', rows: 2, placeholder: 'Ask Claude about this project: what to look at next, what an endpoint does, which hosts belong to the target…' });
  const go = h('button', { class: 'btn primary ai', text: '✦ Ask', disabled: true });
  const send = () => {
    const t = q.value.trim();
    if (!t) return;
    q.value = '';
    go.disabled = true;
    startAgentChat(t, t);
  };
  q.addEventListener('input', () => (go.disabled = !q.value.trim()));
  q.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      send();
    }
  });
  go.onclick = send;
  clear(
    box,
    h('div', { class: 'agqrow' }, q, go),
    h('div', { class: 'agstarters', id: 'agstarters' }),
    h('div', { id: 'agarg' }),
    h('p', { class: 'muted fine agnote', text: 'Claude reads this project through Plonix, read-only. It can suggest; it cannot send requests or change anything.' }),
  );
  drawAgentStarters();
}

async function loadAgentStarters() {
  try {
    AG.skills = ((await api('/api/skills')).skills || []).filter((s) => s.available);
  } catch (_) {
    AG.skills = [];
  }
  drawAgentStarters();
}

/** One-click starts: skills (playbooks) and a few plain questions. */
function drawAgentStarters() {
  const box = $('#agstarters');
  if (!box) return;
  clear(
    box,
    AG.skills.map((sk) => h('button', { class: 'chip k-ai', title: sk.description, onclick: () => runSkill(sk) }, h('span', { text: '✦ ' + sk.title }))),
    STARTER_QUESTIONS.map((t) => h('button', { class: 'chip', title: 'Ask this', onclick: () => startAgentChat(t, t) }, h('span', { text: t }))),
  );
}

/** Runs a skill; one that needs something (a host, a request id) asks for it first. */
async function runSkill(sk, values = {}) {
  const need = (sk.arguments || []).filter((x) => x.required && !values[x.name]);
  const slot = $('#agarg');
  if (need.length) {
    if (!slot) return;
    const hosts = ((S.facets && S.facets.hosts) || []).map((x) => x.value);
    const inputs = need.map((x) => {
      const guess = x.name === 'host' ? (M.sel && hosts.includes(M.sel) ? M.sel : hosts[0] || '') : x.name === 'id' && T.sel ? String(T.sel) : '';
      return { x, el: h('input', { value: guess, placeholder: x.description, list: x.name === 'host' ? 'aghosts' : null, spellcheck: 'false' }) };
    });
    const ok = () => {
      const v = { ...values };
      for (const { x, el } of inputs) v[x.name] = el.value.trim();
      if (inputs.some(({ x }) => !v[x.name])) return;
      clear(slot);
      runSkill(sk, v);
    };
    for (const { el } of inputs) el.addEventListener('keydown', (e) => e.key === 'Enter' && ok());
    clear(
      slot,
      h(
        'div',
        { class: 'agargrow' },
        h('b', { text: sk.title }),
        inputs.map(({ x, el }) => h('label', null, h('span', { text: x.name }), el)),
        h('datalist', { id: 'aghosts' }, hosts.map((v) => h('option', { value: v }))),
        h('button', { class: 'btn primary sm', text: 'Run', onclick: ok }),
        h('button', { class: 'btn sm ghost', text: 'Cancel', onclick: () => clear(slot) }),
      ),
    );
    inputs[0].el.focus();
    return;
  }
  let r;
  try {
    r = await api('/api/skills/' + encodeURIComponent(sk.name) + '?' + new URLSearchParams(values));
  } catch (e) {
    return toast(e.message, 'err');
  }
  if (!r.prompt) return toast(r.needs || 'This skill needs more to go on.', 'err');
  const detail = Object.values(values).join(', ');
  startAgentChat(r.prompt, sk.title + (detail ? ': ' + detail : ''));
}

/* ---- the inbox: what the background watcher found ---- */

const INBOX_SHOW = 8;
const ITEM_KIND = { lead: { label: 'Lead', cls: 'lead' }, note: { label: 'Note', cls: 'note' }, digest: { label: 'New', cls: 'digest' } };
const NEXT_ACTION = {
  lens: 'Open request',
  bench: 'Open in Bench',
  scan: 'Scan this host',
  access: 'Check as other users',
  finding: 'Write it up',
  map: 'Open in Map',
  scope: 'Open Scope',
};

/** Acts on an inbox item where it points: the request, the Bench, Scans, Access or a new finding. */
async function actOnItem(it) {
  markItems([it.id], false);
  const id = it.request;
  const next = it.next || 'lens';
  if (next === 'map' || next === 'scope') {
    if (next === 'map' && it.host) M.sel = it.host;
    return leaveTo(next);
  }
  if (!id) return;
  if (next === 'bench') return sendToBench(id);
  if (next === 'finding') return newFinding([id], it.title);
  if (next === 'access' && toolOn('access-check')) return startAccessCheck({ targets: [id], sourceLabel: it.title });
  if (next === 'scan') {
    try {
      const ex = await api('/api/traffic/' + id);
      SC.host = ex.host;
      return leaveTo('scans');
    } catch (_) {}
  }
  showExchange(id);
}

async function markItems(ids, dismiss) {
  try {
    const r = await api('/api/agents/watch/items', { method: 'POST', body: { ids, dismiss } });
    if (S.status) S.status.agent_inbox_unread = r.unread;
    updateChrome();
  } catch (e) {
    return toast(e.message, 'err');
  }
  if (dismiss || !ids.length) loadAgents();
}

async function saveWatch(patch) {
  try {
    AG.watch = await api('/api/agents/watch', { method: 'PUT', body: { ...AG.watch.settings, ...patch } });
  } catch (e) {
    return toast(e.message, 'err');
  }
  drawAgentInbox();
}

async function lookNow() {
  try {
    await api('/api/agents/watch/look', { method: 'POST' });
  } catch (e) {
    return toast(e.message, 'err');
  }
  AG.lookAsked = Date.now();
  drawAgentInbox();
  toast('Claude will look at your latest traffic in a moment', 'ok');
}

function drawAgentInbox() {
  const box = $('#aginbox');
  const w = AG.watch;
  if (!box || !w) return;
  const a = AG.policy;
  const st = w.settings;
  const items = w.items || [];
  // Asked to look: show it as looking until the look starts and ends (or it gives up).
  const asked = AG.lookAsked && Date.now() - AG.lookAsked < 30000 && !(w.state.last_look_at >= AG.lookAsked);
  const looking = w.running || asked;
  const used = w.state.tokens_today || 0;
  const pct = Math.min(100, Math.round((used / st.daily_tokens) * 100));
  const status = !st.enabled
    ? 'Off'
    : w.capped
      ? 'Paused until tomorrow: today’s tokens are used up'
      : looking
        ? 'Looking at your traffic…'
        : w.waiting
          ? `${w.waiting} new request${w.waiting === 1 ? '' : 's'} waiting; Claude looks when you pause`
          : 'Watching your traffic';
  const toggle = h('input', { type: 'checkbox', checked: st.enabled, onchange: () => saveWatch({ enabled: toggle.checked }) });
  const budget = h(
    'select',
    { title: 'Most tokens Claude may use per day watching this project', onchange: (e) => saveWatch({ daily_tokens: Number(e.target.value) }) },
    w.budgets.map((n) => h('option', { value: n, text: fmtTok(n) + ' a day', selected: n === st.daily_tokens })),
  );
  const head = h(
    'div',
    { class: 'aghead' },
    h('b', { text: 'From Claude' }),
    w.unread ? h('span', { class: 'qn', text: w.unread }) : null,
    h('span', { class: 'agstat' + (looking ? ' busy' : ''), text: status }),
    h('span', { class: 'sp' }),
    w.unread ? h('button', { class: 'btn sm ghost', text: 'Mark all read', onclick: () => markItems([], false) }) : null,
    h('button', { class: 'btn sm', text: looking ? 'Looking…' : 'Look now', disabled: looking || !a || !a.enabled || a.ask_in_app === false, title: 'Have Claude look at the latest in-scope traffic now', onclick: lookNow }),
    h('label', { class: 'agswitch', title: 'Let Claude read new in-scope traffic in the background and leave notes and leads here' }, toggle, h('span', { text: 'Watch my traffic' })),
  );
  let body;
  if (!items.length && !st.enabled) {
    body = h(
      'div',
      { class: 'agempty' },
      h('div', { class: 'agbig', text: 'Let Claude watch while you browse' }),
      h('p', { text: 'When you pause, Claude reads the new in-scope traffic and leaves notes and leads here: what stands out, what to check next, and where. It only reads. Nothing is sent unless you click.' }),
      h('div', { class: 'agrowbtns' }, h('button', { class: 'btn primary', text: 'Turn on', onclick: () => saveWatch({ enabled: true }) }), h('span', { class: 'muted fine', text: 'Up to ' + fmtTok(st.daily_tokens) + ' tokens a day. You can change this or turn it off any time.' })),
    );
  } else if (!items.length) {
    body = h('div', { class: 'ab muted', text: w.state.last_error || 'Nothing yet. Browse the target; when you pause, Claude looks at what is new. Or press Look now.' });
  } else {
    const shown = AG.inboxAll ? items : items.slice(0, INBOX_SHOW);
    body = h(
      'div',
      { class: 'aglist' },
      shown.map((it) => {
        const k = ITEM_KIND[it.kind] || ITEM_KIND.note;
        // Access needs its Market tool; without it the lead opens the request.
        const next = it.next === 'access' && !toolOn('access-check') ? 'lens' : it.next || 'lens';
        const act = it.kind === 'digest' ? null : NEXT_ACTION[next];
        return h(
          'div',
          { class: 'agitem' + (it.read ? '' : ' unread') },
          h('span', { class: 'agkind ' + k.cls, text: k.label }),
          h(
            'div',
            { class: 'agimain' },
            h('div', { class: 'agititle', text: it.title }),
            it.detail ? h('div', { class: 'agidetail', text: it.detail }) : null,
            h(
              'div',
              { class: 'agimeta' },
              it.request ? h('button', { class: 'link', text: 'Request #' + it.request, onclick: () => (markItems([it.id], false), showExchange(it.request)) }) : null,
              h('span', { text: agoText(it.at) }),
            ),
          ),
          h(
            'div',
            { class: 'agiacts' },
            act && (it.request || it.next === 'map' || it.next === 'scope') ? h('button', { class: 'btn sm' + (it.kind === 'lead' ? ' primary' : ''), text: act, onclick: () => actOnItem(it) }) : null,
            !it.read ? h('button', { class: 'mini', text: '✓', title: 'Mark read', onclick: () => markItems([it.id], false) }) : null,
            h('button', { class: 'mini', text: '✕', title: it.kind === 'digest' ? 'Remove' : 'Dismiss: not useful. Claude will suggest fewer like it.', onclick: () => markItems([it.id], true) }),
          ),
        );
      }),
      items.length > INBOX_SHOW
        ? h('button', { class: 'link agmore', text: AG.inboxAll ? 'Show fewer' : `Show all ${items.length}`, onclick: () => ((AG.inboxAll = !AG.inboxAll), drawAgentInbox()) })
        : null,
    );
  }
  clear(
    box,
    head,
    body,
    st.enabled || used
      ? h(
          'div',
          { class: 'agfoot' },
          h('div', { class: 'askmeter' }, h('div', { class: 'bar' + (w.capped ? ' over' : '') }, h('i', { style: { width: pct + '%' } })), h('span', { text: `${fmtTok(used)} of ${fmtTok(st.daily_tokens)} tokens today` + (w.state.looks_today ? ` · ${w.state.looks_today} look${w.state.looks_today === 1 ? '' : 's'}` : '') })),
          h('span', { class: 'sp' }),
          budget,
        )
      : null,
  );
}

/* ---- conversations ---- */

/** Starts a new conversation and opens it. */
function startAgentChat(prompt, ask) {
  AG.chat = 'new';
  const cv = openAgentChat(null, ask);
  cv.start(prompt, ask);
}

/** Shows one conversation in place of the list: a saved one, or a new one (titled `ask`) when `chat` is null. */
function openAgentChat(chat, ask) {
  const body = $('#agbody');
  const stopBtn = h('button', { class: 'btn sm', text: 'Stop', hidden: true });
  const delBtn = h('button', { class: 'btn sm ghost', text: 'Delete', hidden: !chat });
  const title = h('b', { class: 'agctitle', text: chat ? chat.title : (ask || 'New conversation').split('\n')[0] });
  const sub = chat && chat.subject ? h('button', { class: 'chip', title: 'Open it', onclick: () => openSubject(chat.subject) }, h('span', { text: subjectLabel(chat.subject) })) : null;
  const cv = claudeConvo({
    tall: true,
    onRunning: (on) => (stopBtn.hidden = !on),
    onChat: (id) => {
      AG.chat = id;
      delBtn.hidden = false;
    },
    onDone: () => loadAgents(),
  });
  AG.cv = cv;
  stopBtn.onclick = () => cv.stop();
  delBtn.onclick = async () => {
    if (cv.isRunning() || !cv.chat()) return;
    try {
      await api('/api/agents/chats/' + encodeURIComponent(cv.chat()), { method: 'DELETE' });
    } catch (e) {
      return toast(e.message, 'err');
    }
    closeAgentChat();
  };
  clear(
    body,
    h(
      'div',
      { class: 'card agchat' },
      h('div', { class: 'agchead' }, h('button', { class: 'btn sm ghost', text: '← Conversations', onclick: closeAgentChat }), title, sub, h('span', { class: 'sp' }), stopBtn, delBtn),
      h('div', { class: 'agcbody' }, cv.el),
    ),
  );
  if (chat) cv.load(chat);
  return cv;
}

function closeAgentChat() {
  AG.chat = null;
  AG.cv = null;
  drawAgentBody();
  loadAgents();
}

async function showAgentChat(id) {
  let chat;
  try {
    chat = await api('/api/agents/chats/' + encodeURIComponent(id));
  } catch (e) {
    return toast(e.message, 'err');
  }
  if (S.view !== 'agents') return;
  AG.chat = id;
  openAgentChat(chat).focus();
}

/** The conversation list, shown while no conversation is open. */
function drawAgentBody() {
  const body = $('#agbody');
  if (!body || AG.chat) return;
  const del = async (e, c) => {
    e.stopPropagation();
    try {
      await api('/api/agents/chats/' + encodeURIComponent(c.id), { method: 'DELETE' });
    } catch (err) {
      return toast(err.message, 'err');
    }
    loadAgents();
  };
  clear(
    body,
    h('div', { class: 'sechead' }, h('h3', { text: 'Conversations' }), AG.chats.length ? h('span', { class: 'muted fine', text: String(AG.chats.length) }) : null),
    h(
      'div',
      { class: 'card aglist' },
      AG.chats.length
        ? AG.chats.map((c) =>
            h(
              'div',
              { class: 'agrow', role: 'button', tabindex: 0, onclick: () => showAgentChat(c.id), onkeydown: (e) => e.key === 'Enter' && showAgentChat(c.id) },
              h(
                'div',
                { class: 'agrmain' },
                h('div', { class: 'agrtop' }, h('b', { text: c.title }), c.subject ? h('span', { class: 'tag', text: subjectLabel(c.subject) }) : null, c.running ? h('span', { class: 'tag in', text: 'answering' }) : null),
                c.preview ? h('div', { class: 'agrprev', text: c.preview }) : null,
              ),
              h('span', { class: 'agrmeta', text: (c.turns > 1 ? c.turns + ' questions · ' : '') + agoText(c.updated_at) }),
              h('button', { class: 'mini', text: '✕', title: 'Delete this conversation', onclick: (e) => del(e, c) }),
            ),
          )
        : h('div', { class: 'ab muted', text: 'Questions you ask here, or with ✦ Ask Claude anywhere in Plonix, are kept here so you can pick them up again later.' }),
    ),
  );
}

/* ---- the activity feed ---- */

/** What one agent request was, in words, and where it leads in the app. */
function hitInfo(x) {
  const p = x.path;
  const q = new URLSearchParams(x.query || '');
  const host = (m) => decodeURIComponent(m);
  const toMap = (hst) => () => {
    if (hst) M.sel = hst;
    leaveTo('map');
  };
  let m;
  if (p === '/api/traffic') {
    const t = q.get('q') || '';
    return { ico: '⌕', text: t ? 'Searched traffic for ' + t : 'Listed recent traffic', go: () => setQuery(t) };
  }
  if ((m = p.match(/^\/api\/traffic\/(\d+)(?:\/(insights|messages))?$/))) {
    const what = m[2] === 'insights' ? 'Spotted values in request #' : m[2] === 'messages' ? 'WebSocket messages of request #' : 'Request #';
    return { ico: '⇅', text: what + m[1], go: () => showExchange(Number(m[1])) };
  }
  if (p === '/api/hosts') return { ico: '⊞', text: 'Hosts seen', go: toMap() };
  if ((m = p.match(/^\/api\/hosts\/([^/]+)\/endpoints$/))) return { ico: '⊞', text: 'Endpoints on ' + host(m[1]), go: toMap(host(m[1])) };
  if (p === '/api/tech') return { ico: '⊞', text: 'Technologies on every host', go: toMap() };
  if ((m = p.match(/^\/api\/tech\/([^/]+)$/))) return { ico: '⊞', text: 'Technologies on ' + host(m[1]), go: toMap(host(m[1])) };
  if (p === '/api/scope') return { ico: '◉', text: 'Scope and suggested domains', go: () => leaveTo('scope') };
  if (p === '/api/findings') return { ico: '⚑', text: 'Findings', go: () => leaveTo('findings') };
  if (p === '/api/findings/export') return { ico: '⚑', text: 'Findings report', go: () => leaveTo('findings') };
  if ((m = p.match(/^\/api\/findings\/(\d+)$/))) return { ico: '⚑', text: 'Finding #' + m[1], go: () => leaveTo('findings') };
  if (p === '/api/scan/catalog') return { ico: '◎', text: 'Available scan checks', go: () => leaveTo('scans') };
  if ((m = p.match(/^\/api\/scan\/(?:suggest|plan)\/([^/]+)$/))) return { ico: '◎', text: 'Scan plan for ' + host(m[1]), go: () => leaveTo('scans') };
  if (p === '/api/bench/proposals' && x.method === 'POST') return { ico: '⎇', text: 'Suggested an edit on the Bench', go: () => leaveTo('bench') };
  if (p === '/api/status') return { ico: '•', text: 'Engine status' };
  return { ico: '•', text: x.method + ' ' + p };
}

/** Who made a burst of requests: an Ask Claude conversation by its title, else the agent's name. */
function hitWho(client) {
  const [name, run] = client.split('/');
  if (name === 'plonix-watch') return { label: 'Watching your traffic', ask: true };
  if (name !== 'plonix-ask') return { label: name };
  const chat = run && AG.chats.find((c) => (c.runs || []).includes(run));
  return chat ? { label: chat.title, chat: chat.id, ask: true } : { label: 'Ask Claude', ask: true };
}

function drawAgentFeed() {
  const box = $('#agfeed');
  const a = AG.policy;
  if (!box || !a) return;
  const now = Date.now();
  const live = (a.clients || []).filter((c) => now - c.last_seen < AGENT_LIVE_MS && !c.name.startsWith('plonix-'));
  // Consecutive requests from one agent, close together, form one burst.
  const bursts = [];
  for (const x of AG.hits) {
    const b = bursts[bursts.length - 1];
    if (b && b.client === x.client && b.last - x.at < AGENT_BURST_MS) {
      b.hits.push(x);
      b.last = x.at;
    } else bursts.push({ client: x.client, at: x.at, last: x.at, hits: [x] });
  }
  const SHOW = 6;
  clear(
    box,
    h(
      'div',
      { class: 'agfhead' },
      h('span', { class: 'adot' + (live.length ? ' on' : '') }),
      h('b', { text: 'Activity' }),
      h('span', { class: 'muted fine', text: live.length ? (live.length === 1 ? live[0].name + ' connected' : live.length + ' agents connected') : '' }),
      h('span', { class: 'mode', text: 'Read-only', title: 'Agents can look, never send or change. See Setup.' }),
    ),
    bursts.length
      ? h(
          'div',
          { class: 'agfbody' },
          bursts.slice(0, 30).map((b) => {
            const who = hitWho(b.client);
            const items = b.hits.map((x) => {
              const info = hitInfo(x);
              return h(
                info.go ? 'button' : 'div',
                { class: 'agfhit' + (x.refused ? ' refused' : ''), title: x.method + ' ' + x.path + (x.query ? '?' + x.query : ''), onclick: info.go || null },
                h('span', { class: 'ico', text: info.ico }),
                h('span', { class: 'tx', text: (x.refused ? 'Refused: ' : '') + info.text }),
              );
            });
            const more = items.length - SHOW;
            const rest = more > 0 ? items.slice(SHOW) : [];
            for (const r of rest) r.hidden = true;
            return h(
              'div',
              { class: 'agfburst' },
              h(
                'div',
                { class: 'agfwho' },
                who.chat ? h('button', { class: 'link', text: '✦ ' + who.label, title: 'Open this conversation', onclick: () => showAgentChat(who.chat) }) : h('b', { text: (who.ask ? '✦ ' : '') + who.label }),
                h('span', { class: 'muted', text: agoText(b.at) }),
              ),
              items.slice(0, SHOW),
              rest,
              more > 0
                ? h('button', {
                    class: 'link agfmore',
                    text: `+${more} more`,
                    onclick: (e) => {
                      for (const r of rest) r.hidden = false;
                      e.target.remove();
                    },
                  })
                : null,
            );
          }),
        )
      : h('div', { class: 'ab muted', text: 'When Claude or another agent reads this project, each thing it looks at shows up here, so you can follow along and open it yourself.' }),
  );
}

/* ---- setup: connecting agents, what they may do, skills ---- */

function drawAgentSetup() {
  const box = $('#agentsbody');
  const a = AG.policy;
  if (!box || !a) return;
  const now = Date.now();
  const clients = a.clients || [];
  const live = clients.filter((c) => now - c.last_seen < AGENT_LIVE_MS);
  const cmd = (a.connect && a.connect.command) || 'plonix connect claude';
  // The settings card keeps its own state across the status refreshes.
  let settingsCard = $('#agentsettings');
  const fresh = !settingsCard;
  if (fresh) settingsCard = h('div', { class: 'card', id: 'agentsettings' });
  clear(
    box,
    h('div', { class: 'sechead' }, h('h3', { text: 'Setup' }), h('span', { class: 'shacts' }, h('button', { class: 'btn sm ghost', text: 'Hide', onclick: () => toggleAgentSetup(false) }))),
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
                h('td', { class: 'mono', text: c.name === 'plonix-ask' ? 'Ask Claude (in Plonix)' : c.name === 'plonix-watch' ? 'Watcher (in Plonix)' : c.name }),
                h('td', { text: now - c.last_seen < AGENT_LIVE_MS ? 'connected' : 'last seen ' + agoText(c.last_seen) }),
                h('td', { text: c.requests + (c.refused ? ` (${c.refused} refused)` : '') }),
                h('td', { class: 'mono muted', text: c.last_request }),
              ),
            ),
          )
        : h('div', { class: 'ab muted', text: 'When an agent starts the Plonix MCP server it shows up here, with every request it makes.' }),
    ),
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
    h('div', { class: 'sechead' }, h('h3', { text: 'Try asking from your terminal' })),
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
  platform: { label: 'Platforms', one: 'Platform', ico: '⚐' },
};

const GROUP_LABELS = { traffic: 'Traffic', insights: 'Insights', map: 'Map', scope: 'Scope', findings: 'Findings', scan: 'Scans' };
const groupLabel = (g) => GROUP_LABELS[g] || g;

const MK = { data: null, kind: 'all', q: '', sel: null, busy: null, ext: {}, rec: null, peek: '' };

function renderMarket(main) {
  const q = h('input', {
    id: 'mq',
    placeholder: 'Search skills, rules, filters, platforms, bundles and extensions',
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
      h('div', { class: 'mbody', id: 'mbody' }, h('div', { class: 'pane' }, h('div', { class: 'mrec', id: 'mrec', hidden: true }), h('div', { class: 'mgrid', id: 'mgrid' }, h('div', { class: 'muted', text: 'Loading the Market…' }))), h('div', { id: 'mdetail' })),
    ),
  );
  loadMarket(false);
}

async function loadMarket(refresh) {
  try {
    MK.data = await api('/api/market' + (refresh ? '?refresh=true' : ''));
    const ex = await api('/api/extensions').catch(() => ({ extensions: [] }));
    MK.ext = Object.fromEntries((ex.extensions || []).map((x) => [x.name, x]));
    MK.rec = await api('/api/market/recommended' + (MK.peek ? '?profile=' + encodeURIComponent(MK.peek) : '')).catch(() => null);
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
  drawRecommended();
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

/** "Recommended for you": Market items that suit the user's kind of work, with why. */
function drawRecommended() {
  const box = $('#mrec');
  const r = MK.rec;
  if (!box) return;
  box.hidden = !r || MK.kind !== 'all' || !!MK.q.trim();
  if (box.hidden) return;
  const pick = (id) => {
    MK.peek = '';
    api('/api/market/profile', { method: 'POST', body: { profile: id } })
      .then(() => loadMarket(false))
      .catch((e) => toast(e.message, 'err'));
  };
  if (!r.shown) {
    return clear(
      box,
      h('div', { class: 'mrech' }, h('b', { text: 'Get suggestions for your work' }), h('span', { class: 'muted', text: 'Plonix picks Market items that suit it, and says why.' })),
      h('div', { class: 'mrecprofiles' }, r.profiles.map((p) => h('button', { class: 'chip', title: p.line, text: p.title, onclick: () => pick(p.id) }))),
    );
  }
  const select = h(
    'select',
    {
      id: 'mrecprofile',
      class: 'mrecsel',
      title: 'Your work',
      onchange: (e) => {
        if (e.target.value === r.profile) MK.peek = '';
        else MK.peek = e.target.value;
        loadMarket(false);
      },
    },
    r.profiles.map((p) => h('option', { value: p.id, text: p.title + (p.id === r.profile ? ' (yours)' : ''), selected: p.id === r.shown.id })),
  );
  const rec = r.recommendation || { starter: [], included: [], also: [] };
  const byName = Object.fromEntries((MK.data ? MK.data.packages : []).map((p) => [p.name, p]));
  const card = (k) => {
    const p = byName[k.name];
    if (!p) return null;
    const st = marketStatus(p);
    const ki = KIND_INFO[p.kind] || { one: p.kind, ico: '•' };
    return h(
      'div',
      { class: 'mpkg mrecpkg', tabindex: 0, onclick: () => showPackage(p.name), onkeydown: (e) => e.key === 'Enter' && showPackage(p.name) },
      h('div', { class: 'mph' }, h('span', { class: 'mico k-' + p.kind, text: ki.ico }), h('div', { class: 'mpn' }, h('b', { text: p.name }), h('span', { class: 'muted', text: ki.one })), h('span', { class: st.cls, text: st.text })),
      h('div', { class: 'mpd', text: k.why }),
      h('div', { class: 'mpf' }, k.noise !== 'passive' ? h('span', { class: 'muted small', text: k.noise === 'active' ? 'Sends many requests' : 'Sends a few requests' }) : h('span'), st.action ? marketButton(p, st.action, true) : null),
    );
  };
  const peeking = r.shown.id !== r.profile;
  clear(
    box,
    h(
      'div',
      { class: 'mrech' },
      h('b', { text: 'Recommended for you' }),
      select,
      peeking ? h('button', { class: 'btn sm ghost', text: 'Make this my work', onclick: () => pick(r.shown.id) }) : null,
      h('span', { class: 'muted mrecline', text: r.shown.line }),
    ),
    rec.starter.length
      ? h('div', { class: 'mgrid mrecgrid' }, rec.starter.map(card))
      : h('div', { class: 'muted mrecdone', text: 'You have everything picked for this work. More shows up here as the Market grows.' }),
    rec.included.length || rec.also.length
      ? h(
          'div',
          { class: 'mrecmore muted' },
          rec.included.length ? h('span', null, 'You already have ', rec.included.map((k, i) => [i ? ', ' : '', h('a', { href: '#', text: k.name, onclick: (e) => (e.preventDefault(), showPackage(k.name)) })]), '. ') : null,
          rec.also.length ? h('span', null, 'Also for you: ', rec.also.map((k, i) => [i ? ', ' : '', h('a', { href: '#', text: k.name, title: k.why, onclick: (e) => (e.preventDefault(), showPackage(k.name)) })]), '.') : null,
        )
      : null,
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
  // The sha256 of the file the preview showed: confirming adds only that file.
  let shown = null;
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
      const r = await api('/api/market/add', { method: 'POST', body: { source, confirm, grant, sha256: confirm ? shown : undefined } });
      if (r.added) {
        closeModal();
        toast(`Added ${r.file.name} (not verified)`, 'ok');
        MK.kind = 'unverified';
        await loadMarket(true);
        return showPackage(r.file.name);
      }
      const f = r.file;
      shown = f.sha256;
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
  input.addEventListener('input', () => ((confirmBtn.hidden = true), (shown = null), clear(preview)));
  input.addEventListener('keydown', (e) => e.key === 'Enter' && run(false));
}

async function updateAll() {
  try {
    const r = await api('/api/market/update', { method: 'POST', body: {} });
    const failed = r.failed || [];
    const done = `Updated ${(r.changes || []).length} package(s)`;
    if (failed.length) toast(`${done}. Not updated: ${failed.map((f) => `${f.name} (${f.error})`).join('; ')}`, 'err');
    else toast(done, 'ok');
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
  if (det.platform)
    parts.push(
      sec(
        'Programs',
        h('p', { class: 'muted', text: `Lists your ${det.platform.title} programs on the Programs screen and brings one in with its scope and rules. Plonix only talks to ${det.platform.api}, with the token you give it.` }),
        h('button', { class: 'btn sm', text: 'Open Programs', onclick: () => leaveTo('programs') }),
      ),
    );
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
 * A live in-app conversation with Claude Code: the transcript, the progress
 * line while Claude works, and the follow-up box. Used by the Ask sheet and
 * the Agents screen. Turns started with `ask` are saved as a chat the Agents
 * screen lists; follow-ups go into the same chat and resume its session.
 *
 * Hooks in `o`: onRunning(on) when a turn starts or ends, onError() when it
 * fails (to offer fallbacks), onDone({ proposed }) when a turn has finished,
 * onChat(id) when the turn was saved, and onStart() as a turn is sent.
 */
function claudeConvo(o = {}) {
  const convo = { id: null, since: 0, sessionId: null, running: false, proposed: false, chat: o.chat || null };
  const transcript = h('div', { class: 'convo' + (o.tall ? ' tall' : '') });
  const followIn = h('textarea', { class: 'cfollow', rows: 1, placeholder: 'Ask a follow-up…' });
  const sendBtn = h('button', { class: 'btn primary sm', text: 'Send' });
  const followRow = h('div', { class: 'cfollowrow', hidden: true }, followIn, sendBtn);

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
  const toolLine = (text) => h('div', { class: 'ctool', text: '✦ ' + text });

  const alive = () => document.body.contains(transcript);
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const running = (on) => {
    convo.running = on;
    followIn.disabled = sendBtn.disabled = on;
    if (o.onRunning) o.onRunning(on);
  };

  const pollLoop = async () => {
    while (convo.running && alive()) {
      let snap;
      try {
        snap = await api(`/api/agents/run/${convo.id}?since=${convo.since}`);
      } catch (e) {
        setThinking(false);
        sayError(e.message);
        running(false);
        break;
      }
      for (const ev of snap.events) {
        convo.since = ev.seq + 1;
        if (ev.type === 'text') {
          dropDraft();
          add(answer(ev.text));
        } else if (ev.type === 'tool') {
          add(toolLine(ev.text));
        } else if (ev.type === 'proposal') {
          add(toolLine(ev.text));
          convo.proposed = true;
        } else if (ev.type === 'error') {
          setThinking(false);
          sayError(ev.text);
          if (o.onError) o.onError();
        }
      }
      if (snap.session_id) convo.sessionId = snap.session_id;
      if (snap.status !== 'running') {
        setThinking(false);
        const p = snap.progress;
        if (snap.status === 'done' && p) add(h('div', { class: 'cdone', text: `Answered in ${claudeStats(p).replace(/^(.*) · ([\d:]+)$/, '$2 · $1')}` }));
        running(false);
        if (snap.status === 'done') followRow.hidden = false;
        const proposed = convo.proposed;
        convo.proposed = false;
        if (o.onDone) o.onDone({ proposed, ok: snap.status === 'done' });
        break;
      }
      // Keep the working indicator alive between turns.
      if (!thinking) setThinking(true);
      showProgress(snap.progress);
      await sleep(500);
    }
  };

  /** Sends one turn: `prompt` goes to Claude, `ask` (what the user typed) is saved with the chat. */
  const start = async (prompt, ask, extra = {}) => {
    convo.since = 0;
    convo.proposed = false;
    followRow.hidden = true;
    if (o.onStart) o.onStart();
    add(bubble('user', ask));
    running(true);
    setThinking(true);
    try {
      const body = { prompt, ask, ...extra };
      if (convo.chat) body.chat = convo.chat;
      else if (convo.sessionId) body.resume = convo.sessionId;
      const r = await api('/api/agents/run', { method: 'POST', body });
      convo.id = r.id;
      if (r.chat && r.chat !== convo.chat) {
        convo.chat = r.chat;
        if (o.onChat) o.onChat(r.chat);
      }
    } catch (e) {
      setThinking(false);
      running(false);
      sayError(e.message);
      if (o.onError) o.onError();
      return;
    }
    pollLoop();
  };

  /** Shows a saved chat's turns, ready for a follow-up. */
  const load = (chat) => {
    clear(transcript);
    convo.chat = chat.id;
    convo.sessionId = chat.session_id || null;
    for (const t of chat.turns || []) {
      add(bubble('user', t.ask));
      for (const tool of t.tools || []) add(toolLine(tool));
      if (t.answer) add(answer(t.answer));
      if (t.error) add(h('div', { class: 'cerr', text: t.error }));
      if (t.status === 'running') add(h('div', { class: 'cdone', text: 'Still answering. Open this conversation again in a moment to see the answer.' }));
    }
    const last = (chat.turns || [])[chat.turns.length - 1];
    followRow.hidden = !!(last && last.status === 'running');
  };

  const stop = async () => {
    if (!convo.id || !convo.running) return;
    running(false);
    setThinking(false);
    try {
      await api(`/api/agents/run/${convo.id}`, { method: 'DELETE' });
    } catch (_) {}
    sayError('Stopped.');
    if (o.onError) o.onError();
  };

  const reset = () => {
    convo.id = convo.sessionId = convo.chat = null;
    clear(transcript);
    followRow.hidden = true;
  };

  sendBtn.onclick = () => {
    const t = followIn.value.trim();
    if (!t || convo.running) return;
    followIn.value = '';
    start(t, t);
  };
  followIn.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      sendBtn.onclick();
    }
  });

  return { el: h('div', { class: 'convobox' }, transcript, followRow), transcript, add, start, load, stop, reset, isRunning: () => convo.running, chat: () => convo.chat, focus: () => followIn.focus() };
}

/** What an Ask Claude subject is about, in a few words: "Request #42", "api.example.com". */
function subjectLabel(sub) {
  if (!sub) return '';
  if (sub.kind === 'request') return 'Request #' + sub.id;
  if (sub.kind === 'finding') return 'Finding #' + sub.id;
  if (sub.kind === 'host') return sub.host;
  if (sub.kind === 'draft') return 'Bench request';
  return '';
}

/** The part of a subject worth keeping with a saved chat: enough to link back. */
function subjectRef(sub) {
  if (!sub) return null;
  if (sub.kind === 'request' || sub.kind === 'finding') return { kind: sub.kind, id: sub.id };
  if (sub.kind === 'host') return { kind: 'host', host: sub.host };
  if (sub.kind === 'draft') return { kind: 'draft' };
  return null;
}

/** Opens what a saved chat is about, where it lives in the app. */
function openSubject(sub) {
  if (!sub) return;
  if (sub.kind === 'request') showExchange(sub.id);
  else if (sub.kind === 'finding') leaveTo('findings');
  else if (sub.kind === 'host') {
    M.sel = sub.host;
    leaveTo('map');
  } else if (sub.kind === 'draft') leaveTo('bench');
}

/**
 * The Ask sheet: shows exactly what will be shared and how big it is, lets
 * the user edit the question, drop parts and shorten bodies, and asks for
 * an explicit confirmation when it is larger than their limit.
 */
async function askClaude(subject, opts = {}) {
  const st = { exclude: [], question: opts.question || null, max: null, bundle: null, confirmBig: false };

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

  /* ---- footer buttons ---- */
  const copyBtn = h('button', { class: 'btn', text: 'Copy prompt' });
  const termBtn = h('button', { class: 'btn', text: 'Open in Terminal' });
  const askBtn = h('button', { class: 'btn primary ai', text: '✦ Ask Claude' });
  const stopBtn = h('button', { class: 'btn', text: 'Stop', hidden: true });
  const newBtn = h('button', { class: 'btn', text: 'New question', hidden: true });

  /* ---- conversation view (the answer, in-app, saved on the Agents screen) ---- */
  const cv = claudeConvo({
    onStart: () => (m.err.textContent = ''),
    onRunning: (on) => (stopBtn.hidden = !on),
    onError: () => (copyBtn.hidden = termBtn.hidden = false),
    onDone: ({ proposed }) => proposed && offerReview(),
  });
  const savedNote = h('p', { class: 'muted fine', text: 'Saved in Agents, where you can pick this conversation up again later.' });
  const convoView = h('div', { hidden: true }, cv.el, savedNote);

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
        '. The limit is in Settings › AI agents.',
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

  // Claude suggested an edit to the Bench draft: point to the review there.
  // Nothing has changed yet; the Bench shows the diff with Apply and Discard.
  const offerReview = async () => {
    if (subject.kind !== 'draft' || !subject.draft_id) return;
    let list = [];
    try {
      list = (await api('/api/bench/proposals?draft=' + encodeURIComponent(subject.draft_id))).proposals || [];
    } catch (_) {}
    if (!list.length || !document.body.contains(cv.transcript)) return;
    cv.add(
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
    cv.start(st.bundle.prompt, st.bundle.question, { subject: subjectRef(subject) });
  };
  stopBtn.onclick = () => cv.stop();
  newBtn.onclick = () => {
    if (cv.isRunning()) return;
    cv.reset();
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
  data.sections = [appearanceSection(), ...(data.sections || [])];
  const host = h('div', { style: { flex: '1', minHeight: '0', display: 'flex' } });
  const back = backButton();
  clear(box, back ? h('div', { class: 'toolbar' }, back) : null, host);
  PlonixSettings.render(host, data, {
    select: S.settingsSection || 'proxy',
    onSelect: (id) => (S.settingsSection = id),
    save: async (section, values) => {
      if (section === 'appearance') {
        applyTheme(values.theme);
        store('plonix.theme', values.theme);
        applyDensity(values.density);
        store('plonix.density', values.density);
        return { applies: 'now' };
      }
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

/** How Plonix looks on this computer. Kept in the window, not the engine. */
function appearanceSection() {
  return {
    id: 'appearance',
    title: 'Appearance',
    level: 'device',
    description: 'How Plonix looks on this computer.',
    applies: 'now',
    fields: [
      { key: 'theme', label: 'Theme', type: 'choice', options: [{ value: 'auto', label: 'Match system' }, { value: 'light', label: 'Light' }, { value: 'dark', label: 'Dark' }] },
      { key: 'density', label: 'Spacing', type: 'choice', help: 'Dense fits more on screen. Roomy gives rows and panels more air.', options: [{ value: 'dense', label: 'Dense' }, { value: 'roomy', label: 'Roomy' }] },
    ],
    values: { theme: S.theme || 'auto', density: S.density || 'dense' },
  };
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

/* ---------- rules: change traffic as it passes ----------
   Plain header rules (add, change, remove) and text rules, each with where
   it applies (browser, Bench, Scans) and an optional condition written as a
   traffic search. The proxy and the engine apply them; nothing here sends. */

const REPLACE_TARGETS = [
  ['request_line', 'Request line'],
  ['request_header', 'Request headers'],
  ['request_body', 'Request body'],
  ['response_header', 'Response headers'],
  ['response_body', 'Response body'],
];

const RULE_KINDS = [
  ['add_header', 'Add header', 'Sends a header that isn’t there yet'],
  ['change_header', 'Change header', 'Gives a header a new value'],
  ['remove_header', 'Remove header', 'Takes a header out'],
  ['replace', 'Replace text', 'Finds text and puts other text in its place'],
];

/** The rules and the all-rules switch, as last loaded. */
const RL = { data: null };

async function loadRules() {
  try {
    RL.data = await api('/api/replace');
  } catch {
    RL.data = null;
  }
  drawRulesPill();
  drawRulesCount();
  return RL.data;
}

const activeRules = () => (RL.data && RL.data.enabled ? RL.data.rules.filter((r) => r.enabled) : []);

function drawRulesCount() {
  const ct = $('#ct-rules');
  if (!ct || !RL.data) return;
  const n = activeRules().length;
  ct.textContent = RL.data.enabled ? (n ? n + ' on' : '') : RL.data.rules.length ? 'paused' : '';
}

const ruleSide = (r) => (r.target.startsWith('response') ? 'response' : 'request');

/** One line that says what a rule does, in plain words. */
function ruleSummary(r) {
  const side = ruleSide(r) === 'response' ? ' (response)' : '';
  switch (r.kind) {
    case 'add_header':
      return { tag: 'Add header', cls: 'add', text: `${r.pattern}: ${r.replace}${side}` };
    case 'set_header':
      return { tag: 'Add header', cls: 'add', text: `${r.pattern}: ${r.replace}${side}`, note: 'replaces any value already there' };
    case 'change_header':
      return { tag: 'Change header', cls: 'chg', text: `${r.pattern} → ${r.replace}${side}` };
    case 'remove_header':
      return { tag: 'Remove header', cls: 'del', text: r.pattern + side };
    default: {
      const where = (REPLACE_TARGETS.find(([k]) => k === r.target) || [r.target, r.target])[1].toLowerCase();
      return { tag: 'Replace text', cls: 'txt', text: `${r.pattern} → ${r.replace === '' ? '(nothing)' : r.replace}`, note: where + (r.regex ? ', regex' : '') };
    }
  }
}

async function setRulesOn(on) {
  try {
    await api('/api/settings/replace', { method: 'PUT', body: { values: { enabled: on } } });
    toast(on ? 'Rules are on again.' : 'All rules are paused. Traffic passes unchanged.', 'ok');
  } catch (e) {
    toast(e.message, 'err');
  }
  await loadRules();
  if (S.view === 'rules') drawRules();
}

async function renderRules(main) {
  const toggle = h('label', { class: 'rltoggle', id: 'rltoggle' });
  clear(
    main,
    h(
      'div',
      { class: 'view rulesview' },
      h(
        'div',
        { class: 'toolbar' },
        backButton(),
        h('h2', { text: 'Rules' }),
        h('span', { class: 'muted', text: 'Change traffic as it passes. Applied top to bottom.' }),
        h('span', { class: 'spacer' }),
        toggle,
        h('button', { class: 'btn sm primary', text: '+ Add rule', onclick: () => ruleDialog() }),
      ),
      h('div', { class: 'rlbody', id: 'rlbody' }, h('div', { class: 'empty', text: 'Loading rules…' })),
    ),
  );
  await loadRules();
  drawRules();
}

function drawRules() {
  const body = $('#rlbody');
  if (!body) return;
  const d = RL.data;
  if (!d) return clear(body, h('div', { class: 'empty', text: 'The rules could not be loaded.' }));
  clear(
    $('#rltoggle'),
    h('input', { type: 'checkbox', class: 'switch', checked: d.enabled, onchange: (e) => setRulesOn(e.target.checked) }),
    d.enabled ? 'All rules on' : 'All rules paused',
  );
  if (!d.rules.length) {
    return clear(
      body,
      h(
        'div',
        { class: 'rlempty' },
        h('h3', { text: 'No rules yet' }),
        h('p', { class: 'muted', text: 'A rule changes traffic as it passes: send a header on every request, change one, remove one, or replace any text. Right-click a header in the Lens to make a rule from it.' }),
        h('button', { class: 'btn primary', text: '+ Add rule', onclick: () => ruleDialog() }),
      ),
    );
  }
  const reach = (r) =>
    [r.browser ? 'Browser' : null, r.bench ? 'Bench' : null, r.scans ? 'Scans' : null, r.in_scope_only ? 'In scope' : null]
      .filter(Boolean)
      .map((t) => h('span', { class: 'rlchip' + (t === 'In scope' ? ' scope' : ''), text: t }));
  const row = (r) => {
    const s = ruleSummary(r);
    return h(
      'div',
      { class: 'rlrow' + (r.enabled && d.enabled ? '' : ' off'), onclick: (e) => !e.target.closest('input, button') && ruleDialog(r) },
      h('input', {
        type: 'checkbox',
        class: 'switch sm',
        checked: r.enabled,
        title: r.enabled ? 'On: switch this rule off' : 'Off: switch this rule on',
        onchange: async (e) => {
          try {
            await api('/api/replace/' + r.id, { method: 'PATCH', body: { enabled: e.target.checked } });
          } catch (err) {
            toast(err.message, 'err');
          }
          await loadRules();
          drawRules();
        },
      }),
      h('span', { class: 'rlkind ' + s.cls, text: s.tag }),
      h(
        'span',
        { class: 'rlwhat' },
        h('span', { class: 'mono', text: s.text }),
        s.note ? h('span', { class: 'muted', text: s.note }) : null,
        r.when ? h('span', { class: 'rlwhen', title: 'Only when the exchange matches this search' }, 'only when ', h('code', { text: r.when })) : null,
        r.note ? h('span', { class: 'muted', text: '· ' + r.note }) : null,
      ),
      h('span', { class: 'rlreach' }, reach(r)),
      h('button', {
        class: 'iconbtn',
        text: '✕',
        title: 'Remove this rule',
        onclick: async () => {
          try {
            await api('/api/replace/' + r.id, { method: 'DELETE' });
          } catch (err) {
            toast(err.message, 'err');
          }
          await loadRules();
          drawRules();
        },
      }),
    );
  };
  clear(
    body,
    d.enabled ? null : h('div', { class: 'rlnote', text: 'All rules are paused: traffic passes unchanged until you switch them back on.' }),
    h('div', { class: 'rllist' }, d.rules.map(row)),
    h('p', { class: 'muted rltip', text: 'Click a rule to edit it. Right-click any header in the Lens to make a rule from it.' }),
  );
}

/**
 * The Add rule popup, empty, filled in from the Lens (`seed`), or for editing
 * an existing rule (one with an id). Nothing applies until Save.
 */
function ruleDialog(seed = {}) {
  const editing = seed.id != null;
  const st = {
    kind: seed.kind === 'set_header' ? 'add_header' : seed.kind || 'add_header',
    side: seed.target && seed.target.startsWith('response') ? 'response' : 'request',
    target: seed.target && seed.kind === 'replace' ? seed.target : 'request_header',
    browser: seed.browser != null ? seed.browser : true,
    bench: seed.bench != null ? seed.bench : true,
    scans: seed.scans != null ? seed.scans : true,
    scoped: seed.in_scope_only != null ? seed.in_scope_only : true,
  };
  const field = (label, input, hint) => h('label', null, label, input, hint ? h('span', { class: 'rlhint', text: hint }) : null);
  const name = h('input', { type: 'text', class: 'mono', placeholder: 'X-Bug-Bounty', spellcheck: false, value: st.kind !== 'replace' ? seed.pattern || '' : '' });
  const value = h('input', { type: 'text', class: 'mono', placeholder: 'your-handle', spellcheck: false, value: st.kind !== 'replace' ? seed.replace || '' : '' });
  const overwrite = h('input', { type: 'checkbox', checked: seed.kind === 'set_header' });
  const find = h('input', { type: 'text', class: 'mono', placeholder: '"beta":false', spellcheck: false, value: st.kind === 'replace' ? seed.pattern || '' : '' });
  const repl = h('input', { type: 'text', class: 'mono', placeholder: '"beta":true  (empty removes it)', spellcheck: false, value: st.kind === 'replace' ? seed.replace || '' : '' });
  const regex = h('input', { type: 'checkbox', checked: !!seed.regex });
  const target = h('select', null, REPLACE_TARGETS.map(([k, l]) => h('option', { value: k, text: l, selected: k === st.target })));
  const when = h('input', { type: 'text', class: 'mono', placeholder: 'e.g. host:api.example.com method:POST', spellcheck: false, value: seed.when || '' });
  const note = h('input', { type: 'text', placeholder: 'Note (optional)', value: seed.note || '' });
  const kinds = h('div', { class: 'rlkinds' });
  const sideSeg = h('span', { class: 'seg rlside' });
  const reachBox = h('div', { class: 'rlreachpick' });
  const fields = h('div', { class: 'rlfields' });
  const preview = h('div', { class: 'rlpreview mono' });

  const drawPreview = () => {
    const who = st.side === 'response' ? 'Every response will' : 'Every request will';
    const n = name.value.trim() || 'Header';
    const lines = {
      add_header: [[`${who} carry`], ['add', `+ ${n}: ${value.value.trim()}`], overwrite.checked ? [`replacing any ${n} already there`] : [`unless it already has ${n}`]],
      change_header: [[`Where ${n} is sent, it becomes`], ['chg', `${n}: ${value.value.trim()}`]],
      remove_header: [[`${who} lose`], ['del', `− ${n}`]],
      replace: [[`In ${(REPLACE_TARGETS.find(([k]) => k === target.value) || ['', ''])[1].toLowerCase()}`], ['del', '− ' + (find.value || '…')], ['add', '+ ' + (repl.value || '(nothing)')]],
    }[st.kind];
    if (when.value.trim()) lines.push(['only when ' + when.value.trim()]);
    clear(preview, lines.map(([a, b]) => h('div', { class: b ? 'p-' + a : '', text: b || a })));
  };
  const drawKinds = () =>
    clear(
      kinds,
      RULE_KINDS.map(([k, label, hint]) =>
        h('button', { type: 'button', class: 'rlkindbtn' + (st.kind === k ? ' on' : ''), title: hint, onclick: () => ((st.kind = k), draw()) }, h('b', { text: label }), h('span', { text: hint })),
      ),
    );
  const drawSide = () =>
    clear(
      sideSeg,
      ['request', 'response'].map((s) => h('button', { type: 'button', class: st.side === s ? 'on' : '', text: s === 'request' ? 'Requests' : 'Responses', onclick: () => ((st.side = s), draw()) })),
    );
  const chip = (key, label, hint) =>
    h('button', { type: 'button', class: 'rlpick' + (st[key] ? ' on' : ''), title: hint, text: label, onclick: () => ((st[key] = !st[key]), drawReach(), drawPreview()) });
  const drawReach = () =>
    clear(
      reachBox,
      chip('browser', 'Browser', 'Traffic from your browser, through the proxy'),
      chip('bench', 'Bench', 'Requests you send from the Bench, Run and Access check'),
      chip('scans', 'Scans', 'Requests sent by Scans, crawls and extensions'),
      chip('scoped', 'In-scope hosts only', 'Leave hosts outside your scope alone'),
    );
  const draw = () => {
    drawKinds();
    drawSide();
    const header = st.kind !== 'replace';
    clear(
      fields,
      header ? h('div', { class: 'rlsiderow' }, h('span', { class: 'rlhint', text: 'Change' }), sideSeg) : null,
      header ? field('Header name', name) : null,
      st.kind === 'add_header' || st.kind === 'change_header' ? field('Value', value) : null,
      st.kind === 'add_header' ? h('label', { class: 'domrow' }, overwrite, 'If it’s already there, replace its value') : null,
      header ? null : field('In', target),
      header ? null : field('Find', find),
      header ? null : field('Replace with', repl),
      header ? null : h('label', { class: 'domrow' }, regex, 'Regular expression ($1 puts back what a group matched)'),
    );
    drawPreview();
  };
  for (const el of [name, value, find, repl, when]) el.addEventListener('input', drawPreview);
  for (const el of [overwrite, target]) el.addEventListener('change', drawPreview);
  drawReach();
  draw();

  const save = async () => {
    const header = st.kind !== 'replace';
    const body = {
      kind: header ? (st.kind === 'add_header' && overwrite.checked ? 'set_header' : st.kind) : 'replace',
      target: header ? st.side + '_header' : target.value,
      match: header ? name.value.trim() : find.value,
      replace: header ? (st.kind === 'remove_header' ? '' : value.value.trim()) : repl.value,
      regex: header ? false : regex.checked,
      browser: st.browser,
      bench: st.bench,
      scans: st.scans,
      in_scope_only: st.scoped,
      when: when.value.trim(),
      note: note.value,
    };
    if (!body.match) return (header ? name : find).focus();
    try {
      if (editing) await api('/api/replace/' + seed.id, { method: 'PATCH', body });
      else await api('/api/replace', { method: 'POST', body });
    } catch (e) {
      m.err.textContent = e.message;
      return;
    }
    closeModal();
    toast(editing ? 'Rule saved.' : 'Rule added. It applies to traffic from now on.', 'ok');
    await loadRules();
    if (S.view === 'rules') drawRules();
  };
  const m = modal(
    editing ? 'Edit rule' : 'New rule',
    [
      kinds,
      fields,
      h('div', { class: 'rlsection' }, h('span', { class: 'rlhint', text: 'Applies to' }), reachBox),
      field('Only when (optional)', when, 'A traffic search, the same as in Traffic: host:, path:, method:, status:, mime:, or any text.'),
      note,
      preview,
    ],
    [h('button', { class: 'btn', text: 'Cancel', onclick: closeModal }), h('button', { class: 'btn primary', text: editing ? 'Save' : 'Add rule', onclick: save })],
  );
  m.el.querySelector('.mcard').classList.add('rldialog');
  m.el.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') closeModal();
    if (e.key === 'Enter' && e.target.tagName === 'INPUT' && e.target.type === 'text') save();
  });
  (st.kind === 'replace' ? find : name).focus();
}

/** The "3 rules on" pill in the Traffic toolbar: always shows when rules change traffic. */
function drawRulesPill() {
  const slot = $('#rulespill');
  if (!slot) return;
  const d = RL.data;
  if (!d || !d.rules.length) return clear(slot);
  const n = activeRules().length;
  const on = d.enabled && n > 0;
  clear(
    slot,
    h(
      'button',
      {
        class: 'rlpill' + (on ? ' on' : ''),
        title: on ? 'Rules are changing traffic. Click to see them or pause them' : 'Rules are paused or all off',
        onclick: (e) => rulesMenu(e.currentTarget),
      },
      h('span', { class: 'dot' }),
      on ? `${n} rule${n === 1 ? '' : 's'} on` : 'Rules paused',
    ),
  );
}

function rulesMenu(anchor) {
  closePopover();
  const d = RL.data;
  const items = activeRules().map((r) => {
    const s = ruleSummary(r);
    return h('button', { role: 'menuitem', class: 'rlmenuitem', onclick: () => (closePopover(), ruleDialog(r)) }, h('span', { class: 'rlkind ' + s.cls, text: s.tag }), h('span', { class: 'mono', text: s.text }));
  });
  const menu = h(
    'div',
    { class: 'ctxmenu', role: 'menu' },
    items.length ? items : h('div', { class: 'mnote', text: d.enabled ? 'Every rule is switched off.' : 'All rules are paused.' }),
    h('div', { class: 'msep' }),
    h('button', { role: 'menuitem', text: d.enabled ? 'Pause all rules' : 'Resume rules', onclick: () => (closePopover(), setRulesOn(!d.enabled)) }),
    h('button', { role: 'menuitem', text: 'Open Rules', onclick: () => (closePopover(), leaveTo('rules')) }),
  );
  showPopover(menu, anchor.getBoundingClientRect());
}

/** Right-click on a header line in the Lens: make a rule from it, filled in. */
function lensHeaderMenu(e, pre, side) {
  const pos = document.caretRangeFromPoint ? document.caretRangeFromPoint(e.clientX, e.clientY) : null;
  if (!pos || !pre.contains(pos.startContainer)) return;
  const upto = document.createRange();
  upto.setStart(pre, 0);
  upto.setEnd(pos.startContainer, pos.startOffset);
  const text = pre.textContent;
  const at = upto.toString().length;
  const start = text.lastIndexOf('\n', at - 1) + 1;
  const end = text.indexOf('\n', at);
  const line = text.slice(start, end < 0 ? text.length : end);
  // Header lines only: not the first line, and before the blank line that starts the body.
  const head = text.slice(0, text.indexOf('\n\n') < 0 ? text.length : text.indexOf('\n\n'));
  if (start === 0 || start > head.length) return;
  const c = line.indexOf(':');
  if (c < 1) return;
  const name = line.slice(0, c).trim();
  const value = line.slice(c + 1).trim();
  if (!/^[!#$%&'*+.^_`|~0-9A-Za-z-]+$/.test(name)) return;
  e.preventDefault();
  closePopover();
  const target = side + '_header';
  const every = side === 'response' ? 'every response' : 'every request';
  const item = (label, hint, seed) =>
    h('button', { role: 'menuitem', class: 'rlctx', onclick: () => (closePopover(), ruleDialog({ target, pattern: name, replace: value, ...seed })) }, h('b', { text: label }), h('span', { text: hint }));
  const menu = h(
    'div',
    { class: 'ctxmenu', role: 'menu' },
    h('div', { class: 'mnote rlctxhead' }, h('b', { text: 'Create a rule from ' + name }), h('span', { text: 'You review it before it’s saved. It then applies to new traffic.' })),
    item('New rule: send on ' + every, `Adds ${name}: ${value.length > 24 ? value.slice(0, 24) + '…' : value} where it’s missing`, { kind: 'add_header' }),
    item('New rule: always use this value', `Gives ${name} this value on ${every}`, { kind: 'set_header' }),
    item('New rule: remove from ' + every, `Takes ${name} out before it ${side === 'response' ? 'reaches the browser' : 'leaves'}`, { kind: 'remove_header' }),
    h('div', { class: 'msep' }),
    h('button', { role: 'menuitem', text: 'Copy header', onclick: () => (closePopover(), copyText(line)) }),
  );
  showPopover(menu, { left: e.clientX, bottom: e.clientY - 6 });
}

/** Request as sent, the original before rules changed it, or both side by side with the changed lines marked. */
function ruleCompare(ex, view) {
  // The record keeps the original in HTTP/1.1 form; compare like with like.
  const sent = asPlain({ ...requestText(ex), lines: [`${ex.method} ${target(ex)} HTTP/1.1`, ...requestText(ex).lines.slice(1)] }).split('\n');
  const orig = (ex.original_request || '').replace(/\n$/, '').split('\n');
  const inSent = new Set(sent);
  const inOrig = new Set(orig);
  const pre = (lines, other, cls) => h('pre', { class: 'raw rlcmp' }, lines.map((l) => [other.has(l) || !l ? l : h('span', { class: cls, text: l }), '\n']));
  if (view === 'original') return pre(orig, inSent, 'd-del');
  return h(
    'div',
    { class: 'rlcmpgrid' },
    h('div', null, h('div', { class: 'rlhint', text: 'Original, before the rules' }), pre(orig, inSent, 'd-del')),
    h('div', null, h('div', { class: 'rlhint', text: 'As sent' }), pre(sent, inOrig, 'd-add')),
  );
}

/** In Settings, the switch stays; the rules themselves live on the Rules screen. */
function replacePanel() {
  return h(
    'div',
    { class: 'spanel replace' },
    h('p', { class: 'muted', text: 'Rules are kept on their own screen, where you can add, edit and switch them on or off one by one.' }),
    h('button', { class: 'btn', text: 'Open Rules', onclick: () => leaveTo('rules') }),
  );
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
