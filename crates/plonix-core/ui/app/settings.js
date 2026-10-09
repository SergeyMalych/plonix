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
        await api('/api/ui/style', { method: 'PUT', body: { style: values.style } }).catch(() => {});
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
    },
  });
}

/** How Plonix looks on this computer. Kept in the window, not the engine. */
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
      { key: 'style', label: 'Style', type: 'choice', help: 'Classic is the standard Plonix look. Studio adds cream paper, a blue sidebar, round controls and a shape for every tool. Works with light, dark and both spacings.', options: [{ value: 'classic', label: 'Classic' }, { value: 'studio', label: 'Studio' }] },
    ],
    values: { theme: S.theme || 'auto', density: S.density || 'dense', style: S.style || 'classic' },
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
