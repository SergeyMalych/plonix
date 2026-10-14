// Plonix window: Saved users, the Access check and Callbacks.
// Part of the project window's scripts (see index.html for the load order).
'use strict';

/* ======================================================================
   Saved users and the Access check
   Saved users keep each person's cookies and headers. The person acts as one
   of them at a time (the switcher in the title bar): browser traffic to
   in-scope hosts, Bench tabs and Scans are then sent as that user. The Users
   screen shows every cookie, to edit, expire or remove. The Access check
   replays chosen requests as every user, and once signed out.
   ====================================================================== */

/** Loads the saved users for this project and who the person acts as, cached on S. */
async function loadUsers(force) {
  if (S.users && !force) return S.users;
  try {
    const r = await api('/api/users');
    S.users = r.users || [];
    S.acting = r.acting || null;
  } catch (_) {
    S.users = [];
    S.acting = null;
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

const userById = (id) => (S.users || []).find((u) => u.id === id) || null;
const actingUser = () => (S.acting ? userById(S.acting) : null);
const nowSecs = () => Math.floor(Date.now() / 1000);
const cookieLive = (c) => c.expires == null || c.expires > nowSecs();
/** "Dana" for "Dana (another customer)". */
const userShortName = (u) => u.name.replace(/\s*\(.*\)$/, '').trim() || u.name;
/** The first letter of a user's name, for its round badge. */
const userInitial = (u) => ((u && u.name.trim()[0]) || '?').toUpperCase();
/** A steady hue per user, so each badge keeps its colour. */
const USER_HUES = [232, 162, 282, 18, 196, 328, 96];
const userHue = (u) => USER_HUES[[...((u && u.id) || '')].reduce((a, c) => (a * 31 + c.charCodeAt(0)) % 9973, 7) % USER_HUES.length];
function userBadge(u, cls = '') {
  const b = h('span', { class: 'ubadge ' + cls, text: u ? userInitial(u) : '' });
  if (u) b.style.setProperty('--uh', userHue(u));
  return b;
}

/** Saves every user as they stand on S.users. `keep` leaves the objects the
 * Users screen is editing in place (taking only the ids the engine gave),
 * so typing carries on into the next save. */
async function saveUsers(keep) {
  S.users.forEach(syncHeaderRows);
  FOUND.byEx.clear();
  const r = await api('/api/users', { method: 'PUT', body: { users: S.users.map(({ _rows, ...u }) => u) } });
  if (keep && (r.users || []).length === S.users.length) r.users.forEach((u, i) => (S.users[i].id = u.id));
  else S.users = r.users || [];
  drawActing();
  return S.users;
}

/** Picks the user to act as, or none (null): the browser's own session. Sends nothing. */
async function setActing(id) {
  try {
    const r = await api('/api/users/acting', { method: 'PUT', body: { id } });
    S.acting = r.acting || null;
  } catch (e) {
    return toast(e.message, 'err');
  }
  drawActing();
  const u = actingUser();
  toast(u ? `Acting as ${u.name}: your browser, the Bench and Scans now send as them.` : 'Back to your browser’s own session.', 'ok');
  if (S.view === 'users' || S.view === 'bench' || S.view === 'scans') go(S.view, true);
}

/** The title bar pill: who the person is acting as. */
function drawActing() {
  const b = $('#actas');
  if (!b) return;
  const u = actingUser();
  b.classList.toggle('on', !!u);
  b.title = u ? `Acting as ${u.name}. Browser traffic to in-scope hosts, the Bench and Scans are sent with their cookies.` : 'Act as a saved user: send as someone else from your browser, the Bench and Scans';
  clear(b, u ? userBadge(u) : h('span', { class: 'ubadge none' }), h('span', { class: 'aslbl', text: u ? u.name : 'Your browser' }), h('span', { class: 'caret', text: '▾' }));
}

/** The menu under the title bar pill. */
function actingMenu(anchor) {
  closePopover();
  const item = (u) =>
    h(
      'button',
      { role: 'menuitemradio', class: 'actitem' + ((u ? u.id : null) === (S.acting || null) ? ' on' : ''), onclick: () => (closePopover(), setActing(u ? u.id : null)) },
      u ? userBadge(u) : h('span', { class: 'ubadge none' }),
      h('span', { class: 'actname' }, h('b', { text: u ? u.name : 'Your browser' }), h('span', { class: 'muted', text: u ? userSummary(u) : 'The cookies your browser has' })),
      h('span', { class: 'actcheck', text: (u ? u.id : null) === (S.acting || null) ? '✓' : '' }),
    );
  const menu = h(
    'div',
    { class: 'ctxmenu actmenu', role: 'menu' },
    h('div', { class: 'mhead', text: 'Act as' }),
    item(null),
    (S.users || []).map(item),
    (S.users || []).length ? null : h('div', { class: 'mnote', text: 'No saved users yet. Add one, or save a login from the Lens.' }),
    h('div', { class: 'msep' }),
    h('button', { role: 'menuitem', text: 'Manage users and cookies…', onclick: () => (closePopover(), leaveTo('users')) }),
  );
  const r = anchor.getBoundingClientRect();
  showPopover(menu, { left: Math.max(8, r.right - 300), bottom: r.bottom });
}

/** "3 cookies · 1 expired", for menus and the user list. */
function userSummary(u) {
  const live = (u.cookies || []).filter(cookieLive).length;
  const dead = (u.cookies || []).length - live;
  const bits = [live === 1 ? '1 cookie' : live + ' cookies'];
  if (dead) bits.push(dead + ' expired');
  const names = {};
  for (const n of [...(u.headers || []).map((x) => x[0]), ...(u.scoped || []).map((x) => x.name)]) names[n] = (names[n] || 0) + 1;
  const hs = Object.entries(names).map(([n, c]) => (c > 1 ? `${n} ×${c}` : n));
  if (hs.length) bits.push(hs.join(', '));
  return bits.join(' · ');
}

/** A user's headers as one editable list: each with the host and path it is
 * sent to (blank: every in-scope host). Kept on the user while it is edited
 * and split back into `headers` and `scoped` when saved. */
function headerRows(u) {
  if (!u._rows) {
    u._rows = [...(u.headers || []).map(([name, value]) => ({ name, value, host: '', path: '' })), ...(u.scoped || []).map((x) => ({ name: x.name, value: x.value, host: x.host, path: x.path || '' }))];
  }
  return u._rows;
}

function syncHeaderRows(u) {
  if (!u._rows) return;
  const where = (r) => (r.host || '').trim() || (r.path || '').trim();
  u.headers = u._rows.filter((r) => !where(r)).map((r) => [r.name, r.value]);
  // A path needs its host; until the host is typed the row is not saved yet.
  u.scoped = u._rows.filter((r) => (r.host || '').trim()).map((r) => ({ name: r.name, value: r.value, host: (r.host || '').trim(), path: (r.path || '').trim() }));
}

/** "api.shop.example/v2/seller", or what a blank place means. */
const placeText = (host, path) => (host ? host + (path || '') : 'every in-scope host');

/** "in 3 h", "expired 2 d ago", or "no expiry". */
function expiryText(t) {
  if (t == null) return 'no expiry';
  const d = t - nowSecs();
  const span = (n) => (n < 3600 ? Math.max(1, Math.round(n / 60)) + ' min' : n < 172800 ? Math.round(n / 3600) + ' h' : Math.round(n / 86400) + ' d');
  return d > 0 ? 'in ' + span(d) : 'expired ' + span(-d) + ' ago';
}

/** Adds a user (from the Lens, or a blank one) and opens it on the Users screen. */
async function manageUsers(afterSave, prefill) {
  if (!prefill) return leaveTo('users');
  await loadUsers(true);
  S.users.push({ id: '', name: prefill.name || 'New user', note: prefill.note || '', headers: prefill.headers || [], cookies: [], keep_fresh: true });
  try {
    await saveUsers();
  } catch (e) {
    S.users.pop();
    return toast(e.message, 'err');
  }
  US.sel = S.users[S.users.length - 1].id;
  if (afterSave) afterSave();
  leaveTo('users');
}

/** The Bench control that picks which saved user a request is sent as. A tab
 * follows the user the person acts as until one is picked for it. */
function tabUser(tab) {
  const id = tab.asUser != null ? tab.asUser : S.acting || '';
  return id && userById(id) ? id : '';
}

function userSwitcher(tab, main) {
  const sel = h('select', {
    class: 'assel',
    title: 'Send this request as a saved user. Tabs follow the user you act as (title bar) until you pick one here.',
    onchange: () => {
      if (sel.value === '__manage') {
        sel.value = tabUser(tab);
        return leaveTo('users');
      }
      tab.asUser = sel.value;
      saveBench();
      renderBench(main);
    },
  });
  const opts = [h('option', { value: '', text: 'As written' })];
  for (const u of S.users || []) opts.push(h('option', { value: u.id, text: 'As ' + u.name }));
  opts.push(h('option', { value: '__manage', text: 'Manage users…' }));
  append(sel, opts);
  sel.value = tabUser(tab);
  return h('span', { class: 'asbox' }, h('span', { class: 'aslbl', text: '⚿' }), sel);
}

/* ---- users found in traffic ----
   The sessions captured browser traffic carries, grouped by person: a cookie
   for the pages, a token for the API, another for a back office. Plonix
   finds them; the person decides which user each one goes to. */

const FOUND = { list: null, byEx: new Map() };
const foundKey = (sel) => (sel && sel.startsWith('found:') ? sel.slice(6) : null);
const foundByKey = (key) => (FOUND.list || []).find((f) => f.key === key) || null;
/** Sessions with something no saved user sends yet. */
const foundNew = () => (FOUND.list || []).filter((f) => f.fresh > 0);

async function loadFound() {
  try {
    FOUND.list = (await api('/api/users/found')).sessions || [];
  } catch (_) {
    FOUND.list = [];
  }
  FOUND.byEx.clear();
  return FOUND.list;
}

/** The session a captured request carried, if it has anything new. Cached per request. */
async function foundFor(id) {
  if (FOUND.byEx.has(id)) return FOUND.byEx.get(id);
  let f = null;
  try {
    f = ((await api('/api/users/found?exchange=' + id)).sessions || []).find((x) => x.fresh > 0) || null;
  } catch (_) {}
  FOUND.byEx.set(id, f);
  return f;
}

/** What a found session is called: who it is, or where it was used. */
const foundTitle = (f) => f.label || 'A session on ' + (f.credentials[0] || {}).host;
const credPlace = (c) => (c.kind === 'cookie' ? c.host + ' and its subdomains' : c.host + (c.path || ''));
const credKind = (c) => (c.kind === 'cookie' ? 'Cookie' : /^authorization$/i.test(c.name) && /^Bearer /i.test(c.preview) ? 'Bearer token' : 'Header');

/** From a Traffic row: shows the session the request carried, to keep. */
async function keepSessionOf(ex) {
  if (!(await useTool('saved-users', 'Keep this session as a user'))) return;
  FOUND.byEx.delete(ex.id);
  const f = await foundFor(ex.id);
  if (!f) return toast('Every token this request sent is already with a saved user.', 'ok');
  reviewFound(f.key);
}

/** Opens a found session on the Users screen, to look at before keeping it. */
function reviewFound(key) {
  US.sel = 'found:' + key;
  if (S.view === 'users') return drawUsers();
  leaveTo('users');
}

/** Keeps a found session with a saved user (`userId`), or as a new user. */
async function applyFound(f, userId) {
  let r;
  try {
    r = await api('/api/users/found/apply', { method: 'POST', body: { key: f.key, user: userId || null } });
  } catch (e) {
    return toast(e.message, 'err');
  }
  S.users = r.users || [];
  US.sel = r.user;
  drawActing();
  await Promise.all([loadFound(), loadStanding()]);
  const u = userById(r.user);
  if (u) toast(`${userId ? 'Added ' + plural(r.added, 'credential') + ' to' : 'Saved'} ${u.name}. Each one is sent only where it was found.`, 'ok');
  drawUsers();
  return u;
}

function foundItem(f) {
  const owner = f.user && userById(f.user);
  return h(
    'button',
    { class: 'usitem found' + ('found:' + f.key === US.sel ? ' on' : ''), onclick: () => ((US.sel = 'found:' + f.key), drawUsers()) },
    h('span', { class: 'ubadge found', text: '+' }),
    h(
      'span',
      { class: 'usitemtext' },
      h('b', { text: foundTitle(f) }),
      h('span', { class: 'muted', text: owner ? `${plural(f.fresh, 'new token')} for ${userShortName(owner)}` : `${plural(f.credentials.length, 'credential')} · ${f.requests} requests` }),
    ),
  );
}

function foundDetail(f) {
  const owner = f.user && userById(f.user);
  const others = (S.users || []).filter((u) => u.id !== f.user);
  const when = (t) => new Date(t).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
  const pick = h(
    'select',
    { class: 'fdpick', title: 'Add this session to another saved user', onchange: (e) => e.target.value && applyFound(f, e.target.value) },
    h('option', { value: '', text: 'Add to another user…' }),
    others.map((u) => h('option', { value: u.id, text: u.name })),
  );
  const row = (c) =>
    h(
      'div',
      { class: 'fdrow' + (c.new ? '' : ' saved') },
      h('span', { class: 'fdkind', text: credKind(c) }),
      h('span', { class: 'mono fdname', text: c.name }),
      h('span', { class: 'mono fdval', text: c.preview, title: 'Shortened. The full value goes to the user when you keep it.' }),
      h('span', { class: 'fdwhere', title: c.kind === 'cookie' ? 'Sent to this domain and its subdomains' : c.path ? `Sent only to requests under ${c.path} on ${c.host}` : `Sent to every request to ${c.host}` }, h('b', { text: c.host }), c.kind === 'header' && c.path ? h('span', { class: 'fdpath', text: c.path }) : null, c.kind === 'cookie' ? h('span', { class: 'muted', text: ' + subdomains' }) : null),
      h('span', { class: 'fdseen muted', text: c.requests ? plural(c.requests, 'request') : 'set by the server' }),
      c.new ? h('span', { class: 'ustag new', text: 'new' }) : h('span', { class: 'muted fdhas', text: owner ? 'has it' : '' }),
    );
  const hosts = [...new Set(f.credentials.map((c) => c.host))];
  return h(
    'div',
    { class: 'usdetail fdetail' },
    h(
      'div',
      { class: 'ushead' },
      h('span', { class: 'ubadge found lg', text: '+' }),
      h(
        'div',
        { class: 'usnames' },
        h('div', { class: 'fdtitle', text: foundTitle(f) }),
        h('div', { class: 'muted fdsub', text: [f.role, `found in ${plural(f.requests, 'captured request')}`, `last ${when(f.last_ts)}`].filter(Boolean).join(' · ') }),
      ),
      owner
        ? h('button', { class: 'btn sm primary', text: `Add ${plural(f.fresh, 'token')} to ${userShortName(owner)}`, title: `Keep the new ones with ${owner.name}. Each is sent only to the part of the app it was found on.`, onclick: () => applyFound(f, owner.id) })
        : h('button', { class: 'btn sm primary', text: 'Save as a new user', title: 'Make a saved user with this session', onclick: () => applyFound(f, null) }),
      owner ? h('button', { class: 'btn sm', text: 'Save as a new user', onclick: () => applyFound(f, null) }) : null,
      others.length ? pick : null,
    ),
    h(
      'div',
      { class: 'fdnote' },
      owner
        ? `${owner.name} already has some of these, so this is their session. The new ones are what other parts of the app use: ${hosts.length > 1 ? 'other hosts' : 'other paths'}, other tokens. Nothing is added until you say so.`
        : 'No saved user has this session yet. These were sent together, handed out for one another or name the same account, so they belong to one person. Nothing is saved until you say so.',
    ),
    h(
      'section',
      { class: 'ussec' },
      h('div', { class: 'ussech' }, h('h3', { text: 'What this session sends, and where' }), h('span', { class: 'spacer' }), h('button', { class: 'btn xs', text: 'Open a request', title: 'Show one request that carried this session', onclick: () => showExchange(f.sample) })),
      h('div', { class: 'fdgrid' }, h('div', { class: 'fdrow head' }, ['Kind', 'Name', 'Value', 'Sent to', 'Seen', ''].map((t) => h('span', { text: t }))), f.credentials.map(row)),
      f.elsewhere.length
        ? h('div', { class: 'muted fdelse', text: `Also sent to ${andList(f.elsewhere)}, outside scope, so not kept. Add ${f.elsewhere.length > 1 ? 'them' : 'it'} to scope to keep ${f.elsewhere.length > 1 ? 'their' : 'its'} tokens too.` })
        : null,
    ),
  );
}

/** On a saved user: their session in the traffic has tokens they lack. */
function foundBanner(u) {
  const f = foundNew().find((x) => x.user === u.id);
  if (!f) return null;
  const what = f.credentials.filter((c) => c.new).map((c) => `${c.name} for ${credPlace(c)}`);
  return h(
    'div',
    { class: 'fdbanner' },
    h('span', { class: 'ubadge found', text: '+' }),
    h('span', { class: 'fdbtext' }, h('b', { text: `Found in traffic: ${plural(f.fresh, 'more token')} of ${userShortName(u)}’s. ` }), what.slice(0, 3).join(', ') + (what.length > 3 ? '…' : '')),
    h('button', { class: 'btn xs', text: 'Review', onclick: () => reviewFound(f.key) }),
    h('button', { class: 'btn xs primary', text: 'Add them', onclick: () => applyFound(f, u.id) }),
  );
}

/* ---- whether a user's session still works, and what it may do ----
   Read from what Plonix already holds: the expiry each token or cookie
   states, how the server answered requests sent with them, and the roles
   and scopes the tokens and "who am I" answers name. Nothing is sent to
   find out unless the person presses Check now. */

const STAND = { byUser: new Map(), checking: null };

async function loadStanding() {
  try {
    const list = (await api('/api/users/standing')).users || [];
    STAND.byUser = new Map(list.map((x) => [x.user, x]));
  } catch (_) {
    STAND.byUser = new Map();
  }
  return STAND.byUser;
}

/** Each state's word and tone. */
const STATE_LOOK = {
  working: ['Working', 'ok'],
  valid: ['Not expired', 'ok'],
  expiring: ['Expiring soon', 'warn'],
  partly: ['Partly expired', 'warn'],
  refused: ['Refused', 'crit'],
  expired: ['Expired', 'crit'],
  unknown: ['Can’t tell', 'none'],
};

/** Shorter words for the user list, where "can't tell" shows nothing. */
const STATE_SHORT = { working: 'Works', valid: 'Valid', expiring: 'Expiring', partly: 'Partly', refused: 'Refused', expired: 'Expired' };

function stateTag(st, cls = '') {
  if (!st) return null;
  const [word, tone] = STATE_LOOK[st.state] || STATE_LOOK.unknown;
  if (cls === 'sm' && !STATE_SHORT[st.state]) return null;
  return h('span', { class: `usst ${tone} ${cls}`, text: cls === 'sm' ? STATE_SHORT[st.state] : word, title: cls === 'sm' ? word : null });
}

/** "GET /v1/me". */
const answerText = (a) => `${a.method} ${a.path}`;
/** "10:42", or a date for older ones. */
const whenText = (ms) => (Date.now() - ms < 20 * 3600e3 ? new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }) : new Date(ms).toLocaleString([], { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' }));
const credLabel = (c) => (c.kind === 'cookie' ? `the ${c.name} cookie` : /^authorization$/i.test(c.name) ? 'the Authorization token' : `the ${c.name} header`);
const SOURCE_TEXT = { 'exp claim': 'its exp claim', 'cookie expiry': 'the cookie’s expiry', expires_in: 'expires_in in the answer that handed it out' };

/** What the state rests on, in a sentence or two. */
function standingText(u, st) {
  const who = userShortName(u);
  const dead = st.credentials.filter((c) => c.expired);
  const soon = st.credentials.filter((c) => !c.expired && c.expires != null).sort((a, b) => a.expires - b.expires)[0];
  const ranOut = (c) => `${credLabel(c)} ran out ${expiryText(c.expires).replace('expired ', '')} (${SOURCE_TEXT[c.source] || 'its expiry'})`;
  const until = soon ? ` ${credLabel(soon)[0].toUpperCase() + credLabel(soon).slice(1)} runs out ${expiryText(soon.expires)}.` : '';
  const acc = st.accepted ? `the server accepted this session at ${whenText(st.accepted.at)} (${answerText(st.accepted)} → ${st.accepted.status})` : '';
  switch (st.state) {
    case 'working':
      return `${acc[0].toUpperCase() + acc.slice(1)}.${until}`;
    case 'valid':
      return `Nothing sent as ${who} has been answered yet, but nothing has run out.${until}`;
    case 'expiring':
      return `${soon ? credLabel(soon)[0].toUpperCase() + credLabel(soon).slice(1) + ' runs out ' + expiryText(soon.expires) : 'A credential runs out soon'}. ${acc ? acc[0].toUpperCase() + acc.slice(1) + '.' : ''}`;
    case 'partly': {
      const bits = dead.map(ranOut);
      if (st.refused) bits.push(`${answerText(st.refused)} was refused at ${whenText(st.refused.at)} (${st.refused.status}${st.refused.why ? ' “' + st.refused.why + '”' : ''})`);
      return `${bits.join('; ')[0].toUpperCase() + bits.join('; ').slice(1)}.${acc ? ' Elsewhere ' + acc + '.' : ''}`;
    }
    case 'refused':
      return `The server turned ${who} away at ${whenText(st.refused.at)}: ${answerText(st.refused)} → ${st.refused.status}${st.refused.why ? ' “' + st.refused.why + '”' : ''}${st.accepted ? `, where it accepted the same session at ${whenText(st.accepted.at)}` : ''}. Sign in again as ${who}, or paste ${st.credentials.some((c) => c.kind === 'cookie') ? 'fresh cookies' : 'a fresh token'}.`;
    case 'expired':
      return `${dead.map(ranOut).join('; ').replace(/^./, (c) => c.toUpperCase())}. The server will most likely refuse it.${st.probe ? ' Check now to be sure.' : ''}`;
    default:
      return st.credentials.length
        ? `Can’t tell yet: its ${st.credentials.length === 1 ? 'credential states' : 'credentials state'} no expiry, and no answer to a request sent as ${who} is in the traffic.${st.probe ? ' Check now asks the server once.' : ''}`
        : `Can’t tell: ${who} has no session cookie or token yet.`;
  }
}

/** Replays one captured request as the user, once, to see whether the server still takes the session. */
async function checkStanding(u, st) {
  STAND.checking = u.id;
  drawUsers();
  let cell = null;
  try {
    const r = await api('/api/access-check', { method: 'POST', body: { targets: [st.probe.exchange], user_ids: [u.id], include_anon: false } });
    cell = ((r.rows || [])[0] || { cells: [] }).cells[0] || null;
  } catch (e) {
    toast(e.message, 'err');
  }
  await loadStanding();
  STAND.checking = null;
  if (cell) toast(`Sent ${answerText(st.probe)} as ${u.name}: ${cell.error ? cell.error : cell.status}.`, cell.status && cell.status < 300 ? 'ok' : 'err');
  drawUsers();
  return cell;
}

const GRANT_KINDS = [
  ['role', 'Roles'],
  ['scope', 'Scopes'],
  ['group', 'Groups'],
  ['permission', 'Permissions'],
  ['flag', 'Flags'],
];

function standingSection(u) {
  const st = STAND.byUser.get(u.id);
  if (!st) return null;
  const busy = STAND.checking === u.id;
  const check = st.probe
    ? h('button', {
        class: 'btn xs' + (['expired', 'unknown', 'valid', 'refused'].includes(st.state) ? ' primary' : ''),
        text: busy ? 'Checking…' : 'Check now',
        disabled: busy,
        title: `Send ${answerText(st.probe)} to ${st.probe.host} once as ${u.name}, to see whether the server still takes this session`,
        onclick: () => checkStanding(u, st),
      })
    : null;
  const lives = st.credentials.filter((c) => c.expires != null);
  const life = (c) =>
    h(
      'div',
      { class: 'uslife' + (c.expired ? ' dead' : '') },
      h('span', { class: 'mono', text: c.name }),
      h('span', { class: 'muted', text: c.place || (c.kind === 'cookie' ? 'any in-scope host' : 'every in-scope host') }),
      h('span', { class: 'usexp' + (c.expired ? ' dead' : ''), title: new Date(c.expires * 1000).toLocaleString(), text: expiryText(c.expires) }),
      h('span', { class: 'muted', text: SOURCE_TEXT[c.source] ? 'from ' + SOURCE_TEXT[c.source] : '' }),
    );

  const groups = GRANT_KINDS.map(([k, label]) => [label, st.grants.filter((g) => g.kind === k)]).filter(([, gs]) => gs.length);
  const seenRow = (label, list, total, tone) =>
    list.length
      ? h(
          'div',
          { class: 'usgrant' },
          h('span', { class: 'usglbl', text: `${label}${total > list.length ? ` (${total})` : ''}` }),
          h(
            'span',
            { class: 'usgchips' },
            list.map((a) => h('button', { class: `usseen ${tone}`, title: `${a.host}${a.path} · ${whenText(a.at)} · open this request`, onclick: () => showExchange(a.exchange) }, h('span', { class: 'meth m-' + a.method, text: a.method }), h('span', { class: 'mono', text: a.path }), h('b', { text: a.status }))),
          ),
        )
      : null;
  const nothingSeen = !st.allowed.length && !st.blocked.length;

  return h(
    'section',
    { class: 'ussec usstate' },
    h('div', { class: 'ussech' }, h('h3', { text: 'Session' }), stateTag(st), h('span', { class: 'spacer' }), check),
    h('div', { class: 'usstxt' }, standingText(u, st)),
    st.late_accept ? h('div', { class: 'usstxt warn' }, h('b', { text: 'Worth a look: ' }), 'the server accepted this session after its stated expiry had passed. An expired token should be refused.') : null,
    lives.length ? h('div', { class: 'uslives' }, lives.map(life)) : null,
    h('div', { class: 'ussech sub' }, h('h3', { text: 'What it may do' }), h('span', { class: 'muted', text: 'read from its tokens, “who am I” answers and the traffic' })),
    groups.length
      ? groups.map(([label, gs]) => h('div', { class: 'usgrant' }, h('span', { class: 'usglbl', text: label }), h('span', { class: 'usgchips' }, gs.map((g) => h('span', { class: 'usgchip', title: 'From ' + g.from, text: g.value })))))
      : h('div', { class: 'usstxt muted', text: 'Its tokens and “who am I” answers name no roles, scopes or groups.' }),
    seenRow('Allowed', st.allowed, st.allowed_total, 'ok'),
    seenRow('Refused', st.blocked, st.blocked_total, 'crit'),
    nothingSeen
      ? h(
          'div',
          { class: 'usstxt muted' },
          `No request sent as ${userShortName(u)} has been answered yet, so what it reaches is not known. `,
          h('button', { class: 'link', text: 'Run the Access check', onclick: () => startAccessCheck({ users: [u.id] }) }),
          ' to replay a branch of the app as this user.',
        )
      : null,
  );
}

/* ---- the Users screen ---- */

const US = { sel: null, timer: null, saving: false };

async function renderUsers(main) {
  clear(
    main,
    h(
      'div',
      { class: 'view usersview' },
      h(
        'div',
        { class: 'toolbar' },
        backButton(),
        h('h2', { text: 'Users' }),
        h('span', { class: 'muted', text: 'The people you test as: their cookies and headers. Act as one and your browser, the Bench and Scans send as them.' }),
        h('span', { class: 'spacer' }),
        h('span', { class: 'muted ussaved', id: 'ussaved' }),
        h('button', { class: 'btn sm primary', text: '+ Add user', onclick: addBlankUser }),
      ),
      h('div', { class: 'usbody', id: 'usbody' }, h('div', { class: 'empty', text: 'Loading users…' })),
    ),
  );
  await Promise.all([loadUsers(true), loadFound(), loadStanding()]);
  drawActing();
  drawUsers();
}

async function addBlankUser() {
  S.users.push({ id: '', name: 'User ' + (S.users.length + 1), note: '', headers: [], cookies: [], keep_fresh: true });
  try {
    await saveUsers();
  } catch (e) {
    S.users.pop();
    return toast(e.message, 'err');
  }
  US.sel = S.users[S.users.length - 1].id;
  drawUsers();
  const name = $('.usname');
  if (name) (name.focus(), name.select());
}

/** Saves a moment after the last change, so typing doesn't save each key. */
function usersChanged(redraw) {
  clearTimeout(US.timer);
  const note = $('#ussaved');
  if (note) note.textContent = 'Saving…';
  US.timer = setTimeout(async () => {
    try {
      await saveUsers(!redraw);
      if (note) note.textContent = 'Saved';
      if (redraw) await loadStanding();
    } catch (e) {
      if (note) note.textContent = '';
      toast(e.message, 'err');
    }
    if (redraw) drawUsers();
  }, redraw ? 0 : 500);
}

function drawUsers() {
  const body = $('#usbody');
  if (!body) return;
  const users = S.users || [];
  const found = foundNew();
  const foundList = [
    h('div', { class: 'ushd' }, h('span', { text: 'Found in traffic' }), found.length ? h('span', { class: 'n', text: found.length }) : null),
    found.length ? found.map(foundItem) : h('div', { class: 'muted usnone', text: 'Nothing new. Browse the app signed in, and the sessions it uses show up here.' }),
  ];
  if (US.pickFound && FOUND.list) {
    const want = found.find((f) => f.user === US.pickFound);
    if (want) US.sel = 'found:' + want.key;
    US.pickFound = null;
  }
  const fsel = foundByKey(foundKey(US.sel));
  if (!users.length && !fsel) {
    if (found.length) {
      US.sel = 'found:' + found[0].key;
      return drawUsers();
    }
    return clear(
      body,
      h(
        'div',
        { class: 'rlempty' },
        h('h3', { text: 'No saved users yet' }),
        h('p', { class: 'muted', text: 'A saved user is a set of cookies and headers, such as a session cookie or a bearer token. Browse the app signed in and the sessions it uses show up here to keep, or add one and paste its Cookie header.' }),
        h('button', { class: 'btn primary', text: '+ Add user', onclick: addBlankUser }),
      ),
    );
  }
  if (!fsel && !userById(US.sel)) US.sel = (actingUser() || users[0]).id;
  const list = h(
    'div',
    { class: 'uslist' },
    users.map((u) =>
      h(
        'button',
        { class: 'usitem' + (u.id === US.sel ? ' on' : ''), onclick: () => ((US.sel = u.id), drawUsers()) },
        userBadge(u),
        h('span', { class: 'usitemtext' }, h('span', { class: 'usitemname' }, h('b', { text: u.name }), stateTag(STAND.byUser.get(u.id), 'sm')), h('span', { class: 'muted', text: userSummary(u) })),
        u.id === S.acting ? h('span', { class: 'ustag', text: 'acting' }) : null,
      ),
    ),
    h(
      'button',
      { class: 'usitem browser' + (S.acting ? '' : ' acting'), title: 'Stop acting as a saved user', onclick: () => S.acting && setActing(null) },
      h('span', { class: 'ubadge none' }),
      h('span', { class: 'usitemtext' }, h('b', { text: 'Your browser' }), h('span', { class: 'muted', text: S.acting ? 'Click to use its own cookies again' : 'Its own cookies' })),
      S.acting ? null : h('span', { class: 'ustag', text: 'acting' }),
    ),
    foundList,
  );
  clear(body, list, fsel ? foundDetail(fsel) : userDetail(userById(US.sel)));
}

function userDetail(u) {
  const acting = u.id === S.acting;
  const now = nowSecs();
  const cookies = u.cookies || (u.cookies = []);
  const headers = headerRows(u);
  const field = (cls, value, placeholder, set, extra = {}) =>
    h('input', { class: cls, value, placeholder, spellcheck: 'false', oninput: (e) => (set(e.target.value), usersChanged()), ...extra });

  const cookieRow = (c, i) => {
    const live = cookieLive(c);
    return h(
      'div',
      { class: 'usrow' + (live ? '' : ' dead') },
      field('mono', c.name, 'name', (v) => (c.name = v)),
      field('mono', c.value, 'value', (v) => (c.value = v)),
      field('mono', c.domain || '', 'any in-scope host', (v) => (c.domain = v)),
      h('span', { class: 'usexp' + (live ? '' : ' dead'), title: c.expires != null ? new Date(c.expires * 1000).toLocaleString() : 'Sent until you expire or remove it', text: expiryText(c.expires) }),
      live
        ? h('button', { class: 'btn xs', text: 'Expire', title: 'Stop sending this cookie, but keep it here', onclick: () => ((c.expires = now), usersChanged(true)) })
        : h('button', { class: 'btn xs', text: 'Restore', title: 'Send this cookie again, with no expiry', onclick: () => ((c.expires = null), usersChanged(true)) }),
      h('button', { class: 'iconbtn', text: '✕', title: 'Remove this cookie', onclick: () => (cookies.splice(i, 1), usersChanged(true)) }),
    );
  };
  const headerRow = (hd, i) =>
    h(
      'div',
      { class: 'usrow hdr' },
      field('mono', hd.name, 'Authorization', (v) => (hd.name = v)),
      field('mono', hd.value, 'Bearer …', (v) => (hd.value = v)),
      field('mono', hd.host, 'every in-scope host', (v) => (hd.host = v), { title: 'Send only to this host and its subdomains. Leave blank for every in-scope host.' }),
      field('mono', hd.path, 'any path', (v) => (hd.path = v), { title: 'Send only to requests under this path, such as /v2/seller. Where two apply, the longer path wins.' }),
      h('button', { class: 'iconbtn', text: '✕', title: 'Remove this header', onclick: () => (headers.splice(i, 1), usersChanged(true)) }),
    );
  const pasteCookies = () => {
    const box = h('textarea', { class: 'mono uspaste', rows: '4', spellcheck: 'false', placeholder: 'session=abc123; theme=dark' });
    modal('Paste cookies', h('div', { class: 'usersheet' }, h('p', { class: 'hint', text: 'Paste a Cookie header, or the name=value pairs from it. Cookies with the same name are replaced.' }), box), [
      h('button', { class: 'btn', text: 'Cancel', onclick: closeModal }),
      h('button', {
        class: 'btn primary',
        text: 'Add cookies',
        onclick: () => {
          const text = box.value.replace(/^\s*cookie\s*:/i, '');
          for (const pair of text.split(/[;\n]/)) {
            const at = pair.indexOf('=');
            const name = at > 0 ? pair.slice(0, at).trim() : '';
            if (!name) continue;
            const value = pair.slice(at + 1).trim();
            const same = cookies.find((c) => c.name === name);
            if (same) Object.assign(same, { value, expires: null });
            else cookies.push({ name, value, domain: '', expires: null });
          }
          closeModal();
          usersChanged(true);
        },
      }),
    ]);
    box.focus();
  };
  const live = cookies.filter(cookieLive).length;

  return h(
    'div',
    { class: 'usdetail' },
    h(
      'div',
      { class: 'ushead' },
      userBadge(u, 'lg'),
      h('div', { class: 'usnames' }, field('usname', u.name, 'Name, e.g. Alice (admin)', (v) => (u.name = v)), field('usnote', u.note || '', 'Note (optional)', (v) => (u.note = v))),
      acting
        ? h('button', { class: 'btn sm', text: 'Stop acting as this user', onclick: () => setActing(null) })
        : h('button', { class: 'btn sm primary', text: 'Act as this user', title: 'Your browser, the Bench and Scans send as this user until you switch back', onclick: () => setActing(u.id) }),
      h('button', {
        class: 'btn sm',
        text: 'Open a browser as ' + userShortName(u),
        title: 'A browser window of this user’s own, with its own cookies: sign in there without signing out here',
        onclick: () => openTarget(u),
      }),
    ),
    foundBanner(u),
    acting ? h('div', { class: 'usnote-on' }, userBadge(u), `You are acting as ${u.name}. In-scope browser traffic, the Bench and Scans send with these cookies, and cookies the server sets land here instead of in your browser.`) : null,
    standingSection(u),
    h(
      'section',
      { class: 'ussec uscookies' },
      h(
        'div',
        { class: 'ussech' },
        h('h3', { text: 'Cookies' }),
        h('span', { class: 'muted', text: cookies.length ? `${live} sent${cookies.length - live ? ', ' + (cookies.length - live) + ' expired' : ''}` : 'none yet' }),
        h('span', { class: 'spacer' }),
        h('button', { class: 'btn xs', text: 'Paste cookies', onclick: pasteCookies }),
        live ? h('button', { class: 'btn xs', text: 'Expire all', title: 'Sign this user out: stop sending every cookie, but keep them here', onclick: () => (cookies.forEach((c) => cookieLive(c) && (c.expires = now)), usersChanged(true)) }) : null,
        h('button', { class: 'btn xs', text: '+ Add cookie', onclick: () => (cookies.push({ name: '', value: '', domain: '', expires: null }), drawUsers(), $('.uscookies .usrow:last-of-type input').focus()) }),
      ),
      cookies.length ? h('div', { class: 'usgrid' }, h('div', { class: 'usrow head' }, ['Name', 'Value', 'Domain', 'Expires', '', ''].map((t) => h('span', { text: t }))), cookies.map(cookieRow)) : h('div', { class: 'muted usempty', text: 'No cookies. Paste a Cookie header, or act as this user and sign in: the cookies the site sets are kept here.' }),
      h(
        'label',
        { class: 'uscheck' },
        h('input', { type: 'checkbox', checked: u.keep_fresh !== false, onchange: (e) => ((u.keep_fresh = e.target.checked), usersChanged()) }),
        h('span', null, h('b', { text: 'Keep cookies fresh.' }), h('span', { class: 'muted', text: ' When the server sets or clears a cookie in answer to a request sent as this user, it is updated here.' })),
      ),
    ),
    h(
      'section',
      { class: 'ussec' },
      h(
        'div',
        { class: 'ussech' },
        h('h3', { text: 'Headers' }),
        h('span', { class: 'muted', text: 'such as a bearer token; give a host or path to send one only to that part of the app' }),
        h('span', { class: 'spacer' }),
        h('button', { class: 'btn xs', text: '+ Add header', onclick: () => (headers.push({ name: '', value: '', host: '', path: '' }), drawUsers()) }),
      ),
      headers.length ? h('div', { class: 'usgrid' }, h('div', { class: 'usrow hdr head' }, ['Name', 'Value', 'Host', 'Path', ''].map((t) => h('span', { text: t }))), headers.map(headerRow)) : null,
    ),
    h(
      'div',
      { class: 'usfoot' },
      h('span', { class: 'muted', text: 'Values stay in this project. Nothing is sent until you browse, press Send or run a scan.' }),
      h('span', { class: 'spacer' }),
      h('button', {
        class: 'btn xs danger',
        text: 'Delete user',
        onclick: async () => {
          if (!confirm(`Delete ${u.name} and their cookies?`)) return;
          S.users = S.users.filter((x) => x !== u);
          US.sel = null;
          usersChanged(true);
        },
      }),
    ),
  );
}

/* ---- the Access check screen ---- */

const AC = { host: null, prefix: '/', targets: [], sourceLabel: '', anon: true, picks: null, running: false, report: null, err: null };

/** Opens the Access check on a selection from Traffic or the Map. */
function startAccessCheck({ targets = [], host = null, prefix = '/', sourceLabel = '', onlyAnon = false, users = null } = {}) {
  AC.targets = targets;
  AC.host = host;
  AC.prefix = prefix || '/';
  AC.sourceLabel = sourceLabel;
  AC.report = null;
  AC.err = null;
  AC.anon = onlyAnon ? true : AC.anon;
  AC.picks = onlyAnon ? new Set() : users ? new Set(users) : null; // onlyAnon: signed-out only; otherwise the given users, or every saved user
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
    h('label', { class: 'acid' }, h('input', { type: 'checkbox', checked: AC.picks.has(u.id), onchange: (e) => (e.target.checked ? AC.picks.add(u.id) : AC.picks.delete(u.id)) }), h('span', { class: 'acname', text: u.name }), h('span', { class: 'achdr', text: userSummary(u) })),
  );
  clear(
    idCard,
    h('div', { class: 'aclbl' }, 'Replay as', h('button', { class: 'link', style: { marginLeft: 'auto' }, text: (S.users || []).length ? 'Manage users' : 'Add users', onclick: () => leaveTo('users') })),
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
  for (const id of r.identities) headCells.push(h('th', { class: 'acidcol' + (id.anon ? ' anon' : ''), text: id.label, title: id.label }));
  const rows = [];
  for (const row of r.rows) {
    const byId = {};
    for (const c of row.cells) byId[c.identity] = c;
    const tds = [h('td', { class: 'actarget' }, h('div', { class: 'acreq' }, h('span', { class: 'meth m-' + row.method, text: row.method }), h('span', { class: 'mono acpath', text: row.path, title: row.host + row.path })))];
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
    h('p', { class: 'mnote', text: `Callbacks are collected by interactsh, the open-source callback tool by ProjectDiscovery, which runs on ${THIS_COMPUTER}. Install it once in a terminal. It builds with Go in a minute or so, and Plonix finds it in ~/go/bin:` }),
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
  // The client keeps running when it can no longer fetch callbacks; say so.
  const warn = d.phase === 'listening' && d.error ? h('div', { class: 'cbwarn', text: d.error }) : null;
  if (!list.length) {
    const why =
      d.phase === 'listening'
        ? host
          ? 'Nothing has reached this host yet. Callbacks arrive within a few seconds of the call.'
          : 'Waiting for callbacks. Put a host in a request, send it, and anything that reaches the host shows up here.'
        : d.phase === 'starting'
          ? 'Connecting to the callback server…'
          : d.error || 'Press Start listening to register with a callback server. Nothing is sent to any target.';
    return clear(box, head, warn, h('div', { class: 'empty cbempty' + (d.phase === 'failed' ? ' bad' : ''), text: why }));
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
  clear(box, head, warn, table, detail);
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
