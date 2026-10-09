// Plonix window: Programs.
// Part of the project window's scripts (see index.html for the load order).
'use strict';

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
