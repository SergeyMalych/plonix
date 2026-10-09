// Plonix window: Traffic: the list, filters, Intercept, search, and the Lens.
// Part of the project window's scripts (see index.html for the load order).
'use strict';

/* ======================================================================
   Traffic
   ====================================================================== */

const T = { text: '', filters: [], items: [], total: 0, sel: null, live: true, maxId: 0, pretty: true, inspH: store('plonix.inspH'), picked: new Set(), pickAnchor: null, group: store('plonix.groupAlike') !== false, shortPath: store('plonix.shortPath') === true, open: new Set(), visible: null };

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

function setShortPath(on) {
  T.shortPath = on;
  // Full paths are the default, so only turning short paths on is remembered.
  store('plonix.shortPath', on ? true : null);
  drawPathToggle();
  drawRows(Infinity);
}

/** Full paths, or the Map's short form: ids and tokens folded, the query left out, the folder giving way to the name. */
function drawPathToggle() {
  const box = $('#pathseg');
  if (!box) return;
  clear(
    box,
    h('button', { class: T.shortPath ? '' : 'on', text: 'Full path', title: 'Show each path and query in full', onclick: () => setShortPath(false) }),
    h('button', { class: T.shortPath ? 'on' : '', text: 'Short path', title: 'Fold ids and tokens into {id} and {token}, leave out the query, and keep the last part of the path in view. Hover a row for the full path.', onclick: () => setShortPath(true) }),
  );
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
    'source:scan': 'Sent by a scan',
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
        h('span', { class: 'seg groupseg', id: 'pathseg' }),
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
  drawPathToggle();
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
  add('source:scan', 'Sent by a scan', f.scans, 'include', 'replay');
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
  ['source', 'Captured, sent from Bench, sent by a scan or imported', ['bench', 'replay', 'scan', 'har', 'import']],
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
  if (sessionGrant(ex)) actions.push({ label: 'Save login as a user', run: () => saveLoginAsUser(ex) });
  if (decide(ex.host) === 'accepted') {
    if (idT) actions.push({ label: 'Check this id across users', run: () => checkIdAcrossUsers(ex) });
    actions.push({ label: 'Replay signed out', run: () => replaySignedOut(ex) });
  }
  if (decide(ex.host) === 'accepted') {
    for (const name of extsThat('probe')) actions.push({ label: 'Probe for hidden parameters', run: () => probeExchange(name, ex) });
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
  if (ex.replaced) tags.push(h('span', { class: 'tag edited', text: 'changed', title: 'Changed on the way, by rules or by sending as a saved user' }));
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
      T.shortPath
        ? h('td', { class: 'url short', title: target(ex) }, h('span', { class: 'urlrow' }, tags, append(pathParts(ex.short_path || ex.path), [ex.query ? h('span', { class: 'pq', text: '?…' }) : null])))
        : h('td', { class: 'url', title: target(ex) }, tags, tags.length ? ' ' : '', target(ex)),
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
        sentAs(ex) ? h('button', { class: 'tag sentas', text: 'as ' + sentAs(ex), title: `Sent as the saved user ${sentAs(ex)}: their cookies and headers replaced the auth headers. Click to open Users`, onclick: () => leaveTo('users') }) : null,
        ruleNotes(ex).length ? h('button', { class: 'tag edited', text: 'changed', title: 'Changed by rules:\n' + ruleNotes(ex).join('\n') + '\n\nClick to open Rules', onclick: () => leaveTo('rules') }) : null,
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
