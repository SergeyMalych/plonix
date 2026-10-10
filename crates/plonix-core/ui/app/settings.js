// Plonix window: Settings.
// Part of the project window's scripts (see index.html for the load order).
'use strict';

/* ---------- settings ---------- */

async function renderSettings(main) {
  const box = h('div', { class: 'view settingsview' }, h('div', { class: 'empty', text: 'Loading settings…' }));
  main.append(box);
  let data;
  try {
    data = await api('/api/settings');
  } catch (e) {
    clear(box, h('div', { class: 'empty', text: e.message }));
    return;
  }
  if (S.view !== 'settings') return;
  data.sections = [appearanceSection(), ...(data.sections || [])];
  const host = h('div', { style: { flex: '1', minHeight: '0', display: 'flex' } });
  const back = backButton();
  clear(box, back ? h('div', { class: 'toolbar' }, back) : null, host);
  PlonixSettings.render(host, data, {
    select: S.settingsSection || 'proxy',
    onSelect: (id) => (S.settingsSection = id),
    save: async (section, values) => {
      if (section === 'appearance') {
        applyTheme(values.theme);
        store('plonix.theme', values.theme);
        applyDensity(values.density);
        store('plonix.density', values.density);
        applyStyle(values.style);
        store('plonix.style', values.style);
        await api('/api/ui/style', { method: 'PUT', body: values }).catch(() => {});
        return { applies: 'now' };
      }
      const r = await api('/api/settings/' + section, { method: 'PUT', body: { values } });
      if (section === 'agents') loadAgentSettings();
      if (section === 'intercept') loadIntercept();
      if (section === 'proxy') {
        S.status = await api('/api/status');
        updateChrome();
        r.message = 'Saved and applied. The proxy listens on ' + r.proxy + '.';
      }
      return r;
    },
    extra: (section, el) => {
      if (section.id === 'proxy') el.append(proxyPanel());
      if (section.id === 'storage') el.append(storagePanel());
      if (section.id === 'replace') el.append(replacePanel());
      if (section.id === 'client-certs') el.append(clientCertPanel());
      if (section.id === 'usage') el.append(usagePanel());
    },
  });
}

/** How Plonix looks on this computer: every window and the Start screen share it. */
function appearanceSection() {
  return {
    id: 'appearance',
    title: 'Appearance',
    level: 'device',
    description: 'How Plonix looks on this computer.',
    applies: 'now',
    fields: [
      { key: 'theme', label: 'Theme', type: 'choice', options: [{ value: 'auto', label: 'Match system' }, { value: 'light', label: 'Light' }, { value: 'dark', label: 'Dark' }] },
      { key: 'density', label: 'Spacing', type: 'choice', help: 'Dense fits more on screen. Roomy gives rows and panels more air.', options: [{ value: 'dense', label: 'Dense' }, { value: 'roomy', label: 'Roomy' }] },
      { key: 'style', label: 'Style', type: 'choice', help: 'Studio is the standard Plonix look: cream paper, a blue sidebar, round controls and a shape for every tool. Classic is the plainer indigo look. Both work with light, dark and both spacings.', options: [{ value: 'studio', label: 'Studio' }, { value: 'classic', label: 'Classic' }] },
    ],
    values: { theme: S.theme || 'auto', density: S.density || 'dense', style: S.style || 'studio' },
  };
}

function proxyPanel() {
  const st = S.status || {};
  return h(
    'div',
    { class: 'spanel' },
    h('h4', { text: 'Listening now' }),
    h('p', null, 'This project\'s proxy is at ', h('b', { class: 'mono', text: st.proxy || '…' }), '. Other open projects have proxies of their own.'),
    h('p', null, 'Devices that should capture through it need the Plonix certificate, from ', h('span', { class: 'mono', text: 'http://' + (st.proxy || '') + '/ca.pem' }), ' through the proxy.'),
  );
}

function storagePanel() {
  const panel = h('div', { class: 'spanel' }, h('h4', { text: 'Out-of-scope traffic' }), h('p', { text: 'Counting…' }));
  api('/api/storage')
    .then((s) => {
      const st = s.stats;
      const rows = [
        h('h4', { text: 'Out-of-scope traffic' }),
        h('p', { text: `${st.out_of_scope} of ${st.total} captured request(s) are to hosts that are not in scope.` }),
      ];
      if (s.last_prune) {
        const r = s.last_prune;
        rows.push(h('p', { class: 'muted', text: r.skipped ? `Last time: ${r.skipped}.` : `Last time (${fmtDate(r.at)}): deleted ${r.removed}, kept ${r.kept}.` }));
      }
      if (!st.in_scope_rules) rows.push(h('p', { class: 'muted', text: 'Nothing is in scope yet, so nothing would be deleted.' }));
      const btn = h('button', { class: 'btn danger', text: 'Delete Out-of-Scope Traffic Now…', disabled: !st.in_scope_rules || !st.out_of_scope, onclick: () => confirmPrune(st) });
      rows.push(h('div', { class: 'row' }, btn));
      clear(panel, rows);
    })
    .catch((e) => clear(panel, h('p', { text: e.message })));
  return panel;
}

/** Usage statistics: the report exactly as it would be sent, and a way to start over. */
function usagePanel() {
  const panel = h('div', { class: 'spanel' }, h('h4', { text: 'What is sent' }), h('p', { text: 'Loading…' }));
  const show = (v) => {
    const state = v.sharing ? 'On.' : v.disabled_by_env ? 'Off: PLONIX_NO_ANALYTICS or DO_NOT_TRACK is set on this computer.' : 'Off. Nothing is counted or sent.';
    const last = v.last_sent ? ' Last sent ' + fmtDate(v.last_sent * 1000) + '.' : '';
    clear(
      panel,
      h('h4', { text: 'What is sent' }),
      h('p', null, state + last + ' The totals from everyone who shares are public at ', h('a', { href: 'https://plonix.io/analytics', target: '_blank', rel: 'noopener', text: 'plonix.io/analytics' }), '.'),
      h(
        'div',
        { class: 'row' },
        h('button', { class: 'btn', text: 'Show What Is Sent…', onclick: () => showReport(v) }),
        h('button', {
          class: 'btn',
          text: 'Reset Install ID',
          title: 'Deletes the counts kept so far; the next one starts with a new random id',
          disabled: !v.sharing,
          onclick: async () => show(await api('/api/usage/reset', { method: 'POST' })),
        }),
      ),
    );
  };
  api('/api/usage')
    .then(show)
    .catch((e) => clear(panel, h('p', { text: e.message })));
  return panel;
}

function showReport(v) {
  modal(
    'The next usage report',
    [
      h('p', { class: 'mnote', text: `Sent at most once a day to ${v.endpoint}, exactly as below. Sizes are ranges; rejected hosts outside a fixed list of well-known services are only counted as "other".` }),
      h('pre', { class: 'mono', style: { maxHeight: '50vh', overflow: 'auto', whiteSpace: 'pre-wrap' }, text: JSON.stringify(v.next_report, null, 2) }),
    ],
    [h('button', { class: 'btn primary', text: 'Done', onclick: closeModal })],
    true,
  );
}
