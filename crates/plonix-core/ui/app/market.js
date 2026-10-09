// Plonix window: Market.
// Part of the project window's scripts (see index.html for the load order).
'use strict';

/* ======================================================================
   Market: skills, rules, filters, bundles and extensions from a signed catalog
   ====================================================================== */

const KIND_INFO = {
  skill: { label: 'Skills', one: 'Skill', ico: '✦' },
  rules: { label: 'Rules', one: 'Rule pack', ico: '◎' },
  filters: { label: 'Filters', one: 'Filter pack', ico: '⧩' },
  list: { label: 'Lists', one: 'List pack', ico: '≣' },
  bundle: { label: 'Bundles', one: 'Bundle', ico: '❖' },
  extension: { label: 'Extensions', one: 'Extension', ico: '⬡' },
  platform: { label: 'Platforms', one: 'Platform', ico: '⚐' },
};

const GROUP_LABELS = { traffic: 'Traffic', insights: 'Insights', map: 'Map', scope: 'Scope', findings: 'Findings', scan: 'Scans' };
const groupLabel = (g) => GROUP_LABELS[g] || g;

const MK = { data: null, kind: 'all', q: '', sel: null, busy: null, ext: {}, rec: null, peek: '', added: {} };

function renderMarket(main) {
  const q = h('input', {
    id: 'mq',
    placeholder: 'Search skills, rules, filters, platforms, bundles and extensions',
    spellcheck: 'false',
    autocomplete: 'off',
    value: MK.q,
    oninput: (e) => {
      MK.q = e.target.value;
      closePackage();
      drawMarket();
    },
  });
  clear(
    main,
    h(
      'div',
      { class: 'view market' },
      h(
        'div',
        { class: 'toolbar' },
        h('h2', { text: 'Market' }),
        h('div', { class: 'search' }, h('span', { class: 'mg', text: '⌕' }), q),
        h('button', { class: 'btn sm', id: 'mupdate', hidden: true, onclick: updateAll }),
        h('button', { class: 'btn sm', text: 'Add your own', title: 'Add a skill, pack or extension from a GitHub repository, a folder, a file or a link. It is marked Your own.', onclick: () => addExternal() }),
        h('button', { class: 'iconbtn', title: 'Check the Market again', text: '↻', onclick: () => loadMarket(true) }),
      ),
      h('div', { class: 'mtrust', id: 'mtrust' }),
      h('div', { class: 'filterchips mkinds', id: 'mkinds' }),
      h('div', { class: 'mbody', id: 'mbody' }, h('div', { class: 'pane' }, h('div', { class: 'mrec', id: 'mrec', hidden: true }), h('div', { class: 'mgrid', id: 'mgrid' }, h('div', { class: 'muted', text: 'Loading the Market…' }))), h('div', { id: 'mdetail' })),
    ),
  );
  loadMarket(false);
}

async function loadMarket(refresh) {
  try {
    MK.data = await api('/api/market' + (refresh ? '?refresh=true' : ''));
    const ex = await api('/api/extensions').catch(() => ({ extensions: [] }));
    MK.ext = Object.fromEntries((ex.extensions || []).map((x) => [x.name, x]));
    MK.rec = await api('/api/market/recommended' + (MK.peek ? '?profile=' + encodeURIComponent(MK.peek) : '')).catch(() => null);
  } catch (e) {
    const grid = $('#mgrid');
    if (grid) clear(grid, h('div', { class: 'empty' }, h('h3', { text: 'The Market is not available' }), h('p', { text: e.message })));
    return;
  }
  if (S.view !== 'market') return;
  drawMarket();
  if (MK.sel) showPackage(MK.sel);
  // Newer releases of what was added from GitHub: checked after the list shows, never installed on their own.
  if (MK.data.packages.some((p) => p.local && (p.added_from || '').startsWith('github:'))) {
    api('/api/market/added-updates')
      .then((r) => {
        MK.added = Object.fromEntries((r.updates || []).map((u) => [u.name, u]));
        if (S.view === 'market') drawMarket();
      })
      .catch(() => {});
  }
}

/** The shelf a package is on: Official, Community, a publisher you trust, Your own, or Changed. */
const trustClass = (v) => (v.level === 'changed' ? 'bad' : v.level === 'built_in' ? 'in' : { official: 'ok', publisher: 'ok', community: 'com', own: 'warn' }[v.shelf] || 'warn');

/** The trust mark shown next to every package. */
function trustBadge(v, full) {
  if (!v) return null;
  const short = v.level === 'built_in' ? 'Built in' : v.level === 'changed' ? 'Changed' : v.shelf === 'publisher' ? 'Verified' : v.label;
  const mark = v.level === 'verified' && v.shelf !== 'community' ? '✓' : v.level === 'built_in' ? '✓' : v.shelf === 'community' ? '◇' : '!';
  return h('span', { class: 'trust ' + trustClass(v), title: v.detail }, h('i', { text: mark }), full ? v.label : short);
}

const isUnverified = (p) => p.verification && ['unverified', 'changed'].includes(p.verification.level);
const isCommunity = (p) => p.verification && p.verification.shelf === 'community';
/** Code nobody at Plonix has reviewed: a red line next to what it may do. */
const notReviewed = (v) => (v && ['community', 'own'].includes(v.shelf) ? h('p', { class: 'mreview', text: 'Plonix has not reviewed this code. Only say yes to what you are happy for its author to do.' }) : null);

function marketStatus(p) {
  const st = p.status.state;
  if (st === 'built_in') return { text: 'Built in', cls: 'tag in', action: null };
  const up = MK.added[p.name];
  if (p.local && up && !up.error) return { text: 'Release ' + up.latest + ' is out', cls: 'tag upd', action: 'readd' };
  if (p.local && p.added_from && p.added_from.startsWith('/') && !/\.(plonixext|md|json)$/.test(p.added_from)) return { text: 'From a folder', cls: 'tag in', action: 'readd' };
  const x = p.kind === 'extension' && MK.ext[p.name];
  if (st === 'installed' && x && x.disabled_reason) return { text: 'Stopped', cls: 'tag bad', action: 'remove' };
  if (st === 'installed' && x && !x.enabled) return { text: 'Installed · off', cls: 'tag out', action: 'remove' };
  if (st === 'installed') return { text: 'Installed', cls: 'tag in', action: 'remove' };
  if (st === 'update') return { text: 'Update ' + p.status.installed + ' → ' + p.version, cls: 'tag upd', action: 'update' };
  if (st === 'needs_runtime') return { text: 'Coming soon', cls: 'tag out', action: null };
  return { text: 'Available', cls: 'tag out', action: 'install' };
}

function drawMarket() {
  const d = MK.data;
  if (!d) return;
  const trust = $('#mtrust');
  if (trust) {
    const ok = d.trust && d.trust.state === 'verified';
    clear(
      trust,
      h('span', { class: 'mshield' + (ok ? ' ok' : ' bad'), text: ok ? '✓' : '!' }),
      h('b', { text: ok ? 'Signed by ' + d.trust.publisher : 'Not signed' }),
      h('span', { class: 'muted', text: ok ? ' · every package is checked against the signed list before it installs' : ' · nobody vouches for this list' }),
      d.offline_reason ? h('span', { class: 'muted', title: d.offline_reason, text: ' · showing the copy built into Plonix' }) : null,
      d.community ? h('span', { class: 'muted', text: ` · ${d.community} from the community, checked but not reviewed` }) : null,
      d.community_note ? h('span', { class: 'muted', title: d.community_note, text: ' · the community Market is not available right now' }) : null,
    );
  }
  const counts = { all: d.packages.length };
  for (const p of d.packages) counts[p.kind] = (counts[p.kind] || 0) + 1;
  counts.installed = d.packages.filter((p) => ['installed', 'update'].includes(p.status.state)).length;
  counts.unverified = d.packages.filter(isUnverified).length;
  counts.community = d.packages.filter(isCommunity).length;
  const kinds = $('#mkinds');
  if (kinds) {
    const chip = (key, label) =>
      h('button', { class: 'chip' + (MK.kind === key ? ' on' : ''), onclick: () => ((MK.kind = key), drawMarket()) }, h('span', { text: label }), h('span', { class: 'n', text: counts[key] || 0 }));
    clear(kinds, chip('all', 'All'), Object.entries(KIND_INFO).map(([k, v]) => chip(k, v.label)), h('span', { class: 'fsep' }), chip('installed', 'Installed'), counts.community ? chip('community', 'Community') : null, counts.unverified ? chip('unverified', 'Your own') : null);
  }
  drawRecommended();
  const updates = d.packages.filter((p) => p.status.state === 'update');
  const ub = $('#mupdate');
  if (ub) {
    ub.hidden = !updates.length;
    ub.textContent = updates.length === 1 ? 'Install 1 update' : `Install ${updates.length} updates`;
  }
  const q = MK.q.trim().toLowerCase();
  const list = d.packages.filter(
    (p) =>
      (MK.kind === 'all' || p.kind === MK.kind || (MK.kind === 'installed' && ['installed', 'update'].includes(p.status.state)) || (MK.kind === 'unverified' && isUnverified(p)) || (MK.kind === 'community' && isCommunity(p))) &&
      (!q || p.name.includes(q) || p.description.toLowerCase().includes(q) || (KIND_INFO[p.kind] || {}).one.toLowerCase().includes(q)),
  );
  const grid = $('#mgrid');
  if (!grid) return;
  if (!list.length) return clear(grid, h('div', { class: 'empty', text: MK.kind === 'installed' ? 'Nothing installed from the Market yet.' : 'Nothing matches.' }));
  clear(
    grid,
    list.map((p) => {
      const st = marketStatus(p);
      const k = KIND_INFO[p.kind] || { one: p.kind, ico: '•' };
      return h(
        'div',
        { class: 'mpkg' + (MK.sel === p.name ? ' sel' : '') + (isUnverified(p) ? ' unv' : ''), tabindex: 0, onclick: () => showPackage(p.name), onkeydown: (e) => e.key === 'Enter' && showPackage(p.name) },
        h('div', { class: 'mph' }, h('span', { class: 'mico k-' + p.kind, text: k.ico }), h('div', { class: 'mpn' }, h('b', { text: p.name }), h('span', { class: 'muted', text: k.one + ' · ' + p.version + (p.local ? ' · added by you' : '') })), h('span', { class: st.cls, text: st.text })),
        h('div', { class: 'mpd', text: p.description }),
        p.includes && p.includes.length ? h('div', { class: 'mpinc muted', text: 'Includes ' + p.includes.join(', ') }) : null,
        h(
          'div',
          { class: 'mpf' },
          trustBadge(p.verification, false),
          st.action ? marketButton(p, st.action, true) : null,
        ),
      );
    }),
  );
}

/** "Recommended for you": Market items that suit the user's kind of work, with why. */
function drawRecommended() {
  const box = $('#mrec');
  const r = MK.rec;
  if (!box) return;
  box.hidden = !r || MK.kind !== 'all' || !!MK.q.trim();
  if (box.hidden) return;
  const pick = (id) => {
    MK.peek = '';
    api('/api/market/profile', { method: 'POST', body: { profile: id } })
      .then(() => loadMarket(false))
      .catch((e) => toast(e.message, 'err'));
  };
  if (!r.shown) {
    return clear(
      box,
      h('div', { class: 'mrech' }, h('b', { text: 'Get suggestions for your work' }), h('span', { class: 'muted', text: 'Plonix picks Market items that suit it, and says why.' })),
      h('div', { class: 'mrecprofiles' }, r.profiles.map((p) => h('button', { class: 'chip', title: p.line, text: p.title, onclick: () => pick(p.id) }))),
    );
  }
  const select = h(
    'select',
    {
      id: 'mrecprofile',
      class: 'mrecsel',
      title: 'Your work',
      onchange: (e) => {
        if (e.target.value === r.profile) MK.peek = '';
        else MK.peek = e.target.value;
        loadMarket(false);
      },
    },
    r.profiles.map((p) => h('option', { value: p.id, text: p.title + (p.id === r.profile ? ' (yours)' : ''), selected: p.id === r.shown.id })),
  );
  const rec = r.recommendation || { starter: [], included: [], also: [] };
  const byName = Object.fromEntries((MK.data ? MK.data.packages : []).map((p) => [p.name, p]));
  const card = (k) => {
    const p = byName[k.name];
    if (!p) return null;
    const st = marketStatus(p);
    const ki = KIND_INFO[p.kind] || { one: p.kind, ico: '•' };
    return h(
      'div',
      { class: 'mpkg mrecpkg', tabindex: 0, onclick: () => showPackage(p.name), onkeydown: (e) => e.key === 'Enter' && showPackage(p.name) },
      h('div', { class: 'mph' }, h('span', { class: 'mico k-' + p.kind, text: ki.ico }), h('div', { class: 'mpn' }, h('b', { text: p.name }), h('span', { class: 'muted', text: ki.one })), h('span', { class: st.cls, text: st.text })),
      h('div', { class: 'mpd', text: k.why }),
      h('div', { class: 'mpf' }, k.noise !== 'passive' ? h('span', { class: 'muted small', text: k.noise === 'active' ? 'Sends many requests' : 'Sends a few requests' }) : h('span'), st.action ? marketButton(p, st.action, true) : null),
    );
  };
  const peeking = r.shown.id !== r.profile;
  clear(
    box,
    h(
      'div',
      { class: 'mrech' },
      h('b', { text: 'Recommended for you' }),
      select,
      peeking ? h('button', { class: 'btn sm ghost', text: 'Make this my work', onclick: () => pick(r.shown.id) }) : null,
      h('span', { class: 'muted mrecline', text: r.shown.line }),
    ),
    rec.starter.length
      ? h('div', { class: 'mgrid mrecgrid' }, rec.starter.map(card))
      : h('div', { class: 'muted mrecdone', text: 'You have everything picked for this work. More shows up here as the Market grows.' }),
    rec.included.length || rec.also.length
      ? h(
          'div',
          { class: 'mrecmore muted' },
          rec.included.length ? h('span', null, 'You already have ', rec.included.map((k, i) => [i ? ', ' : '', h('a', { href: '#', text: k.name, onclick: (e) => (e.preventDefault(), showPackage(k.name)) })]), '. ') : null,
          rec.also.length ? h('span', null, 'Also for you: ', rec.also.map((k, i) => [i ? ', ' : '', h('a', { href: '#', text: k.name, title: k.why, onclick: (e) => (e.preventDefault(), showPackage(k.name)) })]), '.') : null,
        )
      : null,
  );
}

function marketButton(p, action, small) {
  if (action === 'readd') {
    const up = MK.added[p.name];
    return h('button', {
      class: 'btn' + (small ? ' sm' : '') + ' primary',
      text: up ? 'Look at ' + up.latest : 'Read again',
      title: up ? 'See what the new release asks for before adding it' : 'Read the folder again, after you rebuild it',
      onclick: (e) => {
        e.stopPropagation();
        addExternal(up ? up.source : p.added_from);
      },
    });
  }
  const label = { install: p.kind === 'bundle' ? 'Install all' : 'Install', update: 'Update', remove: 'Remove' }[action];
  const busy = MK.busy === p.name;
  return h('button', {
    class: 'btn' + (small ? ' sm' : '') + (action === 'remove' ? ' danger ghost' : ' primary'),
    disabled: busy,
    text: busy ? 'Working…' : label,
    onclick: (e) => {
      e.stopPropagation();
      marketAction(p, action);
    },
  });
}

async function marketAction(p, action, consent) {
  if (action === 'remove' && p.kind === 'bundle' && !confirm(`Remove ${p.name} and the packages it installed?`)) return;
  if (action !== 'remove' && p.kind === 'extension' && !consent) return extensionConsent(p, action);
  MK.busy = p.name;
  drawMarket();
  try {
    const r = await api('/api/market/' + (action === 'remove' ? 'remove' : 'install'), { method: 'POST', body: { name: p.name, ...(consent || {}) } });
    const changed = (r.changes || []).filter((c) => c.action !== 'unchanged');
    const verb = action === 'remove' ? 'Removed' : action === 'update' ? 'Updated' : 'Installed';
    toast(changed.length > 1 ? `${verb} ${p.name} with ${changed.length - 1} more` : `${verb} ${p.name}`, 'ok');
  } catch (e) {
    toast(e.message, 'err');
  }
  MK.busy = null;
  // Show or hide the tool's screen in the sidebar right away, not on the next poll.
  await poll();
  await loadMarket(false);
  loadFacets();
}

/** A small line icon for a permission row: ok (it gets this), warn (needs care) or off (not granted). */
function capIcon(kind) {
  const ns = 'http://www.w3.org/2000/svg';
  const svg = document.createElementNS(ns, 'svg');
  svg.setAttribute('viewBox', '0 0 16 16');
  svg.setAttribute('aria-hidden', 'true');
  for (const d of { ok: ['M4 8.5l2.5 2.5L12 5.5'], warn: ['M8 4.5v4.5', 'M8 11.6v.1'], off: ['M5 5l6 6', 'M11 5l-6 6'] }[kind]) {
    const path = document.createElementNS(ns, 'path');
    path.setAttribute('d', d);
    svg.append(path);
  }
  return h('span', { class: 'mi ' + kind }, svg);
}

/** What an extension asks for: what it gets on install, then a checkbox for each sensitive one. Returns the boxes.
 * `o.ticked` starts the boxes ticked (Plonix's own items); `o.needed` says it cannot run without them. */
function capabilityList(caps, o = {}) {
  const boxes = [];
  const given = caps.filter((c) => !c.sensitive).map((c) => h('div', { class: 'mcap' }, capIcon('ok'), h('span', { text: c.what })));
  const asks = caps
    .filter((c) => c.sensitive)
    .map((c) => {
      const box = h('input', { type: 'checkbox', value: c.id, checked: !!o.ticked });
      const warn = h('span', { class: 'mcapwarn', text: 'Without this it cannot run.', hidden: !o.needed || !!o.ticked });
      box.addEventListener('change', () => (warn.hidden = !o.needed || box.checked));
      boxes.push(box);
      return h('label', { class: 'mcap ask' }, box, h('span', null, c.what, ' ', warn));
    });
  const rows = h(
    'div',
    { class: 'mcaps' },
    given.length ? [h('div', { class: 'mcaph', text: 'It will be allowed to' }), given] : null,
    asks.length ? [h('div', { class: 'mcaph', text: 'Needs your yes' }), asks, h('p', { class: 'muted fine', text: 'You can change this later on its page.' })] : null,
  );
  return { rows, boxes };
}

/** Before an extension installs: what it may do, and an explicit yes for anything sensitive. */
async function extensionConsent(p, action) {
  let d;
  try {
    d = await api('/api/market/' + encodeURIComponent(p.name));
  } catch (e) {
    return toast(e.message, 'err');
  }
  const x = (d.detail && d.detail.extension) || {};
  if (!x.installable) return toast(x.why_not || `${p.name} cannot be installed in this version of Plonix`, 'err');
  // Plonix's own extensions come ready to run; code nobody at Plonix reviewed waits for a tick.
  const own = p.verification && ['official', 'publisher'].includes(p.verification.shelf);
  const { rows, boxes } = capabilityList(x.capabilities || [], { ticked: own, needed: !!x.program });
  const go = h('button', { class: 'btn primary', text: action === 'update' ? 'Update' : 'Install' });
  modal(
    `${action === 'update' ? 'Update' : 'Install'} ${p.name}?`,
    [
      p.description ? h('p', { class: 'mnote', text: p.description }) : null,
      rows,
      x.program && !x.program.found ? h('div', { class: 'mcaps' }, programNeeds(x.program)) : null,
      notReviewed(p.verification),
      h('div', { class: 'mhow' }, h('div', { class: 'mcaph', text: 'How it runs' }), h('p', { text: x.sandbox })),
    ],
    [h('button', { class: 'btn', text: 'Cancel', onclick: closeModal }), go],
  );
  go.onclick = () => {
    closeModal();
    marketAction(p, action, { approve: true, grant: boxes.filter((b) => b.checked).map((b) => b.value) });
  };
}

/** The program an extension runs: installed on this Mac, or how to install it. */
function programNeeds(pr) {
  if (pr.found) return h('div', { class: 'mcap' }, capIcon('ok'), h('span', { text: `${pr.id} is installed on this Mac.` }));
  return [
    h('div', { class: 'mcap' }, capIcon('warn'), h('span', { text: `${pr.id} is not installed on this Mac yet. Install it in Terminal, then come back:` })),
    h('div', { class: 'mneeds' }, h('code', { text: pr.install }), h('button', { class: 'btn sm', text: 'Copy', onclick: () => copyText(pr.install) })),
  ];
}

/** What running an installed extension does, by how it runs. */
const EXT_RUN = {
  enumerate: { label: 'Find subdomains', busy: 'Looking up subdomains…', title: 'Ask public sources for subdomains of every domain you accepted in Scope', view: 'scope', open: 'Open Scope' },
  scan: { label: 'Check captured traffic', busy: 'Checking…', title: 'Check everything captured so far. New traffic is checked as it arrives.', view: 'traffic', open: 'Open Traffic' },
  sandbox: { label: 'Read captured traffic', busy: 'Reading…', title: 'Hand everything captured so far to this extension. New traffic reaches it on its own.', view: 'traffic', open: 'Open Traffic' },
};

/** How an installed extension runs: scan, enumerate, probe, or sandbox for one with its own code. */
const extKind = (x) => (x.program ? x.program.kind : 'sandbox');

/** What it asks for that needs a yes but has not had one, when it cannot run without it. */
const extMissing = (x) => (x.program ? (x.requested || []).filter((c) => ['run-program', 'scoped-requests'].includes(c) && !(x.granted || []).includes(c)) : []);

/** Gives (or takes back) one capability of an installed extension, then redraws its page. */
async function allowCapability(name, capability, allowed) {
  try {
    await api('/api/extensions/' + encodeURIComponent(name) + '/allowed', { method: 'PUT', body: { capability, allowed } });
    toast(allowed ? `${name} is allowed to do that now` : `${name} may no longer do that`, 'ok');
  } catch (e) {
    toast(e.message, 'err');
  }
  await loadMarket(false);
  poll();
}

/** The full address of a captured request. */
function exchangeUrl(ex) {
  const scheme = ex.scheme || 'https';
  const port = ex.port && !((scheme === 'https' && ex.port === 443) || (scheme === 'http' && ex.port === 80)) ? ':' + ex.port : '';
  return `${scheme}://${ex.host}${port}${ex.path || '/'}${ex.query ? '?' + ex.query : ''}`;
}

/** Runs a subdomain finder over the accepted scope domains and says what it added. */
async function findSubdomains(name) {
  const r = await api('/api/extensions/' + encodeURIComponent(name) + '/run', { method: 'POST', body: {} });
  if (r.problem || r.stopped) throw new Error(r.problem || r.stopped);
  loadScope();
  return r.suggested ? `Added ${r.suggested} new subdomain${r.suggested === 1 ? '' : 's'} to Scope as suggestions. Accept or reject each one there.` : 'No new subdomains this time. Everything it found is already in Scope or was decided before.';
}

/** Probes one in-scope address for hidden query parameters and says what changed the response. */
async function probeParameters(name, url) {
  const r = await api('/api/extensions/' + encodeURIComponent(name) + '/probe', { method: 'POST', body: { url } });
  const found = r.influential || [];
  if (!found.length) return `Sent ${r.sent} parameter names to ${url}. None changed the response.`;
  return `Sent ${r.sent} parameter names. ${found.length === 1 ? 'This one changes' : 'These change'} the response: ${found.join(', ')}. ${r.proposed ? 'Check ' + (found.length === 1 ? 'it' : 'them') + ' in the new finding.' : 'A finding for ' + (found.length === 1 ? 'it' : 'them') + ' is already open.'}`;
}

/** Right-click › Probe for hidden parameters, from Traffic. */
async function probeExchange(name, ex) {
  const url = exchangeUrl(ex);
  toast(`Probing ${url}…`);
  try {
    toast(await probeParameters(name, url), 'ok');
    if (S.view === 'findings') loadFindings();
  } catch (e) {
    toast(e.message, 'err');
  }
}

/** An installed extension: on or off, why Plonix stopped it, what it is missing, and how to run it. */
function extensionState(name) {
  const x = MK.ext[name];
  if (!x) return null;
  const box = h('div', { class: 'extstate' + (x.disabled_reason ? ' stopped' : '') });
  const kind = extKind(x);
  const result = h('div', { class: 'extresult', hidden: true });
  const show = (text, kindOf, view) => {
    result.hidden = false;
    result.className = 'extresult ' + kindOf;
    clear(result, h('span', { text }), view ? h('button', { class: 'btn sm', text: view.open, onclick: () => leaveTo(view.view) }) : null);
  };
  const draw = () => {
    const on = x.enabled;
    const missing = extMissing(x);
    const blocked = !on || missing.length > 0;
    const toggle = h('button', {
      class: 'btn sm' + (on ? '' : ' primary'),
      text: on ? 'Switch off' : 'Switch on',
      onclick: async () => {
        try {
          const r = await api('/api/extensions/' + encodeURIComponent(name) + '/enabled', { method: 'PUT', body: { enabled: !on } });
          Object.assign(x, r.state);
          if (!r.state.disabled_reason) delete x.disabled_reason;
          toast(`${name} is ${x.enabled ? 'on' : 'off'}`, 'ok');
        } catch (e) {
          toast(e.message, 'err');
        }
        box.className = 'extstate' + (x.disabled_reason ? ' stopped' : '');
        draw();
        drawMarket();
        poll();
      },
    });
    let action;
    if (kind === 'probe') {
      const url = h('input', { class: 'exturl', placeholder: 'https://api.example.com/v1/items', spellcheck: 'false', autocomplete: 'off', disabled: blocked });
      const go = h('button', {
        class: 'btn sm primary',
        text: 'Probe',
        disabled: blocked,
        title: 'Send each candidate parameter name once to this in-scope address and compare the responses',
        onclick: async () => {
          if (!url.value.trim()) return url.focus();
          go.disabled = true;
          go.textContent = 'Probing…';
          try {
            show(await probeParameters(name, url.value.trim()), 'ok', { view: 'findings', open: 'Open Findings' });
          } catch (e) {
            show(e.message, 'err');
          }
          go.disabled = false;
          go.textContent = 'Probe';
        },
      });
      url.addEventListener('keydown', (e) => e.key === 'Enter' && go.click());
      action = h('div', { class: 'row extprobe' }, url, go);
    } else {
      const how = EXT_RUN[kind] || EXT_RUN.sandbox;
      const run = h('button', {
        class: 'btn sm primary',
        text: how.label,
        disabled: blocked,
        title: how.title,
        onclick: async () => {
          run.disabled = true;
          run.textContent = how.busy;
          try {
            if (kind === 'enumerate') {
              show(await findSubdomains(name), 'ok', how);
            } else {
              const r = await api('/api/extensions/' + encodeURIComponent(name) + '/run', { method: 'POST', body: {} });
              if (r.stopped || r.problem) show(r.stopped || r.problem, 'err');
              else {
                const said = kind === 'scan' ? `Checked ${r.exchanges} new request(s) and found ${r.notes} secret(s). They show in the Lens.` : `Read ${r.exchanges} request(s): ${r.notes} note(s) in the Lens, ${r.proposed} new finding(s) to review.`;
                const skipped = r.skipped ? ` It skipped ${r.skipped} request(s) to hosts outside scope; accept their hosts in Scope to include them.` : '';
                show(said + skipped, 'ok', r.proposed ? { view: 'findings', open: 'Open Findings' } : how);
              }
            }
          } catch (e) {
            show(e.message, 'err');
          }
          run.disabled = false;
          run.textContent = how.label;
        },
      });
      action = run;
    }
    clear(
      box,
      h('div', { class: 'row' }, h('b', { text: x.disabled_reason ? 'Stopped' : on ? 'On' : 'Off' }), toggle, kind === 'probe' ? null : action),
      x.disabled_reason ? h('p', { text: x.disabled_reason }) : null,
      missing.length
        ? h(
            'div',
            { class: 'extneeds' },
            h('span', { text: `It cannot run until you allow it to ${kind === 'probe' ? 'send its requests' : 'run ' + x.program.id}.` }),
            h('button', { class: 'btn sm primary', text: 'Allow', onclick: async () => { for (const c of missing) await allowCapability(name, c, true); } }),
          )
        : null,
      kind === 'probe' ? action : null,
      result,
      h('p', {
        class: 'muted',
        text: !on
          ? 'It is installed but does not run.'
          : kind === 'enumerate'
            ? 'What it finds waits in Scope as suggestions. Nothing joins your scope until you accept it. Find subdomains is also on the Scope screen.'
            : kind === 'probe'
              ? 'Paste an in-scope address, or right-click a request in Traffic and choose Probe for hidden parameters. Every request it sends shows in Traffic.'
              : kind === 'scan'
                ? 'Secrets it finds show in the Lens as Spotted chips, marked with its name.'
                : 'Its notes show in the Lens, marked with its name. Findings it proposes stay open until you confirm them.',
      }),
    );
  };
  draw();
  return box;
}

/** Adds your own package from a GitHub repository, a folder, a file or a link: look at it first, then confirm. It is marked Your own. */
function addExternal(prefill) {
  const input = h('input', { placeholder: 'github:owner/repo   or   /path/to/extension-folder   or   https://…/skill.md', spellcheck: 'false', autocomplete: 'off', value: prefill || '' });
  let boxes = [];
  // The sha256 of the file the preview showed: confirming adds only that file.
  let shown = null;
  const preview = h('div', { class: 'xpreview' });
  const check = h('button', { class: 'btn', text: 'Look at it' });
  const confirmBtn = h('button', { class: 'btn primary', text: 'Add it as your own', hidden: true });
  const pick = (folder) =>
    h('button', {
      class: 'btn sm',
      text: folder ? 'Choose a folder…' : 'Choose a file…',
      onclick: async () => {
        try {
          const r = await api('/api/market/pick', { method: 'POST', body: { folder } });
          if (r.path) {
            input.value = r.path;
            run(false);
          }
        } catch (e) {
          m.err.textContent = e.message;
        }
      },
    });
  const m = modal(
    'Add your own',
    [
      h('p', { class: 'muted mnote', text: 'From a GitHub repository (Plonix takes the package attached to its latest release, or add @tag), an extension\'s folder while you write it, a file or a link. Plonix checks it in full, shows you what it does, and adds it only after you confirm. Nobody has reviewed it, so it is marked Your own.' }),
      h('label', null, 'Repository, folder, file or address', input),
      nativeFiles() ? h('div', { class: 'xpick' }, pick(true), pick(false)) : null,
      preview,
    ],
    [h('button', { class: 'btn', text: 'Cancel', onclick: closeModal }), check, confirmBtn],
  );
  const run = async (confirm) => {
    m.err.textContent = '';
    const source = input.value.trim();
    if (!source) return input.focus();
    check.disabled = confirmBtn.disabled = true;
    if (!confirm) check.textContent = 'Reading…';
    try {
      const grant = boxes.filter((b) => b.checked).map((b) => b.value);
      const r = await api('/api/market/add', { method: 'POST', body: { source, confirm, grant, sha256: confirm ? shown : undefined } });
      if (r.added) {
        closeModal();
        toast(`Added ${r.file.name} as your own`, 'ok');
        MK.kind = 'unverified';
        delete MK.added[r.file.name];
        await loadMarket(true);
        return showPackage(r.file.name);
      }
      const f = r.file;
      shown = f.sha256;
      const caps = f.capabilities ? capabilityList(f.capabilities) : { rows: [], boxes: [] };
      boxes = caps.boxes;
      clear(
        preview,
        h('div', { class: 'xhead' }, h('b', { class: 'mono', text: f.name }), h('span', { class: 'muted', text: ` ${KIND_INFO[f.kind].one} · ${f.version} · ${f.author}` })),
        h('p', { text: f.description }),
        h('p', { class: 'muted fine', text: 'From ' + f.source }),
        f.kind === 'extension' ? [caps.rows, h('div', { class: 'mhow' }, h('div', { class: 'mcaph', text: 'How it runs' }), h('p', { text: f.effects[f.effects.length - 1] }))] : f.effects.map((e) => h('div', { class: 'mcap' }, h('span', { class: 'mdot', text: '•' }), h('span', { text: e }))),
        f.replaces ? h('p', { class: 'muted', text: `This replaces ${f.name} ${f.replaces}, which is installed.` }) : null,
        h(
          'div',
          { class: 'mtrustbox warn' },
          h('span', { class: 'trust warn' }, h('i', { text: '!' }), 'Your own'),
          f.kind === 'extension' ? notReviewed({ shelf: 'own' }) : h('p', { text: 'It did not come from a signed Market. It is checked and cannot run code, but nobody has reviewed what it says or does.' }),
        ),
        h('p', { class: 'muted fine mono', text: 'sha256 ' + f.sha256 }),
      );
      confirmBtn.hidden = false;
    } catch (e) {
      m.err.textContent = e.message;
      confirmBtn.hidden = true;
      clear(preview);
    }
    check.textContent = 'Look at it';
    check.disabled = confirmBtn.disabled = false;
  };
  check.onclick = () => run(false);
  confirmBtn.onclick = () => run(true);
  input.addEventListener('input', () => ((confirmBtn.hidden = true), (shown = null), clear(preview)));
  input.addEventListener('keydown', (e) => e.key === 'Enter' && run(false));
  if (prefill) run(false);
}

async function updateAll() {
  try {
    const r = await api('/api/market/update', { method: 'POST', body: {} });
    const failed = r.failed || [];
    const done = `Updated ${(r.changes || []).length} package(s)`;
    if (failed.length) toast(`${done}. Not updated: ${failed.map((f) => `${f.name} (${f.error})`).join('; ')}`, 'err');
    else toast(done, 'ok');
  } catch (e) {
    toast(e.message, 'err');
  }
  loadMarket(false);
}

function closePackage() {
  MK.sel = null;
  const body = $('#mbody');
  if (body) body.classList.remove('paged');
  const slot = $('#mdetail');
  if (slot) clear(slot);
  for (const c of document.querySelectorAll('.mpkg')) c.classList.remove('sel');
}

/** How to use an item: where it shows up, the steps, a screenshot of it at work, and a way to get there. */
function guideSection(g) {
  const view = g.open && VIEWS[g.open] && (!VIEWS[g.open].tool || toolOn(VIEWS[g.open].tool)) ? g.open : null;
  return h(
    'div',
    { class: 'msec mguide' },
    h('h4', { text: 'How to use it' }),
    h('div', { class: 'mwhere' }, h('span', { class: 'muted', text: 'Where: ' }), h('b', { text: g.where }), view ? h('button', { class: 'btn sm', text: 'Open ' + VIEWS[view].label, onclick: () => leaveTo(view) }) : null),
    h('ol', { class: 'msteps' }, g.steps.map((t) => h('li', { text: t }))),
    g.shot ? h('img', { class: 'mshot', src: '/ui/guide/' + g.shot + '.jpg', alt: 'Screenshot of ' + g.where, loading: 'lazy' }) : null,
    g.cli ? h('p', { class: 'muted fine' }, 'Also: ', h('code', { text: g.cli })) : null,
  );
}

/** An item's own page: what it is, what it does, what it needs, who made it and how far to trust it. */
async function showPackage(name) {
  MK.sel = name;
  const mbody = $('#mbody');
  if (mbody) mbody.classList.add('paged');
  for (const c of document.querySelectorAll('.mpkg')) c.classList.toggle('sel', c.querySelector('b').textContent === name);
  const slot = $('#mdetail');
  if (!slot) return;
  const p = MK.data && MK.data.packages.find((x) => x.name === name);
  if (!p) return;
  const panel = h('article', { class: 'mside mpage' }, h('div', { class: 'muted', text: 'Loading…' }));
  clear(slot, panel);
  let d;
  try {
    d = await api('/api/market/' + encodeURIComponent(name));
  } catch (e) {
    return clear(panel, h('div', { class: 'perr', text: e.message }));
  }
  if (MK.sel !== name) return;
  const k = KIND_INFO[p.kind] || { one: p.kind, ico: '•' };
  const st = marketStatus(p);
  const det = d.detail || {};
  const sec = (title, ...kids) => h('div', { class: 'msec' }, h('h4', { text: title }), kids);
  const parts = [];
  const about = p.about && p.about.length ? p.about : [p.description];
  if (d.guide) parts.push(guideSection(d.guide));
  parts.push(sec('About', about.map((t) => h('p', { class: 'mabout', text: t }))));
  const rows = [
    ['Type', k.one],
    ['Version', p.version + (st.action === 'update' ? ` (you have ${p.status.installed})` : '')],
    ['Made by', p.author + (p.local ? ' · added by you' : '')],
    ['Status', st.text],
    p.sha256 ? ['Checksum', h('span', { class: 'mono fine', text: 'sha256 ' + p.sha256 })] : null,
    p.url ? ['Source', h('span', { class: 'mono fine', text: p.url })] : null,
    p.homepage ? ['Homepage', h('a', { class: 'link', href: p.homepage, target: '_blank', rel: 'noopener', text: p.homepage })] : null,
  ].filter(Boolean);
  parts.push(sec('Details', h('dl', { class: 'mdl' }, rows.map(([a, b]) => [h('dt', { text: a }), h('dd', null, b)]))));
  const within = (MK.data.packages || []).filter((x) => x.kind === 'bundle' && (x.includes || []).includes(name) && x.name !== name);
  if (within.length) parts.push(sec('Part of', h('div', { class: 'mchips' }, within.map((b) => h('button', { class: 'chip', text: b.name, onclick: () => showPackage(b.name) })))));
  if (p.includes && p.includes.length) {
    parts.push(sec(p.kind === 'bundle' ? 'Installs' : 'Also installs', h('div', { class: 'mchips' }, p.includes.map((n) => h('button', { class: 'chip', text: n, onclick: () => showPackage(n) })))));
  }
  if (det.skill) {
    const sk = det.skill;
    parts.push(
      sec(
        'What agents can read with it',
        h('div', { class: 'mchips' }, sk.uses.map((g) => h('span', { class: 'chip' + (sk.missing.includes(g) ? ' k-neg' : ''), text: groupLabel(g) }))),
        h('p', { class: 'muted fine', text: sk.available ? 'Read-only, like every agent tool. A skill never gives an agent more than your agent settings allow.' : 'Agents will not be offered this skill: ' + sk.missing.map(groupLabel).join(', ') + ' is switched off in Settings › AI agents.' }),
      ),
    );
    if (sk.arguments.length) parts.push(sec('Asks for', sk.arguments.map((a) => h('div', { class: 'marg' }, h('code', { text: a.name }), h('span', { class: 'muted', text: (a.required ? '' : '(optional) ') + a.description })))));
    parts.push(sec('Instructions', h('pre', { class: 'mpre', text: sk.instructions })));
    parts.push(h('p', { class: 'muted fine' }, 'In Claude Code, run ', h('code', { text: '/mcp__plonix__' + p.name }), ' once it is installed.'));
  }
  if (det.extension) {
    const x = det.extension;
    const granted = x.installed ? x.installed.granted : null;
    if (x.installed) parts.unshift(sec('In this Plonix', extensionState(name)));
    if (x.program) parts.push(sec('Needs', programNeeds(x.program)));
    parts.push(
      sec(
        granted ? 'Allowed to' : 'Would be allowed to',
        x.capabilities.map((c) => {
          const off = granted && !granted.includes(c.id);
          // A sensitive one can be allowed, or taken back, here at any time.
          const change = granted && c.sensitive ? h('button', { class: 'btn sm' + (off ? ' primary' : ' ghost'), text: off ? 'Allow' : 'Take back', onclick: () => allowCapability(name, c.id, off) }) : null;
          return h('div', { class: 'mcap' + (off ? ' off' : '') }, capIcon(off ? 'off' : c.sensitive ? 'warn' : 'ok'), h('span', { text: c.what + (off ? ' (not allowed yet)' : c.sensitive && !granted ? ' (asks for your yes when you install)' : '') }), change);
        }),
        h('p', { class: 'muted fine', text: x.installable ? x.sandbox : x.why_not ? 'Not installable in this version: ' + x.why_not : 'Listed so you can see what is coming; its code is not published yet.' }),
      ),
    );
  }
  if (det.rules) parts.push(sec(`Detects ${det.rules.count} technologies`, h('div', { class: 'mchips' }, det.rules.detects.map((n) => h('span', { class: 'chip', text: n })))));
  if (det.filters) parts.push(sec('Filters', det.filters.map((f) => h('div', { class: 'marg' }, h('code', { text: 'is:' + f.id }), h('span', { class: 'muted', text: f.label + ' · ' + f.query })))));
  if (det.platform)
    parts.push(
      sec(
        'Programs',
        h('p', { class: 'muted', text: `Lists your ${det.platform.title} programs on the Programs screen and brings one in with its scope and rules. Plonix only talks to ${det.platform.api}, with the token you give it.` }),
        toolOn('programs')
          ? h('button', { class: 'btn sm', text: 'Open Programs', onclick: () => leaveTo('programs') })
          : h('div', null, h('p', { class: 'muted fine', text: 'Needs the Programs tool, which adds the Programs screen.' }), h('button', { class: 'btn sm', text: 'Get Programs', onclick: () => showPackage('programs') })),
      ),
    );
  if (det.lists) parts.push(sec('Lists', det.lists.map((l) => h('div', { class: 'marg' }, h('code', { text: l.id }), h('span', { class: 'muted', text: `${l.title} · ${l.count} value${l.count === 1 ? '' : 's'}` })))));
  clear(
    panel,
    h('div', { class: 'mback' }, h('button', { class: 'btn sm', text: '← Market', title: 'Back to the list', onclick: () => (closePackage(), drawMarket()) })),
    h('div', { class: 'mside-h' }, h('span', { class: 'mico big k-' + p.kind, text: k.ico }), h('div', null, h('h3', { text: p.name }), h('div', { class: 'muted', text: `${k.one} · ${p.version} · ${p.author}` }))),
    h('p', { class: 'mdesc', text: p.description }),
    h('div', { class: 'mact' }, h('span', { class: st.cls, text: st.text }), st.action ? marketButton(p, st.action, false) : null, st.action === 'update' ? marketButton(p, 'remove', false) : null),
    h('div', { class: 'mtrustbox ' + (trustClass(p.verification) === 'in' ? 'ok' : trustClass(p.verification)) }, trustBadge(p.verification, true), h('p', { text: p.verification.detail })),
    parts,
  );
}
