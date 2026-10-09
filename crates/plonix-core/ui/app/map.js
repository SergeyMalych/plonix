// Plonix window: Map: hosts, endpoints and technologies.
// Part of the project window's scripts (see index.html for the load order).
'use strict';

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
    endpointsSection(eps),
  );
}

const MCOLS = [
  { key: 'method', label: 'Method', w: 84 },
  { key: 'path', label: 'Path' },
  { key: 'status', label: 'Status', w: 76 },
  { key: 'params', label: 'Parameters', w: 240 },
  { key: 'requests', label: 'Requests', w: 92, th: 'num' },
];

/** Values a column sorts by. Status sorts by the lowest code seen, Parameters by how many there are. */
const MSORT = {
  method: (e) => e.method,
  path: (e) => e.path,
  status: (e) => Math.min(...e.statuses, 999),
  params: (e) => e.params.length,
  requests: (e) => e.requests,
};

/**
 * Does an endpoint match the filter? Words match the method, path, status
 * or a parameter; `method:`, `path:`, `status:` (4xx works) and `param:`
 * narrow a word to one field, and a leading `-` hides what matches.
 */
function endpointMatches(e, q) {
  return q
    .toLowerCase()
    .split(/\s+/)
    .filter(Boolean)
    .every((t) => {
      const neg = t.startsWith('-') && t.length > 1;
      if (neg) t = t.slice(1);
      const [, field, val] = t.match(/^(method|path|status|param|params):(.*)$/) || [null, null, t];
      const status = (v) => e.statuses.some((s) => (/^\dxx$/.test(v) ? String(s)[0] === v[0] : String(s).startsWith(v)));
      const hit = !val
        ? true
        : field === 'method'
          ? e.method.toLowerCase().startsWith(val)
          : field === 'path'
            ? e.path.toLowerCase().includes(val)
            : field === 'status'
              ? status(val)
              : field
                ? e.params.some((p) => p.toLowerCase().includes(val))
                : e.method.toLowerCase() === val || e.path.toLowerCase().includes(val) || status(val) || e.params.some((p) => p.toLowerCase().includes(val));
      return neg ? !hit : hit;
    });
}

/** The host's endpoints: filter as you type, click a column title to sort, drag its edge to resize. */
function endpointsSection(eps) {
  const saved = store('plonix.map.columns') || {};
  M.colW = saved.widths || {};
  M.sort = typeof saved.sort === 'string' ? saved.sort : '';
  const save = () => store('plonix.map.columns', { widths: M.colW, sort: M.sort });
  const flex = MCOLS.findIndex((c) => !c.w);
  const cols = MCOLS.map((c) => h('col', { style: c.w ? { width: (M.colW[c.key] || c.w) + 'px' } : null }));
  const setW = (c, i, w) => {
    if (w == null) delete M.colW[c.key];
    else M.colW[c.key] = w;
    cols[i].style.width = (w || c.w) + 'px';
    save();
  };
  const resize = (e, c, i) => {
    e.preventDefault();
    e.stopPropagation();
    const dir = i < flex ? 1 : -1;
    const startX = e.clientX;
    const startW = e.currentTarget.parentElement.getBoundingClientRect().width;
    document.body.classList.add('colresize');
    const move = (ev) => setW(c, i, Math.round(Math.max(TCOL_MIN, Math.min(600, startW + dir * (ev.clientX - startX)))));
    const up = () => {
      document.body.classList.remove('colresize');
      window.removeEventListener('mousemove', move);
      window.removeEventListener('mouseup', up);
    };
    window.addEventListener('mousemove', move);
    window.addEventListener('mouseup', up);
  };
  const ths = MCOLS.map((c, i) =>
    h(
      'th',
      {
        class: (c.th ? c.th + ' ' : '') + 'sortable',
        'data-col': c.key,
        title: 'Sort by ' + c.label,
        onclick: () => {
          M.sort = M.sort === c.key ? '-' + c.key : M.sort === '-' + c.key ? '' : c.key;
          save();
          draw();
        },
      },
      h('span', { class: 'sortarrow' }),
      h('span', { text: c.label }),
      c.w ? h('span', { class: 'colgrip ' + (i < flex ? 'r' : 'l'), title: 'Drag to resize · double-click to reset', onclick: (e) => e.stopPropagation(), onmousedown: (e) => resize(e, c, i), ondblclick: (e) => (e.stopPropagation(), setW(c, i, null)) }) : null,
    ),
  );
  const tbody = h('tbody');
  const count = h('span');
  const input = h('input', {
    type: 'search',
    placeholder: 'Filter: admin, method:POST, status:4xx, param:limit',
    value: M.filter || '',
    spellcheck: 'false',
    oninput: () => ((M.filter = input.value), draw()),
    onkeydown: (e) => e.key === 'Escape' && input.value && (e.stopPropagation(), (input.value = M.filter = ''), draw()),
  });
  function draw() {
    const key = M.sort.replace(/^-/, '');
    const desc = M.sort.startsWith('-');
    for (const th of ths) {
      const on = th.dataset.col === key;
      th.classList.toggle('sorted', on);
      th.querySelector('.sortarrow').textContent = on ? (desc ? '↓' : '↑') : '';
      th.setAttribute('aria-sort', on ? (desc ? 'descending' : 'ascending') : 'none');
    }
    let rows = M.filter ? eps.filter((e) => endpointMatches(e, M.filter)) : eps.slice();
    if (MSORT[key]) {
      const v = MSORT[key];
      rows.sort((a, b) => {
        const x = v(a);
        const y = v(b);
        const c = typeof x === 'number' ? x - y : String(x).localeCompare(String(y));
        return (desc ? -c : c) || a.path.localeCompare(b.path);
      });
    }
    count.textContent = rows.length === eps.length ? `(${eps.length})` : `(${rows.length} of ${eps.length})`;
    clear(
      tbody,
      rows.length
        ? rows.map((e) =>
            h(
              'tr',
              { class: 'click' + (T.sel === e.sample_id ? ' sel' : ''), 'data-ex': e.sample_id, title: 'Open a sample request', onclick: () => showExchange(e.sample_id) },
              h('td', null, h('span', { class: 'meth m-' + e.method, text: e.method })),
              pathCell(e.path),
              h('td', null, e.statuses.map((s) => [h('span', { class: statusClass(s), text: s }), ' '])),
              h('td', null, e.params.map((p) => h('span', { class: 'param', text: p }))),
              h('td', { class: 'num', text: e.requests }),
            ),
          )
        : h('tr', null, h('td', { colspan: MCOLS.length, class: 'muted', text: 'No endpoints match the filter.' })),
    );
  }
  draw();
  return [
    h('div', { class: 'lbl eplbl' }, 'Endpoints ', count, h('div', { class: 'search r' }, h('span', { class: 'mg', text: '⌕' }), input)),
    h('table', { class: 'grid eps' }, h('colgroup', null, cols), h('thead', null, h('tr', null, ths)), tbody),
  ];
}

/** A path cell that never widens the table: the folder part gives way first, so the resource name stays readable. Folded ids and tokens are tinted. Hover shows the whole path. */
function pathCell(path) {
  return h('td', { class: 'pathcell', title: path }, pathParts(path));
}

/** The folder and the resource name of a path as separate spans, so the folder can give way first. */
function pathParts(path) {
  const cut = path.lastIndexOf('/', path.length - 2);
  const dir = cut > 0 ? path.slice(0, cut + 1) : '';
  const part = (cls, text) => h('span', { class: cls }, text.split(/(\{\w+\})/).map((t, i) => (i % 2 ? h('span', { class: 'ph', text: t }) : t)));
  return h('span', { class: 'pc' }, dir ? part('pd', dir) : null, part('pn', path.slice(dir.length)));
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
                pathCell(e.path),
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
