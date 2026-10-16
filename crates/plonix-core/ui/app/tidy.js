// Plonix window: the Bench's Tidy view of the request being edited.
// Part of the project window's scripts (see index.html for the load order).
'use strict';

/* ======================================================================
   Tidy view on the Bench
   The editor (Raw) holds exactly what is sent. Tidy shows the same request
   with the noise out of the way: a sign-in token reads as "JWT of Maya",
   Base64 JSON reads decoded, and the headers every browser adds and the
   tracking cookies fold into one line each. It stays editable, every edit
   lands in the editor, and Send always sends the real values.
   ====================================================================== */

const TIDY_KEY = 'plonix.bench.view';
const tidyOn = () => store(TIDY_KEY) !== 'raw';

/** Headers a browser adds to every request, which rarely change the answer. */
const BROWSER_HEADER = /^(host|user-agent|accept-language|accept-encoding|connection|proxy-connection|keep-alive|sec-ch-ua(-[\w-]+)?|sec-fetch-[\w-]+|sec-gpc|sec-purpose|dnt|upgrade-insecure-requests|priority|te|cache-control|pragma|origin|content-length)$/i;
/** Cookies analytics and consent tools set, which never sign anyone in. */
const TRACKING_COOKIE = /^(_ga(_\w+)?|_gid|_gat\w*|_gcl_\w+|_fbp|_fbc|_hj\w*|_clck|_clsk|_uet\w*|__utm\w*|_pk_\w+(\.\w+)*|ajs_\w+|amplitude\w*|mp_\w+|optanon\w*|cookieconsent\w*|cookie_?consent\w*|consent|_tt_\w*|_ttp|_pin_unauth|_rdt_uuid|hubspotutk|__hs\w+|intercom-[\w-]+)$/i;
/** Cookie names that usually carry a session. */
const SESSIONISH_COOKIE = /sess|sid|auth|token|jwt|login|remember|csrf|xsrf/i;

/* ---- whose a token is ---- */

/** value -> { user?, name } or null, from the engine, kept for half a minute. */
const TIDY_OWNERS = new Map();
let tidyOwnersAt = 0;
/** Asks whose the values not asked about yet are. True when anything new came back. */
async function tidyOwners(values) {
  if (Date.now() - tidyOwnersAt > 30000) {
    TIDY_OWNERS.clear();
    tidyOwnersAt = Date.now();
  }
  const ask = [...new Set(values)].filter((v) => !TIDY_OWNERS.has(v)).slice(0, 64);
  if (!ask.length) return false;
  ask.forEach((v) => TIDY_OWNERS.set(v, null));
  try {
    const r = await api('/api/users/whose', { method: 'POST', body: { values: ask } });
    ask.forEach((v, i) => TIDY_OWNERS.set(v, (r.owners || [])[i] || null));
  } catch (_) {
    return false;
  }
  return ask.some((v) => TIDY_OWNERS.get(v));
}
/** "Maya" for a saved user "Maya (customer)"; a found session's label as it is. */
const ownerShort = (o) => (o.user ? userShortName(o) : o.name);

/* ---- reading the request ---- */

/**
 * The request as Tidy lays it out, one entry per line on screen. A line is a
 * list of segments: plain text, a header name, or a value that may read as a
 * chip; `start` is where a value sits in the editor text. A fold stands for
 * several lines at once and keeps their text to put back.
 */
function tidyModel(raw, opened, asUser) {
  const lines = raw.split('\n');
  let blank = lines.indexOf('');
  if (blank < 0) blank = lines.length;
  const nameOf = (line) => {
    const c = line.indexOf(':');
    return c > 0 ? line.slice(0, c).trim() : '';
  };
  const noise = lines.slice(0, blank).filter((l) => BROWSER_HEADER.test(nameOf(l)));
  const fold = noise.length >= 2 && !opened.has('browser');
  const out = [];
  // The fold closes the headers, after the ones worth reading. Its lines go
  // back there too if the request is edited in Tidy: header order is free.
  const folded = () => fold && out.push({ fold: 'browser', raw: noise.join('\n'), names: noise.map(nameOf) });
  let pos = 0;
  lines.forEach((line, n) => {
    const start = pos;
    pos += line.length + 1;
    if (n === blank) folded();
    if (n >= blank) return out.push({ segs: line ? [{ t: 'text', s: line }] : [] });
    const name = nameOf(line);
    if (BROWSER_HEADER.test(name) && fold) return;
    const row = tidyHeader(line, start, opened);
    if (asUser && AUTH_HEADER.test(name)) row.sent = asUser;
    if (BROWSER_HEADER.test(name) && noise.length >= 2 && line === noise[0]) row.refold = true;
    out.push(row);
  });
  if (blank === lines.length) folded();
  return out;
}

/** One header line: its name, then its value split into what may read as chips. */
function tidyHeader(line, start, opened) {
  const c = line.indexOf(':');
  if (c <= 0) return { segs: [{ t: 'text', s: line }] };
  let i = c + 1;
  while (line[i] === ' ') i++;
  const name = line.slice(0, c);
  const value = line.slice(i);
  const at = start + i;
  const segs = [
    { t: 'name', s: name },
    { t: 'text', s: line.slice(c, i) },
  ];
  const key = name.trim();
  if (/^cookie$/i.test(key)) {
    segs.push(...tidyCookies(value, at, opened));
    const tracked = value.split(';').some((p) => TRACKING_COOKIE.test(p.split('=')[0].trim()));
    return { segs, trackfold: tracked && opened.has('tracking') };
  } else {
    const m = /^authorization$/i.test(key) && value.match(/^([A-Za-z]+ +)(\S.*)$/);
    if (m) segs.push({ t: 'text', s: m[1] }, { t: 'val', s: m[2], start: at + m[1].length, ctx: { name: key, scheme: m[1].trim() } });
    else if (AUTH_HEADER.test(key) || classifyToken(value.trim())) segs.push({ t: 'val', s: value, start: at, ctx: { name: key } });
    else segs.push({ t: 'text', s: value });
  }
  return { segs };
}

/** A Cookie header's pairs; tracking cookies gather into one chip at the end. */
function tidyCookies(value, at, opened) {
  const segs = [];
  const trackers = [];
  const re = /([^;]*)(;\s*|$)/g;
  let m;
  while ((m = re.exec(value)) && m[0]) {
    const pair = m[1];
    const eq = pair.indexOf('=');
    const name = (eq < 0 ? pair : pair.slice(0, eq)).trim();
    if (!opened.has('tracking') && TRACKING_COOKIE.test(name)) {
      trackers.push(pair.trim());
      continue;
    }
    if (eq < 0) segs.push({ t: 'text', s: m[0] });
    else segs.push({ t: 'text', s: pair.slice(0, eq + 1) }, { t: 'val', s: pair.slice(eq + 1), start: at + m.index + eq + 1, ctx: { name, cookie: true } }, { t: 'text', s: m[2] });
  }
  // The visible pairs' last separator is dropped, so the tracking chip reads as the end of the line.
  const last = segs[segs.length - 1];
  if (last && last.t === 'text' && /^;\s*$/.test(last.s)) segs.pop();
  if (trackers.length) segs.push({ t: 'track', s: (segs.length ? '; ' : '') + trackers.join('; '), n: trackers.length, names: trackers.map((p) => p.split('=')[0]) });
  return segs;
}

/** What a value reads as, or null to leave it as text. */
function tidyChip(seg) {
  const v = seg.s.trim();
  const ctx = seg.ctx;
  if (v.length < 8) return null;
  const tok = classifyToken(v);
  const jwt = tok && tok.kind === 'jwt' ? tok : null;
  const owner = TIDY_OWNERS.get(v) || null;
  let kind = jwt ? 'JWT' : /csrf|xsrf/i.test(ctx.name) ? 'CSRF' : /api[-_]?key/i.test(ctx.name) ? 'API key' : ctx.cookie ? 'session' : 'token';
  let basic = null;
  if (ctx.scheme && /^basic$/i.test(ctx.scheme)) {
    const dec = bl_b64decode(v);
    if (dec && dec.includes(':')) basic = dec.slice(0, dec.indexOf(':'));
    kind = 'login';
  }
  const sub = jwt ? tidyJwtNote(jwt.jwt) : null;
  const base = { value: v, ctx, tok, owner, kind, basic, sub };
  if (owner) return { ...base, cls: 'u', label: `${kind} of ${ownerShort(owner)}` };
  if (jwt) {
    const who = tidyJwtWho(jwt.jwt.payload);
    return { ...base, cls: 'j', label: who ? `JWT of ${who}` : 'JWT' };
  }
  if (basic) return { ...base, cls: 'j', label: `login of ${basic}` };
  if (tok && tok.kind === 'b64json') return { ...base, cls: 'd', label: tidyGist(tok.decode()) };
  const secret = ctx.cookie ? SESSIONISH_COOKIE.test(ctx.name) || v.length >= 32 : AUTH_HEADER.test(ctx.name) && v.length >= 16;
  if (secret) return { ...base, cls: 'g', label: `${kind} ${tidyPreview(v)}` };
  return null;
}

const tidyPreview = (v) => (v.length > 16 ? v.slice(0, 6) + '…' + v.slice(-4) : v);
/** Who a token's claims name, as short as it can be said. */
function tidyJwtWho(p) {
  const who = p.name || p.preferred_username || p.email || p.username || p.sub;
  return who == null ? '' : String(who).slice(0, 28);
}
/** "52 min left", "expired" or "unsigned": what to know about a token at a glance. */
function tidyJwtNote(jwt) {
  if (!jwt.sig || String(jwt.header.alg || '').toLowerCase() === 'none') return 'unsigned';
  if (typeof jwt.payload.exp !== 'number') return null;
  const left = jwt.payload.exp * 1000 - Date.now();
  if (left <= 0) return 'expired';
  const min = Math.round(left / 60000);
  if (min < 60) return `${min} min left`;
  if (min < 48 * 60) return `${Math.round(min / 60)} h left`;
  return `${Math.round(min / 1440)} days left`;
}
/** JSON read in one line: {cart, items: 2, admin: false}. */
function tidyGist(text) {
  const v = bl_tryJson(text);
  if (Array.isArray(v)) return `[${v.length} items]`;
  if (!v) return 'decoded JSON';
  const keys = Object.keys(v);
  const parts = keys.slice(0, 3).map((k) => {
    const x = v[k];
    if (x === null || typeof x === 'number' || typeof x === 'boolean') return `${k}: ${x}`;
    if (typeof x === 'string' && x.length <= 8) return `${k}: "${x}"`;
    return k;
  });
  return '{' + parts.join(', ') + (keys.length > 3 ? ', …' : '') + '}';
}

/* ---- drawing ---- */

/** The Tidy view for a Bench tab. It edits `editor`, which stays the request. */
function tidyView(tab, editor, main) {
  const box = h('div', {
    id: 'benchtidy',
    class: 'tidy',
    contenteditable: 'true',
    spellcheck: 'false',
    role: 'textbox',
    'aria-multiline': 'true',
    'aria-label': 'Request, tidy view',
  });
  const note = h('div', { class: 'tidynote' });
  const wrap = h('div', { class: 'tidywrap' }, box, note);
  const opened = new Set(tab.tidyOpen || []);
  let self = false;
  let dirty = false;

  const draw = () => {
    dirty = false;
    closeTidyPop();
    const asUser = toolOn('saved-users') ? userById(tabUser(tab)) : null;
    const model = tidyModel(editor.value, opened, asUser && userShortName(asUser));
    clear(box, model.map((row) => tidyRow(row, opened)));
    clear(
      note,
      asUser ? h('span', { class: 'tsentnote', text: `Sent as ${userShortName(asUser)}: their cookies and sign-in headers replace the struck-through ones. ` }) : null,
      h('span', { class: 'tok', text: '✓ Sends the real values.' }),
      ' Chips and folded lines only change how the request reads; Raw shows every byte.',
    );
    const values = [];
    for (const row of model) for (const s of row.segs || []) if (s.t === 'val' && !opened.has('v:' + s.s.trim())) values.push(s.s.trim());
    tidyOwners(values.filter((v) => v.length >= 8)).then((more) => more && box.isConnected && refresh());
  };
  const refresh = () => {
    clearTimeout(box._t);
    box._t = setTimeout(() => {
      if (!box.isConnected) return;
      if (document.activeElement === box) dirty = true;
      else draw();
    }, 80);
  };
  const open = (key) => {
    if (key === 'reset') opened.clear();
    else if (key.startsWith('-')) opened.delete(key.slice(1));
    else opened.add(key);
    tab.tidyOpen = [...opened].filter((k) => !k.startsWith('v:'));
    saveBench();
    draw();
  };

  box.addEventListener('input', () => {
    dirty = true;
    const text = tidyText(box);
    if (text === editor.value) return;
    editor.value = text;
    self = true;
    editor.dispatchEvent(new Event('input'));
    self = false;
  });
  editor.addEventListener('input', () => self || refresh());
  box.addEventListener('focus', () => (R.cbField = 'tidy'));
  box.addEventListener('blur', () => dirty && setTimeout(() => document.activeElement !== box && draw(), 0));
  box.addEventListener('paste', (e) => {
    e.preventDefault();
    document.execCommand('insertText', false, (e.clipboardData && e.clipboardData.getData('text/plain')) || '');
  });
  box.addEventListener('keydown', (e) => {
    if (e.key === 'Tab') {
      e.preventDefault();
      document.execCommand('insertText', false, '  ');
    }
  });
  box.addEventListener('click', (e) => {
    const el = e.target.closest('[data-open]');
    if (el) {
      e.preventDefault();
      return open(el.dataset.open);
    }
    const chip = e.target.closest('.tchip');
    if (chip && chip._info) {
      e.preventDefault();
      tidyPop(chip, tab, editor, main, open, wrap);
    }
  });
  editor._tidy = refresh;
  draw();
  return wrap;
}

/** One line on screen. */
function tidyRow(row, opened) {
  if (row.fold) {
    const n = row.names.length;
    return h(
      'div',
      { class: 'tl tfold', contenteditable: 'false', 'data-raw': row.raw, 'data-open': 'browser', title: 'Headers your browser adds to every request. Show them to read or edit them.' },
      h('span', { class: 'ttri' }),
      h('b', { text: `${n} browser headers` }),
      h('span', { class: 'tnames', text: row.names.join(', ') }),
      h('span', { class: 'tshow', text: 'Show' }),
    );
  }
  const line = h('div', { class: 'tl' + (row.sent ? ' tsent' : ''), title: row.sent ? `Replaced on Send by ${row.sent}’s` : null });
  for (const s of row.segs) {
    if (s.t === 'name') line.append(h('span', { class: 'tk', text: s.s }));
    else if (s.t === 'track')
      line.append(
        h(
          'span',
          { class: 'tchip g ttrack', contenteditable: 'false', 'data-raw': s.s, 'data-open': 'tracking', title: 'Analytics and consent cookies: ' + s.names.join(', ') + '. Click to show them.' },
          h('i'),
          `${s.n} tracking cookie${s.n === 1 ? '' : 's'}`,
        ),
      );
    else if (s.t === 'val') {
      const info = opened.has('v:' + s.s.trim()) ? null : tidyChip(s);
      if (!info) {
        line.append(s.s);
        continue;
      }
      // The chip stands for the value exactly; spaces around it stay text.
      const lead = s.s.slice(0, s.s.length - s.s.trimStart().length);
      const trail = s.s.slice(s.s.trimEnd().length);
      if (lead) line.append(lead);
      const chip = h('span', { class: 'tchip ' + info.cls, contenteditable: 'false', 'data-raw': info.value, title: 'Click to see or change it' }, h('i'), info.label, info.sub ? h('span', { class: 'tsub', text: ' · ' + info.sub }) : null);
      chip._info = { ...info, start: s.start + lead.length };
      line.append(chip);
      if (trail) line.append(trail);
    } else line.append(s.s);
  }
  if (row.refold) line.append(h('span', { class: 'trefold', contenteditable: 'false', 'data-raw': '', 'data-open': '-browser', text: 'fold', title: 'Fold the browser headers again' }));
  if (row.trackfold)
    line.append(h('span', { class: 'trefold', contenteditable: 'false', 'data-raw': '', 'data-open': '-tracking', text: 'fold', title: 'Fold the tracking cookies again' }));
  if (!line.childNodes.length) line.append(h('br'));
  return line;
}

/** The request text the Tidy view stands for: chips and folds put back as they were. */
function tidyText(box) {
  const walk = (node) => {
    if (node.nodeType === Node.TEXT_NODE) return node.data;
    if (node.nodeType !== Node.ELEMENT_NODE) return '';
    if (node.dataset.raw != null) return node.dataset.raw + (node.classList.contains('tl') ? '\n' : '');
    if (node.tagName === 'BR') return '\n';
    let inner = '';
    for (const c of node.childNodes) inner += walk(c);
    if (node.tagName === 'DIV' || node.tagName === 'P') {
      // A line ending in a lone <br> keeps the line open; it is not a line of its own.
      if (inner.endsWith('\n') && node.lastChild && node.lastChild.tagName === 'BR') inner = inner.slice(0, -1);
      return inner + '\n';
    }
    return inner;
  };
  let text = '';
  for (const c of box.childNodes) text += walk(c);
  // Each line closes with a line break; the last one does not have one in the request.
  const last = box.lastChild;
  if (last && last.nodeType === Node.ELEMENT_NODE && /^(DIV|P)$/.test(last.tagName) && text.endsWith('\n')) text = text.slice(0, -1);
  return text.replace(/ /g, ' ');
}

/* ---- a chip's card ---- */

function closeTidyPop() {
  for (const p of document.querySelectorAll('.tpop')) {
    if (p._off) p._off();
    p.remove();
  }
  for (const c of document.querySelectorAll('.tchip.on')) c.classList.remove('on');
}

/** Puts `next` where `info` stands in the editor, as if typed there. */
function tidyReplace(editor, info, next) {
  let at = info.start;
  if (editor.value.slice(at, at + info.value.length) !== info.value) at = editor.value.indexOf(info.value);
  if (at < 0) return false;
  editor.value = editor.value.slice(0, at) + next + editor.value.slice(at + info.value.length);
  editor.dispatchEvent(new Event('input'));
  return true;
}

/** Other saved users' values for the same cookie or header, to swap in. */
function tidySwaps(info) {
  if (!toolOn('saved-users')) return [];
  const out = [];
  for (const u of S.users || []) {
    let v = null;
    if (info.ctx.cookie) v = ((u.cookies || []).find((c) => c.name === info.ctx.name && cookieLive(c)) || {}).value;
    else {
      const same = (k) => k.toLowerCase() === info.ctx.name.toLowerCase();
      v = ((u.headers || []).find(([k]) => same(k)) || [])[1] || ((u.scoped || []).find((x) => same(x.name)) || {}).value;
      if (v && info.ctx.scheme) v = v.replace(/^[A-Za-z]+ +/, '');
    }
    if (v && v.trim() !== info.value) out.push({ user: u, value: v.trim() });
  }
  return out;
}

function tidyPop(chip, tab, editor, main, open, wrap) {
  const was = chip.classList.contains('on');
  closeTidyPop();
  if (was) return;
  const info = chip._info;
  chip.classList.add('on');
  const o = info.owner;
  const who = o ? (o.user ? `${o.name}, from Users` : `${o.name}, found in traffic`) : info.cls === 'd' ? 'Base64 JSON, shown decoded' : 'Not matched to a saved user';
  const rows = [];
  if (info.tok && info.tok.kind === 'jwt') {
    const p = info.tok.jwt.payload;
    const show = (k, v) => v != null && v !== '' && rows.push([k, Array.isArray(v) ? v.join(', ') : typeof v === 'object' ? JSON.stringify(v) : String(v)]);
    show('sub', p.sub);
    show('name', p.name || p.preferred_username);
    show('email', p.email);
    show('role', p.role || p.roles || p.scope || p.scp || p.groups);
    if (typeof p.exp === 'number') rows.push(['expires', fmtDate(p.exp * 1000) + (info.sub ? ` (${info.sub})` : '')]);
    rows.push(['alg', String(info.tok.jwt.header.alg || '?')]);
  }
  if (info.basic) rows.push(['user', info.basic]);
  const decoded = info.cls === 'd' ? h('pre', { class: 'tjson', text: info.tok.decode() }) : null;
  const span = { start: info.start, end: info.start + info.value.length };
  const lensItem = () => {
    const at = editor.value.slice(span.start, span.end) === info.value ? span.start : editor.value.indexOf(info.value);
    return scanDraft(editor.value).find((x) => x.start === at) || null;
  };
  const edit =
    info.tok && lensItem()
      ? h('button', {
          class: 'btn sm primary',
          text: info.tok.kind === 'jwt' ? 'Edit claims' : 'Edit decoded',
          title: 'Edit it in the Lens below the request; the request is rewritten as you type',
          onclick: () => {
            const it = lensItem();
            const strip = $('#main .benchlens');
            if (!it || !strip) return;
            closeTidyPop();
            const items = scanDraft(editor.value);
            const n = items.findIndex((x) => x.start === it.start);
            const spot = strip.querySelectorAll('.spot')[n];
            if (spot) spot.click();
          },
        })
      : null;
  const swaps = tidySwaps(info).map((s) =>
    h('button', {
      class: 'btn sm',
      text: `Use ${userShortName(s.user)}’s`,
      title: `Put ${s.user.name}’s ${info.ctx.name} here instead`,
      onclick: () => {
        closeTidyPop();
        if (tidyReplace(editor, info, s.value)) toast(`${info.ctx.name} is now ${userShortName(s.user)}’s. Send to try it.`, 'ok');
      },
    }),
  );
  const pop = h(
    'div',
    { class: 'tpop', role: 'dialog' },
    h('div', { class: 'tph' }, h('span', { class: 'tchip ' + info.cls }, h('i'), info.label), h('span', { class: 'tpwho', text: who })),
    h(
      'div',
      { class: 'tpact' },
      edit,
      swaps,
      h('button', { class: 'btn sm', text: 'Show raw here', title: 'Show this value as text in the Tidy view', onclick: () => (closeTidyPop(), open('v:' + info.value)) }),
      h('button', { class: 'btn sm', text: 'Copy', onclick: () => (copyText(info.value), closeTidyPop()) }),
    ),
    rows.length ? h('table', { class: 'tptab' }, rows.map(([k, v]) => h('tr', null, h('td', { text: k }), h('td', { text: v })))) : null,
    decoded,
    h('div', { class: 'tpraw', text: info.value.length > 400 ? info.value.slice(0, 400) + '…' : info.value }),
  );
  wrap.append(pop);
  const wr = wrap.getBoundingClientRect();
  const cr = chip.getBoundingClientRect();
  const left = Math.max(8, Math.min(cr.left - wr.left, wr.width - pop.offsetWidth - 8));
  pop.style.left = left + 'px';
  // In a short window the card scrolls inside the room under the chip rather than run off the column.
  const top = cr.bottom - wr.top + 6;
  pop.style.top = top + 'px';
  const col = (wrap.closest('.rcol') || wrap).getBoundingClientRect();
  pop.style.maxHeight = Math.max(120, col.bottom - cr.bottom - 14) + 'px';
  const away = (e) => {
    if (!pop.contains(e.target) && !chip.contains(e.target)) closeTidyPop();
  };
  const esc = (e) => e.key === 'Escape' && (e.stopPropagation(), closeTidyPop());
  setTimeout(() => document.addEventListener('mousedown', away, true), 0);
  document.addEventListener('keydown', esc, true);
  pop._off = () => {
    document.removeEventListener('mousedown', away, true);
    document.removeEventListener('keydown', esc, true);
  };
}

/** Tidy | Raw for the request editor. Picking Tidy again folds back what was opened. */
function tidySeg(tab, main) {
  const on = tidyOn();
  const pick = (v) => {
    store(TIDY_KEY, v);
    if (v === 'tidy') tab.tidyOpen = [];
    saveBench();
    renderBench(main);
  };
  return h(
    'span',
    { class: 'seg tidyseg', title: 'Tidy reads the request for you: tokens show whose they are and the noise folds away. Raw is every byte. Both send the same request.' },
    h('button', { id: 'tidybtn', class: on ? 'on' : '', text: 'Tidy', onclick: () => pick('tidy') }),
    h('button', { id: 'rawbtn', class: on ? '' : 'on', text: 'Raw', onclick: () => pick('raw') }),
  );
}
