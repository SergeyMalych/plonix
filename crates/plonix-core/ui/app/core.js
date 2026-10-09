// Plonix window: Shared state, the API client, the session, the demo walkthrough, theme, live updates and the exchange cache.
// Part of the project window's scripts (see index.html for the load order).
'use strict';

/* ---------- formatting ---------- */

const fmtTime = (ms) => new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit', hour12: false });
const fmtDate = (ms) => new Date(ms).toLocaleString([], { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' });
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

/* ---------- API ---------- */

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
    text: 'Brightcart is a made-up shop whose traffic was captured ahead of time. This walk takes about two minutes and stops on each part of Plonix. Where you see Test now!, press it to watch that part work for real on the demo. Use the arrow keys or the buttons, and leave whenever you like.',
    view: 'traffic',
  },
  {
    view: 'traffic',
    target: '#searchbox',
    title: 'Traffic',
    test: 'traffic',
    text: 'Every request your browser makes through Plonix lands here, live. Search any text, or type a filter such as host:, status:, path: or method: and press Enter.',
  },
  {
    view: 'traffic',
    target: '#chips',
    title: 'Filters',
    test: 'filters',
    text: 'Filters are chips: + shows only what matches, − hides it. The Suggested chips come from this traffic, so the useful ones are a click away. Filters are saved with the project.',
    action: { label: 'See ready-made filters', run: () => filterTour() },
  },
  {
    view: 'traffic',
    target: '#groupseg',
    title: 'Grouping',
    test: 'grouping',
    text: 'The same request sent several times in a row folds into one ×N row with its time span. Switch between Grouped and Every request here, or click a folded row to unfold it in place.',
  },
  {
    view: 'traffic',
    target: '#pathseg',
    title: 'Short paths',
    text: 'Long paths full of ids and tokens are hard to scan. Short path folds them into {id} and {token}, as the Map does, and keeps the last part of each path in view. Hover a row for the full path.',
    action: { label: 'Try short paths', run: () => setShortPath(!T.shortPath) },
  },
  {
    view: 'traffic',
    prep: async () => {
      const r = await api('/api/traffic?limit=1&q=' + encodeURIComponent('path:/v1/orders/1042 mime:json'));
      if (r.items.length) await openInspector(r.items[0].id);
    },
    target: '#inspector',
    title: 'Lens',
    test: 'lens',
    text: 'Lens shows the request and response, and points out what matters in them: tokens, emails and card numbers in this order. Select any text in it to decode it, find it in other traffic or ask Claude about it.',
  },
  {
    view: 'traffic',
    target: ['.lenssugg:not([hidden])', '#inspector'],
    title: 'Suggestions',
    test: 'suggestions',
    text: 'Plonix reads the request and suggests next steps that fit it, one click each, such as drafting a finding or getting ideas for this endpoint. Nothing is sent until you click.',
  },
  {
    view: 'traffic',
    target: ['#inspector .lenshead', '#inspector'],
    title: 'Ask Claude',
    test: 'ask',
    text: 'Ask Claude Code about any request, finding or host. You see exactly what it gets before anything is sent, and the answer comes back here with what it was based on.',
  },
  {
    view: 'scope',
    target: ['#scopebody .card.sugg', '#scopebody'],
    title: 'Scope',
    test: 'scope',
    text: 'As you browse, Plonix spots domains that belong to your target and shows why. You accept or reject each one, and anything that sends requests stays inside what you accepted.',
  },
  {
    view: 'map',
    target: ['#hostlist', '#main .view'],
    title: 'Map',
    test: 'map',
    text: 'Hosts, endpoints and parameters learned from the traffic, with the technologies Plonix detected. Click any endpoint to see its requests in place.',
  },
  {
    view: 'bench',
    target: ['.reqbar', '#main .view'],
    title: 'Bench',
    test: 'bench',
    text: 'Each tab is an experiment: edit a request, send it, branch it and compare the responses. The demo comes with three ready-made experiments.',
  },
  {
    view: 'bench',
    prep: () => {
      const i = R.tabs.findIndex((x) => x.name === 'Order lookup');
      if (i >= 0) R.active = i;
    },
    target: ['.hist', '#main .view'],
    title: 'Compare',
    test: 'compare',
    text: 'Every send is kept in the tab’s history. Tick any two and Compare puts them side by side, with the lines that differ marked, for the response or the request.',
  },
  {
    view: 'bench',
    prep: () => {
      const i = R.tabs.findIndex((x) => (x.url || '').includes(MARK));
      if (i >= 0) R.active = i;
    },
    target: ['.runconf', '#main .view'],
    title: 'Run',
    test: 'run',
    text: 'Mark a value with • and Run sends the request once per value, with a sensible list picked for you. Here it walks the order id through nearby numbers so you can spot orders that aren’t yours.',
  },
  {
    view: 'users',
    prep: () => (US.sel = 'dana'),
    target: ['.uscookies', '#main .view'],
    title: 'Users',
    test: 'sameuser',
    text: 'The people you test as, each with their cookies and headers. Edit a value, expire a cookie to stop sending it, or paste a fresh Cookie header. Cookies the server sets for a user are kept here, so the session stays current. Open a browser as Dana to get a window of her own, where you can sign in as her without signing out anywhere else.',
  },
  {
    view: 'users',
    target: '#actas',
    title: 'Act as a user',
    test: 'actas',
    text: 'Pick who you are from the title bar. Your browser, the Bench and Scans then send as that user to in-scope hosts, so you can click around as Dana and compare with Maya. Switching sends nothing by itself.',
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
    test: 'rules',
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
    test: 'market',
    text: 'Extensions, skills, filter packs and word lists, each signed and checked before it installs. Tools such as Saved users and the Access check, which replays requests as each user and signed out, are switched on from here in your own projects.',
  },
  {
    view: 'market',
    target: '#mkinds',
    title: 'Official, Community and Your own',
    text: 'Each item says who stands behind it. Official items are reviewed by the Plonix maintainers. Community items are written by their authors and checked automatically. Your own are what you add from a GitHub repository, a folder or a file. Plonix asks before anything sensitive, and says when nobody has reviewed the code.',
    action: { label: 'Add your own', run: () => addExternal() },
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

/* ---- Test now: a live example for each stop ---- */

/** What a Test now example can do: wait, type like a person, press a
 * button where it can be seen, and move the ring to what it shows. Every
 * example runs on the demo's own data; its made-up hosts are answered by the
 * demo's stand-in API, so nothing reaches the internet. */
function tourKit(i) {
  const alive = () => TOUR.i === i && !!TOUR.el;
  const wait = (ms) => new Promise((r) => setTimeout(r, ms));
  const kit = {
    alive,
    wait,
    async until(fn, ms = 6000) {
      for (let t = 0; t < ms && alive(); t += 100) {
        const v = fn();
        if (v) return v;
        await wait(100);
      }
      return fn();
    },
    ring(target) {
      TOUR.focus = target;
      placeTour();
    },
    async type(el, text, { clear: wipe = true, ms = 40 } = {}) {
      if (!el) return;
      if (wipe) el.value = '';
      for (const ch of text) {
        if (!alive()) return;
        el.value += ch;
        el.dispatchEvent(new Event('input', { bubbles: true }));
        await wait(ms);
      }
      el.blur();
    },
    /** Glides the pointer to the middle of `el`. */
    async point(el) {
      if (!el || !alive()) return;
      el.scrollIntoView({ block: 'nearest', inline: 'nearest' });
      const m = tourMouse();
      const r = el.getBoundingClientRect();
      m.hidden = false;
      m.classList.remove('gone');
      m.style.transform = `translate(${Math.round(r.left + Math.min(r.width / 2, 40))}px, ${Math.round(r.top + r.height / 2)}px)`;
      await wait(620);
    },
    /** Points at `el` and clicks it, so the user sees where the click lands. */
    async press(el) {
      if (!el || !alive()) return;
      await kit.point(el);
      if (!alive()) return;
      const m = tourMouse();
      m.classList.add('click');
      el.classList.add('tourpress');
      await wait(260);
      m.classList.remove('click');
      el.classList.remove('tourpress');
      el.click();
      await wait(120);
    },
  };
  // Typing starts with a click into the field.
  const type = kit.type;
  kit.type = async (el, text, opts) => {
    await kit.press(el);
    await type(el, text, opts);
  };
  return kit;
}

/** The pointer a Test now example moves and clicks with, starting from the Test now button. */
function tourMouse() {
  let m = $('.tourmouse');
  if (!m) {
    m = h('div', { class: 'tourmouse', hidden: true, 'aria-hidden': 'true' });
    m.innerHTML = '<svg width="22" height="26" viewBox="0 0 22 26"><path d="M2 2 L2 21 L7 16.5 L10.5 24 L14 22.5 L10.6 15.2 L17.5 15 Z" fill="#fff" stroke="#14161c" stroke-width="1.6" stroke-linejoin="round"/></svg><span class="tmring"></span>';
    document.body.append(m);
    const b = $('.tourcard .ttest');
    const r = b ? b.getBoundingClientRect() : { left: innerWidth / 2, top: innerHeight / 2, width: 0, height: 0 };
    m.style.transform = `translate(${Math.round(r.left + r.width / 2)}px, ${Math.round(r.top + r.height / 2)}px)`;
  }
  return m;
}

function hideTourMouse(now) {
  const m = $('.tourmouse');
  if (!m) return;
  if (now) return m.remove();
  m.classList.add('gone');
  setTimeout(() => m.classList.contains('gone') && m.remove(), 900);
}

const andList = (xs) => (xs.length < 2 ? xs.join('') : xs.slice(0, -1).join(', ') + ' and ' + xs[xs.length - 1]);

const TOUR_TESTS = {
  traffic: {
    run: async (t) => {
      await t.type($('#q'), 'usr_8f2c41');
      await t.wait(700);
      t.ring('#tablewrap');
      const n = (($('#tcount') || {}).textContent || '').trim();
      return `${n || 'These requests'} carry Maya’s user id, usr_8f2c41, somewhere in a URL, header or body. One search, across every host, as fast as you type.`;
    },
    undo: () => {
      const q = $('#q');
      if (q) q.value = '';
      T.text = '';
      saveTrafficView();
      if (S.view === 'traffic' && T.refresh) T.refresh(true);
    },
  },
  filters: {
    run: async (t) => {
      TOUR.saved = { filters: T.filters.map((f) => ({ ...f })), text: T.text };
      const filters = [{ term: 'is:auth', mode: 'include' }];
      T.filters = filters;
      T.text = '';
      const q = $('#q');
      if (q) q.value = '';
      filtersChanged();
      await t.wait(700);
      t.ring('#tablewrap');
      const r = await api('/api/traffic?limit=200&q=' + encodeURIComponent(queryFor(filters, ''))).catch(() => ({ items: [], total: 0 }));
      const hosts = new Set(r.items.map((x) => x.host));
      return `One chip, is:auth, pulls out the whole sign-in flow: ${plural(r.total, 'request')} across ${plural(hosts.size, 'host')}, from the login page to the token exchange.`;
    },
    undo: () => {
      if (!TOUR.saved) return;
      T.filters = TOUR.saved.filters;
      T.text = TOUR.saved.text;
      TOUR.saved = null;
      if (S.view === 'traffic') filtersChanged();
      else saveTrafficView();
    },
  },
  grouping: {
    run: async (t) => {
      const rowsNow = () => document.querySelectorAll('#rows tr').length;
      const seg = await t.until(() => $('#groupseg'));
      if (!seg) return 'Grouping lives in Traffic.';
      t.ring('#groupseg');
      const grouped = rowsNow();
      await t.press([...seg.querySelectorAll('button')].find((b) => /every/i.test(b.textContent)));
      await t.until(() => rowsNow() !== grouped, 2500);
      const every = rowsNow();
      t.ring('#tablewrap');
      await t.wait(900);
      await t.press([...$('#groupseg').querySelectorAll('button')].find((b) => /grouped/i.test(b.textContent)));
      await t.until(() => rowsNow() === grouped, 2500);
      const tag = await t.until(() => $('#rows .tag.alike:not(.on)'));
      if (!tag) return `Every request shows ${every} rows; Grouped folds them into ${grouped}.`;
      const id = Number(tag.closest('tr').dataset.id);
      const n = (tag.textContent.match(/\d+/) || ['2'])[0];
      await t.press(tag);
      await t.wait(250);
      const tr = $(`#rows tr[data-id="${id}"]`);
      if (tr) t.ring(tr);
      const ex = await getExchange(id).catch(() => null);
      return `Every request: ${every} rows. Grouped: ${grouped}, with repeats folded into one row. ${ex ? ex.method + ' ' + ex.path : 'This one'} was sent ${n} times in a row; one click unfolds them right underneath.`;
    },
    undo: () => {
      if (!T.group) setGrouping(true);
    },
  },
  lens: {
    run: async (t) => {
      const ex = T.sel ? await getExchange(T.sel).catch(() => null) : null;
      const auth = ex && (ex.req_headers || []).find(([k]) => /^authorization$/i.test(k));
      const tok = auth && auth[1].replace(/^Bearer\s+/i, '');
      const slot = $('#inspector .selslot');
      if (!tok || !slot) return 'Select any text in the Lens to decode it.';
      t.ring(slot.parentElement);
      decodeSelection(slot.parentElement, tok);
      await t.wait(200);
      t.ring($('#inspector .spotdetail') || slot.parentElement);
      const it = genericDecode(tok);
      const p = (it.jwt && it.jwt.payload) || {};
      const who = [p.sub && 'user ' + p.sub, p.role && 'role ' + p.role, p.exp && 'expires ' + new Date(p.exp * 1000).toLocaleDateString()].filter(Boolean).join(', ');
      return `The bearer token, decoded in place: ${who || 'its claims and header'}. Select any value in a request or response to do the same.`;
    },
    undo: () => document.querySelectorAll('#inspector .selslot').forEach((s) => clear(s)),
  },
  suggestions: {
    run: async (t) => {
      const chip = await t.until(() => $('.lenssugg .chip.k-warn') || $('.lenssugg .chip'));
      if (!chip) return 'No suggestions for this request.';
      const label = chip.textContent.trim();
      await t.press(chip);
      const m = await t.until(() => $('.modal'));
      if (!m) return label;
      m.classList.add('tourmodal');
      t.ring(m.querySelector('.mcard'));
      return `“${label}” turned into a ready finding: title, severity and this request as evidence, filled in for you. Save it as is, or let Claude write it up.`;
    },
  },
  scope: {
    run: async (t) => {
      const card = $('#scopebody .card.sugg');
      const btn = card && card.querySelector('button.primary');
      if (!btn) return 'Nothing waiting for a decision.';
      const host = (card.querySelector('.dom') || {}).textContent || 'This host';
      const why = card.querySelectorAll('.ev').length;
      await t.press(btn);
      await t.wait(600);
      t.ring(['#railsecs', '#scopebody']);
      return `${host.trim()} is now in scope, accepted on ${why ? plural(why, 'piece', 'pieces') + ' of' : 'the'} evidence Plonix found while you browsed. The Bench, Scans and every check can reach it; anything you didn’t accept stays out.`;
    },
  },
  map: {
    run: async (t) => {
      const host = 'api.brightcart.example';
      M.sel = host;
      M.specOpen = host;
      if (typeof drawHostList === 'function') drawHostList();
      drawHostDetail();
      const sec = await t.until(() => $('#hostdetail .specsec'));
      if (!sec) return 'This host has no API description.';
      sec.scrollIntoView({ block: 'center' });
      await t.wait(250);
      t.ring(sec);
      const spec = await api('/api/hosts/' + encodeURIComponent(host) + '/spec').catch(() => null);
      const todo = spec ? spec.endpoints.filter((e) => !e.visited) : [];
      const pick = todo.find((e) => /admin|role/.test(e.path)) || todo[0];
      return `Plonix found the API’s own description in the traffic: ${plural(spec ? spec.endpoints.length : 0, 'endpoint')}, ${todo.length} never visited${pick ? `, such as ${pick.method} ${pick.path}` : ''}. Each one is a click from the Bench.`;
    },
  },
  sameuser: {
    run: async (t) => {
      if (!userById('maya') || !userById('dana')) return 'Maya and Dana are gone; start the demo over to get them back.';
      go('bench', true);
      const tab = { name: 'Who am I', method: 'GET', url: 'https://api.brightcart.example/v1/me', raw: 'Accept: application/json\nUser-Agent: Plonix\n\n', bodyB64: null, history: [], cur: null, picks: [], asUser: 'maya' };
      const was = R.active;
      R.tabs.push(tab);
      R.active = R.tabs.length - 1;
      TOUR.undo = () => {
        const i = R.tabs.indexOf(tab);
        if (i < 0) return;
        R.tabs.splice(i, 1);
        R.active = Math.min(was, R.tabs.length - 1);
        saveBench();
      };
      saveBench();
      renderBench($('#main'));
      const who = async () => {
        const send = await t.until(() => $('.reqbar .btn.primary:not([disabled])'));
        const before = tab.cur;
        await t.press(send);
        await t.until(() => tab.cur && tab.cur !== before && !$('.reqbar .btn.primary[disabled]'));
        await t.wait(250);
        t.ring(['.rsplit > .rcol:last-child', '#main .view']);
        try {
          return JSON.parse((await getExchange(tab.cur)).resp_text).name || null;
        } catch (_) {
          return null;
        }
      };
      t.ring('.reqbar');
      const first = await who();
      await t.wait(900);
      const sel = await t.until(() => $('.reqbar .assel'));
      t.ring('.reqbar');
      await t.press(sel);
      sel.value = 'dana';
      sel.dispatchEvent(new Event('change', { bubbles: true }));
      await t.wait(500);
      const second = await who();
      if (!first || !second) return 'The shop did not recognise the session; start the demo over to refresh the users’ cookies.';
      return `Sent one request as Maya, then picked Dana in the box next to Send and sent it again. The shop answered ${first}, then ${second}: each user’s cookies went with the same request, and nothing else changed.`;
    },
  },
  actas: {
    run: async (t) => {
      const pill = await t.until(() => $('#actas'));
      if (!pill) return 'Saved users is not switched on in this project.';
      await t.press(pill);
      const dana = await t.until(() => [...document.querySelectorAll('.actmenu .actitem')].find((b) => /dana/i.test(b.textContent)));
      if (!dana) return 'Dana is gone; start the demo over to get her back.';
      await t.wait(350);
      await t.press(dana);
      await t.until(() => S.acting);
      await t.wait(300);
      t.ring('#actas');
      const send = (as_user) => api('/api/send', { method: 'POST', body: { method: 'GET', url: 'https://api.brightcart.example/v1/me', headers: [['Accept', 'application/json']], as_user } });
      const who = async (as_user) => {
        try {
          return JSON.parse((await send(as_user)).resp_text).name || null;
        } catch (_) {
          return null;
        }
      };
      const [asDana, asMaya] = [await who(S.acting), await who('maya')];
      if (!asDana) return 'The shop did not recognise Dana; her session may have run out. Start the demo over to refresh it.';
      return `You are now ${asDana}. The shop’s “who am I” endpoint answers ${asDana}, where the same request as Maya answers ${asMaya || 'Maya'}. Browse, use the Bench or run a scan and it all goes out as ${asDana.split(' ')[0]}, with no logging out and back in.`;
    },
    undo: () => S.acting && api('/api/users/acting', { method: 'PUT', body: { id: null } }).then(() => ((S.acting = null), drawActing())).catch(() => {}),
  },
  rules: {
    run: async (t) => {
      const ex = await api('/api/send', { method: 'POST', body: { method: 'GET', url: 'https://api.brightcart.example/v1/orders/1042', headers: [['Accept', 'application/json']] } });
      t.ring(['.rllist', '#main .view']);
      // Only Accept was sent, so any other header was added by a rule on the way.
      const changes = (ex.req_headers || []).filter(([k]) => !/^(accept|host|content-length)$/i.test(k)).map(([k, v]) => `${k}: ${v}`);
      return `Sent a bare request from the Bench. On the way, the rules ${changes.length ? 'added ' + changes.join(', ') : 'checked it and changed nothing'}, and the request is tagged “changed” in Traffic so you always know.`;
    },
  },
  ask: {
    run: async (t) => {
      const btn = await t.until(() => [...document.querySelectorAll('#inspector button')].find((b) => /ask claude/i.test(b.textContent)));
      if (!btn) return 'Open a request in the Lens to ask about it.';
      await t.press(btn);
      const m = await t.until(() => $('.modal textarea.askq'));
      if (!m) return 'Ask Claude needs Claude Code on this Mac.';
      $('.modal').classList.add('tourmodal');
      t.ring($('.modal .mcard'));
      await t.type(m, 'Can this token read other customers’ orders? What should I try next?', { ms: 22 });
      m.dispatchEvent(new Event('input', { bubbles: true }));
      await t.wait(500);
      const parts = document.querySelectorAll('.modal .askparts input[type=checkbox]:checked').length;
      return `Ask Claude opened on this request with your question, and shows exactly what Claude Code gets${parts ? ` (${plural(parts, 'part')}, each one you can untick)` : ''}. Press Ask Claude and the answer is written here, then kept in Agents.`;
    },
  },
  compare: {
    run: async (t) => {
      const i = R.tabs.findIndex((x) => x.name === 'Order lookup');
      if (i < 0 || R.tabs[i].history.length < 2) return 'The order lookup experiment is gone; start the demo over to get it back.';
      R.active = i;
      R.tabs[i].picks = [];
      saveBench();
      renderBench($('#main'));
      await t.until(() => $('.hist .histrows'));
      t.ring('.hist');
      const boxes = [...document.querySelectorAll('.hist .histrows input[type=checkbox]')].slice(0, 2);
      for (const b of boxes) await t.press(b);
      const cmp = await t.until(() => [...document.querySelectorAll('.hist .histhead button')].find((b) => /compare/i.test(b.textContent)));
      await t.press(cmp);
      const view = await t.until(() => $('#cmpslot .cmpview'));
      if (!view) return 'Tick two sends to compare them.';
      view.scrollIntoView({ block: 'start' });
      await t.wait(300);
      t.ring('#cmpslot .cmpview');
      const [a, b] = await Promise.all(R.tabs[i].picks.map(getExchange));
      const who = (ex) => {
        try {
          return JSON.parse(ex.resp_text).customer.name;
        } catch (_) {
          return null;
        }
      };
      const sum = ($('#cmpslot .cmpsum') || {}).textContent || '';
      return `${sum} Same request, one digit apart in the URL, and two different people’s orders: ${andList([who(a), who(b)].filter(Boolean)) || 'two customers'}, with their emails and cards.`;
    },
    undo: () => {
      const tab = R.tabs.find((x) => x.name === 'Order lookup');
      if (tab) tab.picks = [];
      saveBench();
    },
  },
  bench: {
    run: async (t) => {
      const i = R.tabs.findIndex((x) => x.name === 'Order lookup');
      if (i < 0) return 'The order lookup experiment is gone; start the demo over to get it back.';
      R.active = i;
      saveBench();
      renderBench($('#main'));
      const url = await t.until(() => $('#benchurl'));
      t.ring('.reqbar');
      url.value = url.value.replace(/\d+$/, '');
      await t.type(url, '1043', { clear: false, ms: 140 });
      await t.press($('.reqbar .btn.primary'));
      await t.until(() => R.tabs[i].cur && !$('.reqbar .btn.primary[disabled]'));
      await t.wait(300);
      t.ring(['.rsplit > .rcol:last-child', '#main .view']);
      const ex = R.tabs[i].cur ? await getExchange(R.tabs[i].cur).catch(() => null) : null;
      let who = '';
      try {
        const o = JSON.parse(ex.resp_text);
        who = `${o.customer.name}’s order: their name, email and card number`;
      } catch (_) {}
      return `Order 1043 came back with ${who || 'someone else’s order'}, while signed in as Maya. The API never checks whose order it is.`;
    },
  },
  run: {
    run: async (t) => {
      const i = R.tabs.findIndex((x) => /run/i.test(x.name || '') && (x.url || '').includes(MARK));
      if (i < 0) return 'The ready-made run is gone; start the demo over to get it back.';
      R.active = i;
      saveBench();
      renderBench($('#main'));
      const start = await t.until(() => $('.runstart:not([disabled])'));
      if (!start) return 'Pick a value to run first.';
      await t.press(start);
      t.ring('#runresults');
      const tab = R.tabs[i];
      await t.until(() => tab.runState && !tab.runState.busy, 20000);
      const rep = tab.runState && tab.runState.report;
      if (!rep) return (tab.runState && tab.runState.error) || 'The run did not finish.';
      const ok = rep.rows.filter((r) => r.status === 200 && r.exchange_id);
      const names = new Set();
      for (const r of ok.slice(0, 10)) {
        try {
          names.add(JSON.parse((await getExchange(r.exchange_id)).resp_text).customer.name);
        } catch (_) {}
      }
      t.ring('#runresults');
      return `Sent ${plural(rep.rows.length, 'request')} in a moment and got ${ok.length} orders back, belonging to ${plural(names.size, 'different customer')}, including ${andList([...names].slice(0, 3))}. That’s the flaw, found with one click.`;
    },
  },
  market: {
    run: async (t) => {
      for (const name of ['saved-users', 'access-check']) {
        if (!toolOn(name)) await api('/api/market/install', { method: 'POST', body: { name } });
      }
      S.status = await api('/api/status');
      renderShell();
      const r = await api('/api/traffic?limit=1&q=' + encodeURIComponent('path:/v1/orders/1042 mime:json'));
      if (!r.items.length) return 'The order to check is gone; start the demo over to get it back.';
      startAccessCheck({ targets: [r.items[0].id], sourceLabel: 'GET /v1/orders/1042, Maya’s order' });
      const btn = await t.until(() => $('#main .acpane .btn.primary:not([disabled])'));
      t.ring('#main .view');
      await t.press(btn);
      await t.until(() => AC.report || AC.err, 15000);
      await t.wait(200);
      t.ring(['#main .actable', '#main .view']);
      if (!AC.report) return AC.err || 'The check did not finish.';
      const ok = AC.report.identities.filter((id) => AC.report.rows[0].cells.some((c) => c.identity === id.id && okStatus(c.status)));
      return `Switched on the Access check, then replayed Maya’s order ${andList(AC.report.identities.map((x) => (x.anon ? 'signed out' : 'as ' + x.label.replace(/\s*\(.*\)$/, ''))))}. ${ok.length === AC.report.identities.length ? 'Every one of them got it, even signed out.' : `${plural(ok.length, 'identity', 'identities')} got it.`}`;
    },
  },
};

const TOUR = { i: -1, el: null, spot: null, timer: null, keys: null, focus: null, undo: null, saved: null };

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
  tourUndo();
  Object.assign(TOUR, { i: -1, el: null, spot: null, timer: null, keys: null, focus: null, saved: null });
}

async function tourStep(i) {
  if (!TOUR.el || i < 0) return;
  if (i >= TOUR_STEPS.length) return endTour();
  tourUndo();
  TOUR.i = i;
  TOUR.focus = null;
  const s = TOUR_STEPS[i];
  const last = i === TOUR_STEPS.length - 1;
  closeModal();
  if (s.prep && s.view !== 'traffic') s.prep();
  if (s.view && (S.view !== s.view || s.view === 'settings' || (s.prep && s.view !== 'traffic'))) go(s.view, true);
  if (s.prep && s.view === 'traffic') await Promise.resolve(s.prep()).catch(() => {});
  if (TOUR.i !== i || !TOUR.el) return;
  const dots = TOUR_STEPS.map((_, n) => h('span', { class: 'tdot' + (n === i ? ' on' : n < i ? ' done' : '') }));
  clear(
    TOUR.el,
    h('div', { class: 'tourhead' }, h('span', { class: 'tcount', text: i && !last ? `${i} of ${TOUR_STEPS.length - 2}` : 'Demo walkthrough' }), h('button', { class: 'iconbtn', title: 'End the walkthrough  (Esc)', text: '✕', onclick: endTour })),
    h('h4', { text: s.title }),
    h('p', { text: s.text }),
    s.action ? h('button', { class: 'linkbtn taction', text: s.action.label + ' →', onclick: s.action.run }) : null,
    s.test ? tourTestBox(i, TOUR_TESTS[s.test]) : null,
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

/** Undoes what the last Test now changed on screen, so the next stop starts clean. */
function tourUndo() {
  hideTourMouse(true);
  const undo = TOUR.undo;
  TOUR.undo = null;
  if (undo) {
    try {
      undo();
    } catch (_) {}
  }
}

/** The Test now button, and where the example says what it found. */
function tourTestBox(i, test) {
  if (!test) return null;
  const result = h('div', { class: 'tresult', hidden: true });
  const btn = h('button', { class: 'btn sm ttest', onclick: () => go() }, h('span', { class: 'tico', text: '▶' }), h('span', { text: 'Test now!' }));
  const go = async () => {
    if (btn.disabled) return;
    tourUndo();
    btn.disabled = true;
    btn.classList.add('busy');
    btn.lastChild.textContent = 'Running…';
    result.hidden = true;
    TOUR.undo = test.undo || null;
    let said;
    try {
      said = await test.run(tourKit(i));
    } catch (e) {
      said = e.message;
    }
    if (TOUR.i !== i || !TOUR.el) return;
    setTimeout(() => TOUR.i === i && hideTourMouse(), 1400);
    btn.disabled = false;
    btn.classList.remove('busy');
    btn.lastChild.textContent = 'Test again';
    if (said) {
      clear(result, h('span', { class: 'tok', text: '✓' }), h('span', { text: said }));
      result.hidden = false;
    }
    placeTour();
  };
  return h('div', { class: 'ttestbox' }, btn, result);
}

/** The first of a step's targets that is on screen; a running Test now
 * points the ring at what it is showing instead. */
function tourTarget(s) {
  for (const sel of [].concat(TOUR.focus || s.target || [])) {
    if (sel instanceof Element) {
      if (sel.isConnected && sel.getClientRects().length) return sel;
      continue;
    }
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
    if (vw - r.right - pad >= cw + gap + 12) [x, y] = [r.right + pad + gap, clampY(r.top)];
    else if (r.left - pad >= cw + gap + 12) [x, y] = [r.left - pad - gap - cw, clampY(r.top)];
    else if (vh - r.bottom - pad >= ch + gap * 2) [x, y] = [clampX(r.left), r.bottom + pad + gap];
    else if (r.top - pad >= ch + gap * 2) [x, y] = [clampX(r.left), r.top - pad - gap - ch];
    else [x, y] = [clampX(r.right - cw - 20), clampY(r.bottom - ch - 20)];
  }
  // Whatever the placement, the whole card stays inside the window.
  x = Math.max(12, Math.min(x, vw - cw - 12));
  y = Math.max(12, Math.min(y, vh - ch - 12));
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
    if (prev.tools && String(st.tools) !== String(prev.tools)) redrawNav();
    if (S.view === 'scope' && JSON.stringify(st.extensions) !== JSON.stringify(prev.extensions)) renderScopeBody();
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
