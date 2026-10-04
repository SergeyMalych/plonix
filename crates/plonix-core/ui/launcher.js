/* The Plonix Start screen: pick a project, create one, change its settings.
 * Each project opens in its own window (in the app) or browser tab, served
 * by a session of its own. */
'use strict';

const h = PlonixSettings.h;
const $ = (sel, root = document) => root.querySelector(sel);
const IN_APP = !!window.__PLONIX_APP__;

function store(key, value) {
  try {
    if (value === undefined) return JSON.parse(localStorage.getItem(key));
    if (value === null) localStorage.removeItem(key);
    else localStorage.setItem(key, JSON.stringify(value));
  } catch (_) {
    return null;
  }
}

const L = { token: null, about: {}, projects: [], timer: null, opening: null };

class ApiError extends Error {
  constructor(status, data) {
    super((data && data.error) || 'Request failed');
    this.status = status;
    this.code = data && data.code;
    this.problems = data && data.problems;
  }
}

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
    throw new ApiError(0, { error: 'Plonix is not running. Open it again.' });
  }
  const data = await resp.json().catch(() => null);
  if (resp.status === 401) {
    store('plonix.hubtoken', null);
    showLock('This page has signed out.');
    throw new ApiError(401, data);
  }
  if (!resp.ok) throw new ApiError(resp.status, data);
  return data;
}

/* ---------- helpers ---------- */

function toast(msg, kind = '') {
  let box = $('.toasts');
  if (!box) document.body.append((box = h('div', { class: 'toasts' })));
  const t = h('div', { class: 'toast ' + kind, text: msg });
  box.append(t);
  setTimeout(() => t.remove(), kind === 'err' ? 7000 : 3200);
}

function closeModal() {
  const m = $('.modal');
  if (m) m.remove();
}

function modal(title, body, actions, wide) {
  closeModal();
  const err = h('span', { class: 'err' });
  const m = h(
    'div',
    { class: 'modal', onmousedown: (e) => e.target === m && closeModal() },
    h('div', { class: 'mcard' + (wide ? ' wide' : ''), role: 'dialog' }, h('h3', { text: title }), h('div', { class: 'mb' }, body), h('div', { class: 'mf' }, err, actions)),
  );
  document.body.append(m);
  const first = m.querySelector('input, textarea, select');
  if (first) first.focus();
  return { el: m, err };
}

function tilde(p) {
  const home = L.about.home_dir;
  return home && p.startsWith(home + '/') ? '~' + p.slice(home.length) : p;
}

function ago(ms) {
  if (!ms) return 'never opened';
  const s = Math.max(0, (Date.now() - ms) / 1000);
  if (s < 60) return 'just now';
  if (s < 3600) return Math.floor(s / 60) + ' min ago';
  if (s < 86400) return Math.floor(s / 3600) + ' h ago';
  if (s < 86400 * 30) return Math.floor(s / 86400) + ' d ago';
  return new Date(ms).toLocaleDateString();
}

function fmtSize(n) {
  if (n == null) return '';
  if (n < 1024) return n + ' B';
  if (n < 1024 * 1024) return (n / 1024).toFixed(0) + ' KB';
  if (n < 1024 * 1024 * 1024) return (n / 1024 / 1024).toFixed(1) + ' MB';
  return (n / 1024 / 1024 / 1024).toFixed(2) + ' GB';
}

function slug(name) {
  const s = name.trim().toLowerCase().replace(/[^a-z0-9._-]+/g, '-').replace(/-+/g, '-').replace(/^[-.]+|[-.]+$/g, '');
  return s || 'project';
}

/* ---------- sign-in ---------- */

async function boot() {
  const t = store('plonix.theme');
  if (t && t !== 'auto') document.documentElement.setAttribute('data-theme', t);
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
  try {
    L.about = await api('/api/hub');
  } catch (e) {
    if (e.status !== 401) showLock(e.message);
    return;
  }
  renderShell();
  refresh();
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
    h(
      'div',
      { class: 'demohint' },
      h('span', { text: 'New to Plonix? Look around a sample project first: traffic from a made-up shop, already explored.' }),
      h('button', { class: 'btn', text: 'Try the Demo', onclick: () => openDemo() }),
    ),
  );
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
  PlonixSettings.render(box, data, {
    only: ['global'],
    save: async (section, values) => {
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
