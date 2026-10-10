// Plonix window: Bench: edit and send requests, the editable Lens, payload runs.
// Part of the project window's scripts (see index.html for the load order).
'use strict';

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

/** Sends a request to the Bench and shows a one-line note about why — used by the Mind Reader chips.
 * With `param`, that query parameter's value is marked as a position, ready to vary or Run. */
async function benchWithNote(id, note, param) {
  const before = R.tabs.length;
  await sendToBench(id);
  const tab = R.tabs.length > before ? R.tabs[R.tabs.length - 1] : null;
  if (tab && param) {
    const marked = markQueryParam(tab.url, param);
    if (marked !== tab.url) {
      tab.url = marked;
      saveBench();
      if (S.view === 'bench') renderBench($('#main'));
    }
  }
  if (note) toast(note, 'ok');
}

/** The URL with one query parameter's value wrapped in position markers. */
function markQueryParam(url, name) {
  const q = url.indexOf('?');
  if (q < 0) return url;
  const parts = url.slice(q + 1).split('&');
  const dec = (s) => {
    try {
      return decodeURIComponent(s.replace(/\+/g, ' '));
    } catch (_) {
      return s;
    }
  };
  const i = parts.findIndex((p) => dec(p.split('=')[0]) === name && p.includes('='));
  if (i < 0) return url;
  const eq = parts[i].indexOf('=');
  if (eq === parts[i].length - 1) return url;
  parts[i] = parts[i].slice(0, eq + 1) + MARK + parts[i].slice(eq + 1) + MARK;
  return url.slice(0, q + 1) + parts.join('&');
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
    // The • marks only say where Run varies a value; Send sends the request without them.
    const { headers, body, bad } = parseRaw(withoutMarks(tab.raw));
    if (bad.length) return toast('Not a header line: ' + bad[0] + ' (use "Name: value", then a blank line before the body)', 'err');
    const req = { method: tab.method, url: withoutMarks(tab.url), headers };
    if (toolOn('saved-users') && tabUser(tab)) req.as_user = tabUser(tab);
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
      h('div', { class: 'reqbar' }, panelToggle, method, h('div', { class: 'urlwrap' }, url), urlMarkBtn, toolOn('saved-users') ? userSwitcher(tab, main) : null, runMode ? null : sendBtn),
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
const withoutMarks = (text) => (text || '').split(MARK).join('');
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
    as_user: toolOn('saved-users') && tabUser(tab) ? tabUser(tab) : null,
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
