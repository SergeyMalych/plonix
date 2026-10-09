// Plonix window: The window shell: screens, sidebar, keyboard, menu-bar entry points. Loaded last: it builds VIEWS from every screen and boots the page.
// Part of the project window's scripts (see index.html for the load order).
'use strict';

/* ---------- shell ---------- */

const VIEWS = {
  traffic: { label: 'Traffic', ico: '⇅', render: renderTraffic },
  bench: { label: 'Bench', ico: '⎇', render: renderBench },
  scope: { label: 'Scope', ico: '◉', render: renderScope },
  map: { label: 'Map', ico: '⊞', render: renderMap },
  users: { label: 'Users', ico: '☺\uFE0E', render: renderUsers, tool: 'saved-users' },
  access: { label: 'Access', ico: '⚿', render: renderAccess, tool: 'access-check' },
  callbacks: { label: 'Callbacks', ico: '↩', render: renderCallbacks, tool: 'callbacks' },
  findings: { label: 'Findings', ico: '⚑', render: renderFindings },
  agents: { label: 'Agents', ico: '✦', render: renderAgents },
  market: { label: 'Market', ico: '⬢', render: renderMarket },
  scans: { label: 'Scans', ico: '⌖', render: renderScans },
  programs: { label: 'Programs', ico: '◈', render: renderPrograms, tool: 'programs' },
  rules: { label: 'Rules', ico: '⇄', render: renderRules },
  settings: { label: 'Settings', ico: '⚙', render: renderSettings, footer: true },
};

const IN_APP = !!window.__PLONIX_APP__;

/** Extensions switched on that run a given way (enumerate, probe, scan or sandbox), by name. */
const extsThat = (kind) => Object.entries((S.status && S.status.extensions) || {}).filter(([, k]) => k === kind).map(([n]) => n);
/** Whether a built-in tool has been switched on from the Market. */
const toolOn = (id) => !!(S.status && ((S.status.tools && S.status.tools.includes(id)) || (S.status.demo && DEMO_TOOLS.includes(id))));
/** Tools the demo project shows without installing them, so its walkthrough can stop on them. */
const DEMO_TOOLS = ['saved-users', 'access-check', 'programs'];

/** What a suggestion says when the built-in tool it needs is still off. */
const TOOL_ASK = {
  'saved-users': { label: 'Saved users', what: 'keeps each user’s cookies and tokens, so the Bench, Scans and your browser can send as them' },
  'access-check': { label: 'Access check', what: 'replays a request as each saved user and once signed out, and lines up who got what' },
};

/** Makes sure the built-in tool a suggestion needs is on. If it is off, asks once and
 * switches it on from the official Market (the Access check brings Saved users with it).
 * Resolves true when the tool is ready; nothing else is sent. */
function useTool(id, action) {
  if (toolOn(id)) return Promise.resolve(true);
  const need = [id === 'access-check' && !toolOn('saved-users') ? 'saved-users' : null, id].filter(Boolean);
  const names = need.map((n) => (TOOL_ASK[n] || { label: n }).label);
  return new Promise((done) => {
    const yes = h('button', {
      class: 'btn primary',
      text: 'Switch on',
      onclick: async () => {
        yes.disabled = true;
        try {
          for (const name of need) await api('/api/market/install', { method: 'POST', body: { name } });
          await poll();
          closeModal();
          toast(`${andList(names)} switched on`, 'ok');
          done(toolOn(id));
        } catch (e) {
          m.err.textContent = e.message;
          yes.disabled = false;
        }
      },
    });
    const m = modal(
      `Switch on ${andList(names)}?`,
      [
        h('div', { text: `“${action}” needs ${andList(names)}, free ${need.length > 1 ? 'tools' : 'tool'} from the official Market. ${need.map((n) => `${(TOOL_ASK[n] || { label: n }).label} ${(TOOL_ASK[n] || { what: '' }).what}.`).join(' ')}` }),
        h('div', { class: 'muted', text: `Switching on sends nothing. You can switch ${need.length > 1 ? 'them' : 'it'} off again in the Market.` }),
      ],
      [h('button', { class: 'btn', text: 'Not now', onclick: () => (closeModal(), done(false)) }), yes],
    );
  });
}

/** Screens with a number key (⌘1-⌘9 in the app), in the order of the app's
 * View menu (plonix-app/src/main.rs), so a hint always names the right key. */
const SHORTCUTS = ['traffic', 'bench', 'scope', 'map', 'findings', 'agents', 'market', 'scans', 'programs'];
const shownView = (key) => !!VIEWS[key] && !VIEWS[key].footer && (!VIEWS[key].tool || toolOn(VIEWS[key].tool));

/** The sidebar's screen buttons, without the tools that are not switched on. */
function navList() {
  const nav = h('div', { class: 'nav' });
  Object.entries(VIEWS).forEach(([key, v]) => {
    if (!shownView(key)) return;
    const n = SHORTCUTS.indexOf(key);
    nav.append(
      h(
        'button',
        { 'data-v': key, title: n < 0 ? v.label : `${v.label}  (${IN_APP ? '⌘' : ''}${n + 1})`, onclick: () => go(key) },
        h('span', { class: 'ico', text: v.ico }),
        h('span', { class: 'nl', text: v.label }),
        h('span', { class: 'ct', id: 'ct-' + key }),
      ),
    );
  });
  return nav;
}

/** Rebuilds the sidebar's buttons and the Act as pill when a tool is
 * switched on or off, and leaves a screen whose tool was just removed. */
function redrawNav() {
  const old = $('#rail .nav');
  if (!old) return;
  const nav = navList();
  for (const b of nav.children) b.classList.toggle('on', b.dataset.v === S.view);
  old.replaceWith(nav);
  const pill = $('#actas');
  if (toolOn('saved-users') && !pill) {
    $('#engine').before(actasPill());
    loadUsers().then(drawActing);
  } else if (!toolOn('saved-users') && pill) pill.remove();
  if (!shownView(S.view) && !VIEWS[S.view]?.footer) go('traffic', true);
  updateChrome();
}

const actasPill = () => h('button', { class: 'actas', id: 'actas', onclick: (e) => actingMenu(e.currentTarget) });

function renderShell() {
  const nav = navList();
  clear(
    $('#app'),
    h(
      'div',
      { class: 'titlebar' },
      h('div', { class: 'brand' }, h('img', { src: '/ui/icon.svg', alt: '' }), 'Plonix', h('span', { class: 'proj', id: 'proj' })),
      h(
        'div',
        { class: 'right' },
        toolOn('saved-users') ? actasPill() : null,
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
  if (toolOn('saved-users')) loadUsers().then(drawActing);
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

/** Opens a target in the capture browser: an isolated browser that routes
 * through Plonix. With a saved user, it is that user's own browser window. */
function openTarget(user) {
  user = user && user.id ? user : null;
  const who = user ? userShortName(user) : '';
  const lastHost = () => {
    const r = ((S.scope && S.scope.rules) || []).find((x) => x.decision === 'accepted');
    return r ? r.pattern : '';
  };
  const input = h('input', { placeholder: 'example.com', spellcheck: 'false', autocomplete: 'off', value: pstore('plonix.lastTarget') || (user ? lastHost() : '') });
  const go = async () => {
    const target = input.value.trim();
    if (!target) return input.focus();
    btn.disabled = true;
    m.err.textContent = '';
    const r = await launchTarget(target, user);
    btn.disabled = false;
    // launchTarget may have replaced this dialog with one of its own.
    if (!m.el.isConnected) return;
    if (r.ok) closeModal();
    else m.err.textContent = r.message;
  };
  input.addEventListener('keydown', (e) => e.key === 'Enter' && go());
  const btn = h('button', { class: 'btn primary', text: 'Open', onclick: go });
  const m = modal(
    user ? `Open a browser as ${who}` : 'Open a target',
    [
      h('label', null, 'Site or URL', input),
      h('p', {
        class: 'muted mnote',
        text: user
          ? `Opens a browser window of ${who}’s own, with its own cookies, so you can sign in as ${who} there and stay signed in as yourself here. ${(user.cookies || []).some(cookieLive) ? 'It starts with the cookies saved here' : 'Sign in once in that window'}. The cookies and tokens it uses are saved for ${who} as you go, so the Bench, the Access check and Scans send the same.`
          : 'Opens a separate browser that captures through Plonix and trusts its certificate. The domain and its subdomains go into scope.',
      }),
    ],
    [h('button', { class: 'btn', text: 'Cancel', onclick: closeModal }), btn],
  );
  input.select();
}

async function launchTarget(target, user) {
  try {
    const r = await api('/api/browser/open', { method: 'POST', body: { target, as_user: user ? user.id : null } });
    pstore('plonix.lastTarget', target);
    await loadScope();
    if (r.as_user) {
      const who = userShortName(r.as_user);
      toast(`Opened ${r.url} in a ${r.browser} window of ${who}’s own. What you do there is sent as ${who}, and their session is saved here as you go.`, 'ok');
    } else {
      toast(`Opened ${r.url} in ${r.browser}. Browse the site; requests appear in Traffic.`, 'ok');
      if (S.view !== 'traffic') go('traffic');
    }
    if (r.needs_trust) trustCertificate(r.browser, r.can_trust);
    return { ok: true };
  } catch (e) {
    if (e.code === 'no_browser' && e.data && e.data.can_install) {
      getPlonixBrowser(target, user);
      return { ok: false, message: '' };
    }
    return { ok: false, message: e.message };
  }
}

const megabytes = (n) => (n / 1048576).toFixed(0);

/** No browser to launch: offers the Plonix browser (Chromium, downloaded once), then opens the target in it. */
function getPlonixBrowser(target, user) {
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
        const r = await launchTarget(target, user);
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
  const key = /^[1-9]$/.test(e.key) && SHORTCUTS[Number(e.key) - 1];
  if (key && shownView(key)) return go(key);
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

// Entry points for the Plonix app's menu bar.
window.plonix = {
  go: (view) => S.token && $('#main') && go(view),
  openTarget: () => S.token && $('#main') && openTarget(),
  toggleSidebar: () => S.token && $('#main') && toggleSidebar(),
  importHar: () => S.token && $('#main') && importHar(),
  exportHar: () => S.token && $('#main') && exportHar({ q: S.view === 'traffic' ? fullQuery() : '', ids: S.view === 'traffic' ? [...T.picked] : [] }),
};

boot();
