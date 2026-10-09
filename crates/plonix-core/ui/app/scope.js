// Plonix window: Scope: rules, suggestions and exclusions.
// Part of the project window's scripts (see index.html for the load order).
'use strict';

/* ======================================================================
   Scope
   ====================================================================== */

function renderScope(main) {
  clear(
    main,
    h(
      'div',
      { class: 'view' },
      h('div', { class: 'toolbar' }, h('h2', { text: 'Scope' }), h('span', { class: 'hint', text: 'Plonix learns which domains belong to your target as you browse. You decide.' })),
      h('div', { class: 'pane' }, h('div', { class: 'stack', id: 'scopebody' })),
    ),
  );
  renderScopeBody();
}

/** Scope's Find subdomains button, there while a subdomain finder is switched on. */
function findSubdomainsButton(name) {
  const b = h('button', {
    class: 'btn sm',
    text: 'Find subdomains',
    title: `Run ${name}: ask public sources for subdomains of the domains you accepted, and add them here as suggestions`,
    onclick: async () => {
      b.disabled = true;
      b.textContent = 'Looking up subdomains…';
      try {
        toast(await findSubdomains(name), 'ok');
      } catch (e) {
        toast(e.message, 'err');
      }
      b.disabled = false;
      b.textContent = 'Find subdomains';
    },
  });
  return b;
}

function renderScopeBody() {
  const box = $('#scopebody');
  if (!box) return;
  const domain = h('input', { type: 'text', placeholder: 'example.com or *.example.com', spellcheck: 'false' });
  const subs = h('input', { type: 'checkbox' });
  const add = async (action) => {
    const d = domain.value.trim();
    if (!d) return domain.focus();
    if (await decideDomain(action, d, subs.checked)) {
      domain.value = '';
      renderScopeBody();
    }
  };
  domain.addEventListener('keydown', (e) => e.key === 'Enter' && add('accept'));
  const sugg = stillPending(S.scope.suggestions);
  const rules = (S.scope.rules || [])
    .filter((r) => !(r.note || '').startsWith('group:'))
    .slice()
    .sort((x, y) => x.decision.localeCompare(y.decision) || x.pattern.localeCompare(y.pattern));
  clear(
    box,
    h(
      'div',
      { class: 'sechead' },
      h('h3', { text: `Suggested domains (${sugg.length})` }),
      h(
        'span',
        { class: 'shacts' },
        extsThat('enumerate').map((name) => findSubdomainsButton(name)),
        sugg.length > 1 ? [h('button', { class: 'btn sm', text: 'Accept all', title: 'Accept every suggested host on its own', onclick: () => decideAll('accept') }), h('button', { class: 'btn sm danger', text: 'Reject all', onclick: () => decideAll('reject') })] : null,
      ),
    ),
    sugg.length
      ? sugg.map((s) =>
          h(
            'div',
            { class: 'card sugg' },
            h(
              'div',
              { class: 'top' },
              h('span', { class: 'dom', text: s.domain }),
              h('span', { class: 'meta', text: `${s.requests} request${s.requests === 1 ? '' : 's'} · score ${s.score}` }),
              h(
                'span',
                { class: 'acts' },
                askButton({ kind: 'host', host: suggestionBase(s.domain) }, 'Ask whether this host belongs to your target'),
                h('button', { class: 'btn sm', text: 'Traffic', onclick: () => setQuery('host:' + s.domain) }),
                scopeButtons(s, 'sm', renderScopeBody),
              ),
            ),
            evidenceList(s.evidence),
          ),
        )
      : h('div', { class: 'card' }, h('div', { class: 'empty', text: 'Nothing to review. Suggestions appear when in-scope pages call, redirect to, link to or share a session with another domain.' })),
    h('div', { class: 'sechead' }, h('h3', { text: `Rules (${rules.length})` })),
    h(
      'div',
      { class: 'card' },
      h(
        'div',
        { class: 'addrule' },
        domain,
        h('label', null, subs, 'include subdomains'),
        h('button', { class: 'btn danger', text: 'Reject', onclick: () => add('reject') }),
        h('button', { class: 'btn primary', text: 'Accept', onclick: () => add('accept') }),
      ),
      rules.length
        ? h(
            'table',
            { class: 'grid' },
            h('thead', null, h('tr', null, h('th', { text: 'Domain' }), h('th', { text: 'Decision' }), h('th', { text: 'Note' }), h('th', { text: 'Since' }), h('th'))),
            h(
              'tbody',
              null,
              rules.map((r) =>
                h(
                  'tr',
                  null,
                  h('td', { class: 'mono', text: (r.include_subdomains ? '*.' : '') + r.pattern }),
                  h('td', null, h('span', { class: 'tag ' + scopeTag(r.decision), text: r.decision })),
                  h('td', { class: 'muted', text: r.note || '' }),
                  h('td', { class: 'muted', text: fmtDate(r.created_at) }),
                  h(
                    'td',
                    { style: { textAlign: 'right' } },
                    h('button', {
                      class: 'btn sm',
                      text: 'Remove',
                      onclick: async () => {
                        try {
                          await api('/api/scope/remove', { method: 'POST', body: { domain: r.pattern } });
                          toast('Removed the rule for ' + r.pattern);
                          await loadScope();
                          renderScopeBody();
                        } catch (e) {
                          toast(e.message, 'err');
                        }
                      },
                    }),
                  ),
                ),
              ),
            ),
          )
        : null,
    ),
    excludedSection(),
  );
}

/* ---- exclusions: grouped out-of-scope domains ---- */

/** Which exclusion groups are expanded on the Scope screen. */
const X = { open: {} };

const groupStateLabel = { on: 'On', partial: 'Some', off: 'Off' };

async function reloadExclusions(ex) {
  if (ex) S.exclusions = ex;
  // Group changes add or drop reject rules, so refresh the rest of the screen too.
  await loadScope();
  renderScopeBody();
}

async function toggleGroup(id, on) {
  try {
    const ex = await api('/api/scope/exclusions/group', { method: 'POST', body: { id, on } });
    toast(on ? 'Excluded the group' : 'Removed the group from exclusions', on ? 'ok' : '');
    await reloadExclusions(ex);
  } catch (e) {
    toast(e.message, 'err');
  }
}

async function toggleExcludedDomain(id, host, on) {
  try {
    const ex = await api('/api/scope/exclusions/domain', { method: 'POST', body: { id, host, on } });
    await reloadExclusions(ex);
  } catch (e) {
    toast(e.message, 'err');
  }
}

function groupCard(g) {
  const open = !!X.open[g.id];
  const count = g.domains.filter((d) => d.excluded).length;
  const header = h(
    'div',
    { class: 'top' },
    h(
      'button',
      { class: 'disclose', title: open ? 'Hide domains' : 'Show domains', onclick: () => ((X.open[g.id] = !open), renderScopeBody()) },
      h('span', { class: 'caret', text: open ? '▾' : '▸' }),
      h('span', { class: 'dom', text: g.name }),
    ),
    h('span', { class: 'meta', text: `${count}/${g.domains.length} excluded${g.builtin ? '' : ' · custom'}` }),
    h(
      'span',
      { class: 'acts' },
      h('span', { class: 'tag ' + (g.state === 'off' ? 'out' : g.state === 'on' ? 'rej' : ''), text: groupStateLabel[g.state] }),
      h('button', { class: 'btn sm', text: g.state === 'on' ? 'Turn off' : 'Exclude all', onclick: () => toggleGroup(g.id, g.state !== 'on') }),
      g.builtin ? null : h('button', { class: 'btn sm danger', text: 'Delete', title: 'Delete this custom group', onclick: () => removeCustomGroup(g) }),
    ),
  );
  const desc = g.description ? h('div', { class: 'gdesc muted', text: g.description }) : null;
  const list = open
    ? h(
        'div',
        { class: 'domlist' },
        g.domains.map((d) =>
          h(
            'label',
            { class: 'domrow' },
            h('input', { type: 'checkbox', checked: d.excluded, onchange: (e) => toggleExcludedDomain(g.id, d.host, e.target.checked) }),
            h('span', { class: 'mono', text: d.host }),
          ),
        ),
      )
    : null;
  return h('div', { class: 'card group' }, header, desc, list);
}

function newGroupForm() {
  const name = h('input', { type: 'text', placeholder: 'Group name, e.g. Vendor widgets', spellcheck: 'false' });
  const domains = h('textarea', { placeholder: 'One domain per line, or comma-separated', rows: '3', spellcheck: 'false' });
  const create = async () => {
    const list = domains.value
      .split(/[\s,]+/)
      .map((d) => d.trim())
      .filter(Boolean);
    if (!name.value.trim()) return name.focus();
    if (!list.length) return domains.focus();
    try {
      const res = await api('/api/scope/exclusions/custom', { method: 'POST', body: { id: '', name: name.value.trim(), domains: list } });
      name.value = '';
      domains.value = '';
      toast('Created the group. Turn it on to exclude its domains.', 'ok');
      await reloadExclusions(res.exclusions);
    } catch (e) {
      toast(e.message, 'err');
    }
  };
  return h(
    'div',
    { class: 'card newgroup' },
    h('div', { class: 'caph', text: 'New group' }),
    name,
    domains,
    h('div', { class: 'row end' }, h('button', { class: 'btn primary', text: 'Create group', onclick: create })),
  );
}

function removeCustomGroup(g) {
  modal(
    'Delete group',
    h('p', null, 'Delete the custom group ', h('b', { text: g.name }), ' and remove any exclusions it added? This cannot be undone.'),
    [
      h('button', { class: 'btn', text: 'Cancel', onclick: closeModal }),
      h('button', {
        class: 'btn danger',
        text: 'Delete group',
        onclick: async () => {
          closeModal();
          try {
            const ex = await api('/api/scope/exclusions/custom', { method: 'DELETE', body: { id: g.id } });
            toast('Deleted the group');
            await reloadExclusions(ex);
          } catch (e) {
            toast(e.message, 'err');
          }
        },
      }),
    ],
  );
}

function excludedSection() {
  const groups = (S.exclusions && S.exclusions.groups) || [];
  const total = groups.reduce((n, g) => n + g.domains.filter((d) => d.excluded).length, 0);
  return h(
    'div',
    { class: 'excluded' },
    h(
      'div',
      { class: 'sechead' },
      h('h3', { text: `Excluded domains (${total})` }),
      h('span', { class: 'hint', text: 'Hosts you never want captured as targets. They are never suggested and never sent to.' }),
    ),
    groups.length ? groups.map(groupCard) : h('div', { class: 'card' }, h('div', { class: 'empty', text: 'No exclusion groups.' })),
    newGroupForm(),
  );
}

/** First run: offer to exclude common third-party domains, once per project. */
function maybeAskExclusions() {
  if (!S.exclusions || S.exclusions.asked || S.askedExclusionsThisSession) return;
  const groups = S.exclusions.groups || [];
  if (!groups.length) return;
  S.askedExclusionsThisSession = true;
  const picks = {};
  groups.forEach((g) => (picks[g.id] = true));
  const rows = groups.map((g) =>
    h(
      'label',
      { class: 'domrow' },
      h('input', { type: 'checkbox', checked: true, onchange: (e) => (picks[g.id] = e.target.checked) }),
      h('span', null, h('b', { text: g.name }), ' ', h('span', { class: 'muted', text: `(${g.domains.length} domains)` })),
    ),
  );
  const finish = async (enable) => {
    closeModal();
    try {
      if (enable) {
        for (const g of groups) if (picks[g.id]) await api('/api/scope/exclusions/group', { method: 'POST', body: { id: g.id, on: true } });
      }
      await api('/api/scope/exclusions/asked', { method: 'POST' });
      await loadScope();
      if (S.view === 'scope') renderScopeBody();
      if (enable) toast('Common domains excluded. Edit them anytime on the Scope screen.', 'ok');
    } catch (e) {
      toast(e.message, 'err');
    }
  };
  modal(
    'Exclude common domains?',
    h(
      'div',
      null,
      h('p', { class: 'muted', text: 'Plonix can keep common third parties — analytics, ads, payments, CDNs and the like — out of your target scope, so they are never suggested or sent to. Pick the groups to exclude; you can change these anytime on the Scope screen.' }),
      h('div', { class: 'domlist' }, rows),
    ),
    [
      h('button', { class: 'btn', text: 'Not now', onclick: () => finish(false) }),
      h('button', { class: 'btn primary', text: 'Exclude selected', onclick: () => finish(true) }),
    ],
  );
}
