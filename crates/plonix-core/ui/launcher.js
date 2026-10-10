/* The Plonix Start screen: pick a project, create one, change its settings.
 * Each project opens in its own window (in the app) or browser tab, served
 * by a session of its own. */
'use strict';

const IN_APP = !!window.__PLONIX_APP__;

const L = { token: null, about: {}, projects: [], timer: null, opening: null, look: {} };

async function api(path, { method = 'GET', body } = {}) {
  let resp;
  try {
    resp = await fetch(path, {
      method,
      headers: { Authorization: 'Bearer ' + L.token, 'X-Plonix-Client': 'launcher', ...(body !== undefined ? { 'Content-Type': 'application/json' } : {}) },
      body: body !== undefined ? JSON.stringify(body) : undefined,
      cache: 'no-store',
    });
  } catch (_) {
    throw new ApiError(0, 'engine_down', 'Plonix is not running. Open it again.');
  }
  const data = await resp.json().catch(() => null);
  if (resp.status === 401) {
    store('plonix.hubtoken', null);
    showLock('This page has signed out.');
    throw new ApiError(401, 'unauthorized', 'Signed out', data);
  }
  if (!resp.ok) throw new ApiError(resp.status, (data && data.code) || 'error', (data && data.error) || 'Request failed', data);
  return data;
}

/* ---------- helpers ---------- */

function ago(ms) {
  if (!ms) return 'never opened';
  const s = Math.max(0, (Date.now() - ms) / 1000);
  if (s < 60) return 'just now';
  if (s < 3600) return Math.floor(s / 60) + ' min ago';
  if (s < 86400) return Math.floor(s / 3600) + ' h ago';
  if (s < 86400 * 30) return Math.floor(s / 86400) + ' d ago';
  return new Date(ms).toLocaleDateString();
}

/** Shows a path under the home folder as ~/… */
function tilde(p) {
  const home = L.about.home_dir;
  return home && p.startsWith(home + '/') ? '~' + p.slice(home.length) : p;
}

function slug(name) {
  const s = name.trim().toLowerCase().replace(/[^a-z0-9._-]+/g, '-').replace(/-+/g, '-').replace(/^[-.]+|[-.]+$/g, '');
  return s || 'project';
}

/* ---------- sign-in ---------- */

/* ---------- look: shared with every project window ---------- */

const THEMES = { auto: 'Match system', light: 'Light', dark: 'Dark' };

/** Style, theme and spacing as saved with the engine, so the Start screen
 *  looks like the project windows. Browser storage only avoids a flash. */
function applyLook(r) {
  const root = document.documentElement;
  if (r.theme === 'light' || r.theme === 'dark') root.setAttribute('data-theme', r.theme);
  else root.removeAttribute('data-theme');
  if (r.style === 'classic') root.removeAttribute('data-style');
  else root.setAttribute('data-style', 'studio');
  root.setAttribute('data-density', r.density === 'roomy' ? 'roomy' : 'dense');
  L.look = { theme: r.theme || 'auto', style: r.style || 'studio', density: r.density || 'dense' };
  for (const [k, v] of Object.entries(L.look)) store('plonix.' + k, v);
  const btn = $('#themebtn');
  if (btn) btn.title = 'Theme: ' + THEMES[L.look.theme] + ' (click to change)';
}

function syncLook() {
  if (!L.token) return;
  api('/api/ui/style').then(applyLook).catch(() => {});
}

async function saveLook(values) {
  applyLook({ ...L.look, ...values });
  return api('/api/ui/style', { method: 'PUT', body: values });
}

function cycleTheme() {
  const next = { auto: 'light', light: 'dark', dark: 'auto' }[L.look.theme] || 'auto';
  saveLook({ theme: next }).catch((e) => toast(e.message, 'err'));
  toast('Theme: ' + THEMES[next]);
}

/** Appearance, as in a project's Settings: one look for every window. */
function appearanceSection() {
  return {
    id: 'appearance',
    title: 'Appearance',
    level: 'global',
    description: 'How Plonix looks on this computer, in every window.',
    applies: 'now',
    fields: [
      { key: 'theme', label: 'Theme', type: 'choice', options: Object.entries(THEMES).map(([value, label]) => ({ value, label })) },
      { key: 'density', label: 'Spacing', type: 'choice', help: 'Dense fits more on screen. Roomy gives rows and panels more air.', options: [{ value: 'dense', label: 'Dense' }, { value: 'roomy', label: 'Roomy' }] },
      { key: 'style', label: 'Style', type: 'choice', help: 'Studio is the standard Plonix look. Classic is the plainer indigo look.', options: [{ value: 'studio', label: 'Studio' }, { value: 'classic', label: 'Classic' }] },
    ],
    values: { ...L.look },
  };
}

async function boot() {
  applyLook({ theme: store('plonix.theme'), style: store('plonix.style'), density: store('plonix.density') });
  const hash = location.hash;
  if (hash.startsWith('#code=')) {
    history.replaceState(null, '', location.pathname);
    try {
      const r = await fetch('/ui/session', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ code: hash.slice(6) }) });
      const data = await r.json().catch(() => ({}));
      if (r.ok && data.token) store('plonix.hubtoken', data.token);
      else if (!store('plonix.hubtoken')) return showLock(data.error);
    } catch (_) {
      return showLock('Plonix is not running.');
    }
  }
  L.token = store('plonix.hubtoken');
  if (!L.token) return showLock();
  syncLook();
  addEventListener('focus', syncLook);
  let terms;
  try {
    [L.about, terms] = await Promise.all([api('/api/hub'), api('/api/terms')]);
  } catch (e) {
    if (e.status !== 401) showLock(e.message);
    return;
  }
  if (!terms.accepted) return showTerms(terms);
  renderShell();
  refresh();
}

/* ---------- first launch: license and terms ---------- */

const REPO = 'https://github.com/SergeyMalych/plonix/blob/main/';

/** Inline Markdown of TERMS.md: links and `code`, everything else as text. */
function inline(text) {
  const out = [];
  const re = /\[([^\]]+)\]\(([^)\s]+)\)|`([^`]+)`/g;
  let at = 0;
  for (let m; (m = re.exec(text)); at = re.lastIndex) {
    if (m.index > at) out.push(text.slice(at, m.index));
    if (m[3]) out.push(h('code', { text: m[3] }));
    else out.push(h('a', { href: /^https:\/\//.test(m[2]) ? m[2] : REPO + m[2], target: '_blank', rel: 'noopener', text: m[1] }));
  }
  out.push(text.slice(at));
  return out;
}

/** TERMS.md as headings and paragraphs. Its title is the card's own. */
function termsDoc(md) {
  const blocks = md.split(/\n\s*\n/).map((b) => b.trim()).filter(Boolean);
  return blocks.slice(1).map((b) => (b.startsWith('## ') ? h('h4', { text: b.slice(3) }) : h('p', null, inline(b.replace(/\s*\n\s*/g, ' ')))));
}

function showTerms(t) {
  clearTimeout(L.timer);
  const doc = h('div', { class: 'tdoc', tabindex: 0 });
  const tabs = h('div', { class: 'seg-ctl', role: 'tablist' });
  const showDoc = (which) => {
    for (const b of tabs.querySelectorAll('button')) b.classList.toggle('on', b.dataset.v === which);
    doc.scrollTop = 0;
    doc.replaceChildren(...(which === 'license' ? [h('pre', { text: t.license })] : termsDoc(t.terms)));
  };
  tabs.append(
    h('button', { class: 'segbtn', 'data-v': 'terms', text: 'Terms of use', onclick: () => showDoc('terms') }),
    h('button', { class: 'segbtn', 'data-v': 'license', text: 'License (Apache 2.0)', onclick: () => showDoc('license') }),
  );
  const off = t.usage_disabled_by_env;
  const go = h('button', { class: 'btn primary', text: 'Continue', disabled: true });
  const accept = h('input', { type: 'checkbox', id: 't-accept', onchange: () => (go.disabled = !accept.checked) });
  const share = h('input', { type: 'checkbox', id: 't-share', checked: t.share_usage && !off, disabled: off });
  const err = h('span', { class: 'err' });
  go.addEventListener('click', async () => {
    go.disabled = true;
    err.textContent = '';
    try {
      await api('/api/terms', { method: 'POST', body: { accept: accept.checked, share_usage: share.checked } });
      renderShell();
      refresh();
    } catch (e) {
      err.textContent = e.message;
      go.disabled = false;
    }
  });
  $('#app').replaceChildren(
    h(
      'div',
      { class: 'terms' },
      h(
        'div',
        { class: 'tcard', role: 'dialog', 'aria-labelledby': 't-title' },
        h(
          'div',
          { class: 'thead' },
          h('img', { src: '/ui/icon.svg', alt: '' }),
          h('div', null, h('h1', { id: 't-title', text: 'Welcome to Plonix' }), h('p', { text: 'Free and open source. Before you start, please read the license and the terms of use.' })),
        ),
        tabs,
        doc,
        h(
          'div',
          { class: 'tchecks' },
          h('label', { class: 'tcheck', for: 't-accept' }, accept, h('span', null, h('b', { text: 'I accept the license and terms' }))),
          h(
            'label',
            { class: 'tcheck', for: 't-share' },
            share,
            h(
              'span',
              null,
              h('b', { text: 'Share anonymous usage statistics' }),
              h(
                'small',
                null,
                off
                  ? 'Off: PLONIX_NO_ANALYTICS or DO_NOT_TRACK is set on this computer. '
                  : 'Once a day: how often features are used, the Plonix version, OS and CPU type. Never URLs, traffic, project names or anything you type. Change it any time in Settings. ',
                h('a', { href: t.privacy_url, target: '_blank', rel: 'noopener', text: 'Exactly what is sent' }),
              ),
            ),
          ),
        ),
        h('div', { class: 'tfoot' }, err, go),
      ),
    ),
  );
  showDoc('terms');
  accept.focus();
}

function showLock(message) {
  clearTimeout(L.timer);
  $('#app').replaceChildren(
    h(
      'div',
      { class: 'lock' },
      h(
        'div',
        { class: 'mcard' },
        h('img', { src: '/ui/icon.svg', alt: '' }),
        h('h1', { text: IN_APP ? 'Reopen Plonix' : 'Open Plonix from your terminal' }),
        message ? h('p', { text: message }) : null,
        IN_APP ? h('p', { text: 'Quit Plonix and open it again.' }) : [h('p', { text: 'For your safety this page only opens through a one-time link. Run:' }), h('pre', { text: 'plonix launcher' })],
      ),
    ),
  );
}

/* ---------- shell ---------- */

function renderShell() {
  $('#app').replaceChildren(
    h(
      'div',
      { class: 'titlebar' },
      h('div', { class: 'brand' }, h('img', { src: '/ui/icon.svg', alt: '' }), 'Plonix'),
      h(
        'div',
        { class: 'right' },
        h('button', { class: 'iconbtn', id: 'themebtn', title: 'Theme: ' + THEMES[L.look.theme] + ' (click to change)', onclick: cycleTheme }, h('span', { class: 'themeico' })),
        h('button', { class: 'iconbtn', title: 'Settings for all projects' + (IN_APP ? '  (⌘,)' : ''), onclick: globalSettings, text: '⚙' }),
      ),
    ),
    h(
      'main',
      { class: 'lmain' },
      h(
        'div',
        { class: 'lhead' },
        h('div', null, h('h1', { text: 'Projects' }), h('p', { class: 'lsub', text: 'Each project keeps its own traffic, scope, findings and settings in a folder you choose. Open several at once: each gets its own proxy.' })),
        h(
          'div',
          { class: 'lbtns' },
          h('button', { class: 'btn', text: 'Try the Demo', title: 'Explore a sample project: traffic captured from a made-up shop, with scope, findings and Bench experiments', onclick: () => openDemo() }),
          h('button', { class: 'btn', text: 'Add Existing…', title: 'Add a project folder that is not in the list', onclick: addExisting }),
          h('button', { class: 'btn primary', text: 'New Project', onclick: () => newProject() }),
        ),
      ),
      h('div', { class: 'plist', id: 'plist' }),
    ),
  );
}

async function refresh() {
  clearTimeout(L.timer);
  try {
    L.projects = await api('/api/projects');
    drawList();
  } catch (e) {
    if (e.status === 401) return;
  }
  L.timer = setTimeout(refresh, 2000);
}

/** How many recently opened projects the Start screen suggests. */
const RECENT = 3;

function drawList() {
  const box = $('#plist');
  if (!box) return;
  // Redraw only when something changed, so focus and hover survive the refresh.
  const sig = JSON.stringify(L.projects);
  if (sig === L.drawn && box.childElementCount) return;
  const first = L.drawn === undefined;
  L.drawn = sig;
  if (!L.projects.length) {
    box.replaceChildren(welcome());
    return;
  }
  // Suggest the last few projects first; everything else follows.
  const recent = L.projects
    .filter((p) => p.available && p.last_opened > 0)
    .sort((a, b) => b.last_opened - a.last_opened)
    .slice(0, RECENT);
  const rest = L.projects.filter((p) => !recent.includes(p));
  box.replaceChildren(
    ...[
      recent.length ? h('div', { class: 'lsec', text: 'Pick up where you left off' }) : null,
      recent.length ? h('div', { class: 'recent' }, recent.map(card)) : null,
      rest.length && recent.length ? h('div', { class: 'lsec', text: 'Other projects' }) : null,
      ...rest.map(row),
    ].filter(Boolean),
  );
  // On launch the most recent project is one Enter away.
  if (first && recent.length) box.querySelector('.rcard .btn')?.focus();
}

/** A suggested project: name, when it was last open, and one button. */
function card(p) {
  const open = !!p.session;
  return h(
    'div',
    { class: 'rcard', ondblclick: () => openProject(p) },
    h(
      'div',
      { class: 'rtop' },
      h('div', { class: 'pav', text: (p.name.trim()[0] || 'P').toUpperCase() }),
      h('button', { class: 'iconbtn', title: 'More', text: '⋯', onclick: (e) => (e.stopPropagation(), moreMenu(p, e.currentTarget)) }),
    ),
    h('div', { class: 'rname', title: p.name }, h('span', { text: p.name }), demoBadge(p)),
    h('div', { class: 'ppath mono', title: p.path, text: tilde(p.path) }),
    h(
      'div',
      { class: 'rfoot' },
      open
        ? h('span', { class: 'pstate open' }, h('span', { class: 'dot' }), 'Open · proxy ', h('b', { class: 'mono', text: p.session.proxy }))
        : h('span', { class: 'pstate', text: 'Opened ' + ago(p.last_opened) + (p.size_bytes ? ' · ' + fmtSize(p.size_bytes) : '') }),
      h('button', { class: 'btn ' + (open ? '' : 'primary'), text: open ? 'Show' : 'Open', onclick: (e) => (e.stopPropagation(), openProject(p)) }),
    ),
  );
}

function row(p) {
  const open = !!p.session;
  const initial = (p.name.trim()[0] || 'P').toUpperCase();
  const state = !p.available
    ? h('span', { class: 'pstate missing', text: 'Folder missing' })
    : open
      ? h('span', { class: 'pstate open', title: 'Open now, proxy ' + p.session.proxy }, h('span', { class: 'dot' }), 'Open · proxy ', h('b', { class: 'mono', text: p.session.proxy }))
      : h('span', { class: 'pstate', text: ago(p.last_opened) });
  const openBtn = h('button', { class: 'btn ' + (open ? '' : 'primary'), text: open ? 'Show' : 'Open', disabled: !p.available, onclick: (e) => (e.stopPropagation(), openProject(p)) });
  return h(
    'div',
    { class: 'prow' + (p.available ? '' : ' gone'), tabindex: 0, ondblclick: () => p.available && openProject(p), onkeydown: (e) => e.key === 'Enter' && p.available && openProject(p) },
    h('div', { class: 'pav', text: initial }),
    h(
      'div',
      { class: 'pinfo' },
      h('div', { class: 'pname' }, h('span', { text: p.name }), demoBadge(p), state),
      h('div', { class: 'ppath mono', title: p.path, text: tilde(p.path) }),
      p.warning ? h('div', { class: 'pwarn', text: p.warning }) : null,
    ),
    h('div', { class: 'pmeta', text: fmtSize(p.size_bytes) }),
    h(
      'div',
      { class: 'pact' },
      h('button', { class: 'iconbtn', title: 'Project settings', text: '⚙', disabled: !p.available, onclick: (e) => (e.stopPropagation(), projectSettings(p)) }),
      h('button', { class: 'iconbtn', title: 'More', text: '⋯', onclick: (e) => (e.stopPropagation(), moreMenu(p, e.currentTarget)) }),
      openBtn,
    ),
  );
}

function welcome() {
  const name = h('input', { placeholder: 'e.g. Acme staging', spellcheck: 'false', autocomplete: 'off' });
  const go = () => createProject({ name: name.value, location: '' }, null).catch((e) => toast(e.message, 'err'));
  name.addEventListener('keydown', (e) => e.key === 'Enter' && go());
  return h(
    'div',
    { class: 'welcome' },
    h('img', { src: '/ui/icon.svg', alt: '' }),
    h('h2', { text: 'Start your first project' }),
    h('p', { text: 'Give it a name, usually the target you are testing. You can change everything later.' }),
    h('div', { class: 'starter' }, name, h('button', { class: 'btn primary', text: 'Create and Open', onclick: go })),
    h('p', { class: 'small', text: 'It is saved in ' + tilde(L.about.projects_dir || '~/Plonix') + '. Choose another folder with New Project.' }),
    workPicker(),
    h(
      'div',
      { class: 'demohint' },
      h('span', { text: 'New to Plonix? Look around a sample project first: traffic from a made-up shop, already explored.' }),
      h('button', { class: 'btn', text: 'Try the Demo', onclick: () => openDemo() }),
    ),
  );
}

/** "What kind of work do you do?": one tap installs a starter set from the Market. */
function workPicker() {
  const row = h('div', { class: 'workpick' });
  const note = h('p', { class: 'small worknote', 'aria-live': 'polite' });
  const draw = (profiles, picked, busy) =>
    row.replaceChildren(
      ...[
      h('span', { class: 'worklabel', text: 'What kind of work do you do?' }),
      profiles.map((p) =>
        h('button', {
          class: 'chip' + (p.id === picked ? ' on' : ''),
          title: p.line,
          disabled: !!busy,
          text: p.title,
          onclick: () => choose(profiles, p),
        }),
      ),
      picked ? null : h('button', { class: 'chip ghost', text: 'Skip', disabled: !!busy, onclick: () => ((note.textContent = 'Skipped. You can pick later in Settings › Market.'), row.classList.add('skipped')) }),
      ]
        .flat()
        .filter(Boolean),
    );
  const choose = async (profiles, p) => {
    draw(profiles, p.id, true);
    note.textContent = 'Setting up a starter set for ' + p.title.toLowerCase() + 's…';
    try {
      const r = await api('/api/starter', { method: 'POST', body: { profile: p.id } });
      const got = (r.result.changes || []).filter((c) => c.action !== 'unchanged' && c.kind !== 'bundle').map((c) => c.name);
      const waiting = (r.result.waiting || []).map((w) => w.name);
      const parts = [];
      if (got.length) parts.push('Installed ' + got.join(', ') + '.');
      if (waiting.length) parts.push((waiting.length === 1 ? 'One extension is' : waiting.length + ' extensions are') + ' waiting for you under Recommended in the Market.');
      if (!parts.length) parts.push('You already have the starter set. More suggestions show up in the Market.');
      note.textContent = parts.join(' ');
    } catch (e) {
      note.textContent = e.message;
    }
    draw(profiles, p.id, false);
  };
  api('/api/starter')
    .then((d) => {
      draw(d.profiles, d.profile, false);
      if (d.profile) note.textContent = 'Market suggestions follow this. Change it any time in Settings › Market.';
    })
    .catch(() => row.remove());
  return h('div', { class: 'work' }, row, note);
}

function demoBadge(p) {
  return p.demo ? h('span', { class: 'pbadge', text: 'Demo', title: 'A sample project. Start it over from ⋯ whenever you like.' }) : null;
}

/* ---------- the demo project ---------- */

/** Opens the demo project, creating it the first time. */
async function openDemo() {
  try {
    const d = await api('/api/projects/demo', { method: 'POST', body: {} });
    await refresh();
    await openProject({ id: d.id, name: d.name });
  } catch (e) {
    toast(e.message, 'err');
  }
}

function restartDemo(p) {
  const m = modal(
    'Start the demo over?',
    [h('p', { class: 'muted', text: 'The demo project goes back to how it shipped: your changes to its scope, findings and Bench are replaced by a fresh copy.' })],
    [
      h('button', { class: 'btn', text: 'Cancel', onclick: closeModal }),
      h('button', {
        class: 'btn primary',
        text: 'Start Over',
        onclick: async () => {
          try {
            await api('/api/projects/demo', { method: 'POST', body: { fresh: true } });
            closeModal();
            toast('The demo project is fresh again.', 'ok');
            refresh();
          } catch (e) {
            m.err.textContent = e.message;
          }
        },
      }),
    ],
  );
}

/* ---------- actions ---------- */

async function openProject(p) {
  if (L.opening) return;
  L.opening = p.id;
  // A tab opened before the request keeps browsers from blocking it.
  const tab = IN_APP ? null : window.open('', 'plonix-' + p.id);
  try {
    const r = await api(`/api/projects/${p.id}/open`, { method: 'POST' });
    if (IN_APP) location.href = r.url;
    else if (tab) tab.location = r.url;
    else location.href = r.url;
    if (r.started) toast(`${p.name} is open. Its proxy listens on ${r.proxy}.`, 'ok');
    setTimeout(refresh, 300);
  } catch (e) {
    if (tab) tab.close();
    toast(e.message, 'err');
  } finally {
    L.opening = null;
  }
}

async function createProject(body, m) {
  if (!body.name.trim()) throw new Error('Give the project a name.');
  const p = await api('/api/projects', { method: 'POST', body });
  closeModal();
  await refresh();
  await openProject({ id: p.id, name: p.name });
  return p;
}

function newProject() {
  const name = h('input', { placeholder: 'e.g. Acme staging', spellcheck: 'false', autocomplete: 'off' });
  const loc = h('input', { class: 'mono', spellcheck: 'false', autocomplete: 'off', value: tilde(L.about.projects_dir || '') });
  const preview = h('div', { class: 'mnote mono' });
  const update = () => {
    const base = loc.value.trim().replace(/\/+$/, '') || tilde(L.about.projects_dir || '');
    preview.textContent = 'Folder: ' + base + '/' + slug(name.value || 'project');
  };
  name.addEventListener('input', update);
  loc.addEventListener('input', update);
  update();
  const pick = L.about.can_pick_folder
    ? h('button', {
        class: 'btn',
        type: 'button',
        text: 'Choose…',
        onclick: async () => {
          try {
            const r = await api('/api/pick-folder', { method: 'POST' });
            if (r.path) {
              loc.value = tilde(r.path.replace(/\/+$/, ''));
              update();
            }
          } catch (e) {
            m.err.textContent = e.message;
          }
        },
      })
    : null;
  const create = h('button', {
    class: 'btn primary',
    text: 'Create and Open',
    onclick: async () => {
      create.disabled = true;
      m.err.textContent = '';
      try {
        const home = L.about.home_dir;
        let location = loc.value.trim();
        if (home && location.startsWith('~')) location = home + location.slice(1);
        await createProject({ name: name.value, location }, m);
      } catch (e) {
        m.err.textContent = e.message;
        create.disabled = false;
      }
    },
  });
  name.addEventListener('keydown', (e) => e.key === 'Enter' && create.click());
  const m = modal(
    'New project',
    [
      h('label', null, 'Name', name),
      h('label', null, 'Location', h('div', { class: 'inrow' }, loc, pick)),
      preview,
      h('p', { class: 'muted mnote', text: 'The project folder holds its traffic database, settings and capture-browser profile. A local folder works best; avoid folders that sync to the cloud.' }),
    ],
    [h('button', { class: 'btn', text: 'Cancel', onclick: closeModal }), create],
  );
}

function addExisting() {
  const path = h('input', { class: 'mono', spellcheck: 'false', placeholder: '~/Plonix/acme-staging' });
  const add = async (value) => {
    let p = (value || path.value).trim();
    const home = L.about.home_dir;
    if (home && p.startsWith('~')) p = home + p.slice(1);
    try {
      const r = await api('/api/projects/add', { method: 'POST', body: { path: p } });
      closeModal();
      toast(`Added ${r.name}.`, 'ok');
      refresh();
    } catch (e) {
      m.err.textContent = e.message;
    }
  };
  const pick = L.about.can_pick_folder
    ? h('button', {
        class: 'btn',
        text: 'Choose…',
        onclick: async () => {
          const r = await api('/api/pick-folder', { method: 'POST' }).catch((e) => ((m.err.textContent = e.message), {}));
          if (r.path) {
            path.value = tilde(r.path.replace(/\/+$/, ''));
            add(r.path);
          }
        },
      })
    : null;
  path.addEventListener('keydown', (e) => e.key === 'Enter' && add());
  const m = modal(
    'Add an existing project',
    [h('label', null, 'Project folder (it contains plonix-project.json)', h('div', { class: 'inrow' }, path, pick))],
    [h('button', { class: 'btn', text: 'Cancel', onclick: closeModal }), h('button', { class: 'btn primary', text: 'Add', onclick: () => add() })],
  );
}

function moreMenu(p, anchor) {
  const items = [
    p.available && ['Settings…', () => projectSettings(p)],
    p.available && [IN_APP ? 'Show in Finder' : 'Show folder', () => api(`/api/projects/${p.id}/reveal`, { method: 'POST' }).catch((e) => toast(e.message, 'err'))],
    p.session && ['Close project', () => closeProject(p)],
    p.demo && !p.session && ['Start demo over…', () => restartDemo(p)],
    !p.session && ['Remove from list…', () => forgetProject(p)],
  ].filter(Boolean);
  const old = $('.popmenu');
  if (old) old.remove();
  const r = anchor.getBoundingClientRect();
  const menu = h(
    'div',
    { class: 'popmenu', style: `top:${r.bottom + 4}px; right:${window.innerWidth - r.right}px` },
    items.map(([label, fn]) => h('button', { text: label, onclick: () => (menu.remove(), fn()) })),
  );
  document.body.append(menu);
  setTimeout(() => document.addEventListener('mousedown', function off(e) {
    if (!menu.contains(e.target)) {
      menu.remove();
      document.removeEventListener('mousedown', off);
    }
  }), 0);
}

async function closeProject(p) {
  try {
    await api(`/api/projects/${p.id}/close`, { method: 'POST' });
    toast(`${p.name} is closed.`, 'ok');
  } catch (e) {
    toast(e.message, 'err');
  }
  refresh();
}

function forgetProject(p) {
  const m = modal(
    `Remove ${p.name} from the list?`,
    [h('p', { class: 'muted', text: `Its folder stays where it is (${tilde(p.path)}), with all its traffic. Add it back any time with Add Existing.` })],
    [
      h('button', { class: 'btn', text: 'Cancel', onclick: closeModal }),
      h('button', {
        class: 'btn danger',
        text: 'Remove from List',
        onclick: async () => {
          try {
            await api(`/api/projects/${p.id}/forget`, { method: 'POST' });
            closeModal();
            refresh();
          } catch (e) {
            m.err.textContent = e.message;
          }
        },
      }),
    ],
  );
}

async function projectSettings(p) {
  let data;
  try {
    data = await api(`/api/projects/${p.id}/settings`);
  } catch (e) {
    return toast(e.message, 'err');
  }
  const name = h('input', { value: data.project.name, spellcheck: 'false' });
  const rename = h('button', {
    class: 'btn sm',
    text: 'Rename',
    onclick: async () => {
      try {
        await api(`/api/projects/${p.id}/settings/general`, { method: 'PUT', body: { values: { name: name.value } } });
        toast('Renamed.', 'ok');
        refresh();
      } catch (e) {
        toast(e.message, 'err');
      }
    },
  });
  const box = h('div', { class: 'settings-host' });
  modal(
    `${data.project.name} · Settings`,
    [
      h('div', { class: 'inrow nameedit' }, h('span', { class: 'flabel', text: 'Name' }), name, rename),
      h('div', { class: 'mnote mono', text: tilde(data.project.dir) }),
      p.session ? h('div', { class: 'mnote', text: 'This project is open. Proxy changes apply to it right away.' }) : null,
      box,
    ],
    [h('button', { class: 'btn', text: 'Done', onclick: closeModal })],
    true,
  );
  PlonixSettings.render(box, data, {
    only: ['project'],
    save: (section, values) => api(`/api/projects/${p.id}/settings/${section}`, { method: 'PUT', body: { values } }),
    extra: (section, el) => {
      if (section.id === 'storage' && data.project.last_prune) {
        const r = data.project.last_prune;
        el.append(h('p', { class: 'mnote', text: r.skipped ? `Last close: ${r.skipped}.` : `Last close: deleted ${r.removed} out-of-scope request(s), kept ${r.kept}.` }));
      }
    },
  });
}

async function globalSettings() {
  let data;
  try {
    data = await api('/api/settings');
  } catch (e) {
    return toast(e.message, 'err');
  }
  const box = h('div', { class: 'settings-host' });
  modal('Settings for all projects', [box, h('p', { class: 'mnote', text: 'Each project also has its own settings (proxy, storage): open them with ⚙ on its row.' })], [h('button', { class: 'btn', text: 'Done', onclick: closeModal })], true);
  data.sections = [appearanceSection(), ...(data.sections || [])];
  PlonixSettings.render(box, data, {
    only: ['global'],
    save: async (section, values) => {
      if (section === 'appearance') {
        await saveLook(values);
        return { applies: 'now' };
      }
      const r = await api(`/api/settings/${section}`, { method: 'PUT', body: { values } });
      L.about = await api('/api/hub');
      return r;
    },
  });
}

document.addEventListener('keydown', (e) => {
  if (e.key === 'Escape') {
    const pm = $('.popmenu');
    if (pm) return pm.remove();
    closeModal();
  }
});

// Entry points for the Plonix app's menu bar.
window.plonixLauncher = {
  newProject: () => L.token && $('#plist') && newProject(),
  settings: () => L.token && $('#plist') && globalSettings(),
};

boot();
