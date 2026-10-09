// Plonix window: Rules: change traffic as it passes.
// Part of the project window's scripts (see index.html for the load order).
'use strict';

/* ---------- rules: change traffic as it passes ----------
   Plain header rules (add, change, remove) and text rules, each with where
   it applies (browser, Bench, Scans) and an optional condition written as a
   traffic search. The proxy and the engine apply them; nothing here sends. */

const REPLACE_TARGETS = [
  ['request_line', 'Request line'],
  ['request_header', 'Request headers'],
  ['request_body', 'Request body'],
  ['response_header', 'Response headers'],
  ['response_body', 'Response body'],
];

const RULE_KINDS = [
  ['add_header', 'Add header', 'Sends a header that isn’t there yet'],
  ['change_header', 'Change header', 'Gives a header a new value'],
  ['remove_header', 'Remove header', 'Takes a header out'],
  ['replace', 'Replace text', 'Finds text and puts other text in its place'],
];

/** The rules and the all-rules switch, as last loaded. */
const RL = { data: null };

async function loadRules() {
  try {
    RL.data = await api('/api/replace');
  } catch {
    RL.data = null;
  }
  drawRulesPill();
  drawRulesCount();
  return RL.data;
}

const activeRules = () => (RL.data && RL.data.enabled ? RL.data.rules.filter((r) => r.enabled) : []);

function drawRulesCount() {
  const ct = $('#ct-rules');
  if (!ct || !RL.data) return;
  const n = activeRules().length;
  ct.textContent = RL.data.enabled ? (n ? n + ' on' : '') : RL.data.rules.length ? 'paused' : '';
}

const ruleSide = (r) => (r.target.startsWith('response') ? 'response' : 'request');

/** One line that says what a rule does, in plain words. */
function ruleSummary(r) {
  const side = ruleSide(r) === 'response' ? ' (response)' : '';
  switch (r.kind) {
    case 'add_header':
      return { tag: 'Add header', cls: 'add', text: `${r.pattern}: ${r.replace}${side}` };
    case 'set_header':
      return { tag: 'Add header', cls: 'add', text: `${r.pattern}: ${r.replace}${side}`, note: 'replaces any value already there' };
    case 'change_header':
      return { tag: 'Change header', cls: 'chg', text: `${r.pattern} → ${r.replace}${side}` };
    case 'remove_header':
      return { tag: 'Remove header', cls: 'del', text: r.pattern + side };
    default: {
      const where = (REPLACE_TARGETS.find(([k]) => k === r.target) || [r.target, r.target])[1].toLowerCase();
      return { tag: 'Replace text', cls: 'txt', text: `${r.pattern} → ${r.replace === '' ? '(nothing)' : r.replace}`, note: where + (r.regex ? ', regex' : '') };
    }
  }
}

async function setRulesOn(on) {
  try {
    await api('/api/settings/replace', { method: 'PUT', body: { values: { enabled: on } } });
    toast(on ? 'Rules are on again.' : 'All rules are paused. Traffic passes unchanged.', 'ok');
  } catch (e) {
    toast(e.message, 'err');
  }
  await loadRules();
  if (S.view === 'rules') drawRules();
}

async function renderRules(main) {
  const toggle = h('label', { class: 'rltoggle', id: 'rltoggle' });
  clear(
    main,
    h(
      'div',
      { class: 'view rulesview' },
      h(
        'div',
        { class: 'toolbar' },
        backButton(),
        h('h2', { text: 'Rules' }),
        h('span', { class: 'muted', text: 'Change traffic as it passes. Applied top to bottom.' }),
        h('span', { class: 'spacer' }),
        toggle,
        h('button', { class: 'btn sm primary', text: '+ Add rule', onclick: () => ruleDialog() }),
      ),
      h('div', { class: 'rlbody', id: 'rlbody' }, h('div', { class: 'empty', text: 'Loading rules…' })),
    ),
  );
  await loadRules();
  drawRules();
}

function drawRules() {
  const body = $('#rlbody');
  if (!body) return;
  const d = RL.data;
  if (!d) return clear(body, h('div', { class: 'empty', text: 'The rules could not be loaded.' }));
  clear(
    $('#rltoggle'),
    h('input', { type: 'checkbox', class: 'switch', checked: d.enabled, onchange: (e) => setRulesOn(e.target.checked) }),
    d.enabled ? 'All rules on' : 'All rules paused',
  );
  if (!d.rules.length) {
    return clear(
      body,
      h(
        'div',
        { class: 'rlempty' },
        h('h3', { text: 'No rules yet' }),
        h('p', { class: 'muted', text: 'A rule changes traffic as it passes: send a header on every request, change one, remove one, or replace any text. Right-click a header in the Lens to make a rule from it.' }),
        h('button', { class: 'btn primary', text: '+ Add rule', onclick: () => ruleDialog() }),
      ),
    );
  }
  const reach = (r) =>
    [r.browser ? 'Browser' : null, r.bench ? 'Bench' : null, r.scans ? 'Scans' : null, r.in_scope_only ? 'In scope' : null]
      .filter(Boolean)
      .map((t) => h('span', { class: 'rlchip' + (t === 'In scope' ? ' scope' : ''), text: t }));
  const row = (r) => {
    const s = ruleSummary(r);
    return h(
      'div',
      { class: 'rlrow' + (r.enabled && d.enabled ? '' : ' off'), onclick: (e) => !e.target.closest('input, button') && ruleDialog(r) },
      h('input', {
        type: 'checkbox',
        class: 'switch sm',
        checked: r.enabled,
        title: r.enabled ? 'On: switch this rule off' : 'Off: switch this rule on',
        onchange: async (e) => {
          try {
            await api('/api/replace/' + r.id, { method: 'PATCH', body: { enabled: e.target.checked } });
          } catch (err) {
            toast(err.message, 'err');
          }
          await loadRules();
          drawRules();
        },
      }),
      h('span', { class: 'rlkind ' + s.cls, text: s.tag }),
      h(
        'span',
        { class: 'rlwhat' },
        h('span', { class: 'mono', text: s.text }),
        s.note ? h('span', { class: 'muted', text: s.note }) : null,
        r.when ? h('span', { class: 'rlwhen', title: 'Only when the exchange matches this search' }, 'only when ', h('code', { text: r.when })) : null,
        r.note ? h('span', { class: 'muted', text: '· ' + r.note }) : null,
      ),
      h('span', { class: 'rlreach' }, reach(r)),
      h('button', {
        class: 'iconbtn',
        text: '✕',
        title: 'Remove this rule',
        onclick: async () => {
          try {
            await api('/api/replace/' + r.id, { method: 'DELETE' });
          } catch (err) {
            toast(err.message, 'err');
          }
          await loadRules();
          drawRules();
        },
      }),
    );
  };
  clear(
    body,
    d.enabled ? null : h('div', { class: 'rlnote', text: 'All rules are paused: traffic passes unchanged until you switch them back on.' }),
    h('div', { class: 'rllist' }, d.rules.map(row)),
    h('p', { class: 'muted rltip', text: 'Click a rule to edit it. Right-click any header in the Lens to make a rule from it.' }),
  );
}

/**
 * The Add rule popup, empty, filled in from the Lens (`seed`), or for editing
 * an existing rule (one with an id). Nothing applies until Save.
 */
function ruleDialog(seed = {}) {
  const editing = seed.id != null;
  const st = {
    kind: seed.kind === 'set_header' ? 'add_header' : seed.kind || 'add_header',
    side: seed.target && seed.target.startsWith('response') ? 'response' : 'request',
    target: seed.target && seed.kind === 'replace' ? seed.target : 'request_header',
    browser: seed.browser != null ? seed.browser : true,
    bench: seed.bench != null ? seed.bench : true,
    scans: seed.scans != null ? seed.scans : true,
    scoped: seed.in_scope_only != null ? seed.in_scope_only : true,
  };
  const field = (label, input, hint) => h('label', null, label, input, hint ? h('span', { class: 'rlhint', text: hint }) : null);
  const name = h('input', { type: 'text', class: 'mono', placeholder: 'X-Bug-Bounty', spellcheck: false, value: st.kind !== 'replace' ? seed.pattern || '' : '' });
  const value = h('input', { type: 'text', class: 'mono', placeholder: 'your-handle', spellcheck: false, value: st.kind !== 'replace' ? seed.replace || '' : '' });
  const overwrite = h('input', { type: 'checkbox', checked: seed.kind === 'set_header' });
  const find = h('input', { type: 'text', class: 'mono', placeholder: '"beta":false', spellcheck: false, value: st.kind === 'replace' ? seed.pattern || '' : '' });
  const repl = h('input', { type: 'text', class: 'mono', placeholder: '"beta":true  (empty removes it)', spellcheck: false, value: st.kind === 'replace' ? seed.replace || '' : '' });
  const regex = h('input', { type: 'checkbox', checked: !!seed.regex });
  const target = h('select', null, REPLACE_TARGETS.map(([k, l]) => h('option', { value: k, text: l, selected: k === st.target })));
  const when = h('input', { type: 'text', class: 'mono', placeholder: 'e.g. host:api.example.com method:POST', spellcheck: false, value: seed.when || '' });
  const note = h('input', { type: 'text', placeholder: 'Note (optional)', value: seed.note || '' });
  const kinds = h('div', { class: 'rlkinds' });
  const sideSeg = h('span', { class: 'seg rlside' });
  const reachBox = h('div', { class: 'rlreachpick' });
  const fields = h('div', { class: 'rlfields' });
  const preview = h('div', { class: 'rlpreview mono' });

  const drawPreview = () => {
    const who = st.side === 'response' ? 'Every response will' : 'Every request will';
    const n = name.value.trim() || 'Header';
    const lines = {
      add_header: [[`${who} carry`], ['add', `+ ${n}: ${value.value.trim()}`], overwrite.checked ? [`replacing any ${n} already there`] : [`unless it already has ${n}`]],
      change_header: [[`Where ${n} is sent, it becomes`], ['chg', `${n}: ${value.value.trim()}`]],
      remove_header: [[`${who} lose`], ['del', `− ${n}`]],
      replace: [[`In ${(REPLACE_TARGETS.find(([k]) => k === target.value) || ['', ''])[1].toLowerCase()}`], ['del', '− ' + (find.value || '…')], ['add', '+ ' + (repl.value || '(nothing)')]],
    }[st.kind];
    if (when.value.trim()) lines.push(['only when ' + when.value.trim()]);
    clear(preview, lines.map(([a, b]) => h('div', { class: b ? 'p-' + a : '', text: b || a })));
  };
  const drawKinds = () =>
    clear(
      kinds,
      RULE_KINDS.map(([k, label, hint]) =>
        h('button', { type: 'button', class: 'rlkindbtn' + (st.kind === k ? ' on' : ''), title: hint, onclick: () => ((st.kind = k), draw()) }, h('b', { text: label }), h('span', { text: hint })),
      ),
    );
  const drawSide = () =>
    clear(
      sideSeg,
      ['request', 'response'].map((s) => h('button', { type: 'button', class: st.side === s ? 'on' : '', text: s === 'request' ? 'Requests' : 'Responses', onclick: () => ((st.side = s), draw()) })),
    );
  const chip = (key, label, hint) =>
    h('button', { type: 'button', class: 'rlpick' + (st[key] ? ' on' : ''), title: hint, text: label, onclick: () => ((st[key] = !st[key]), drawReach(), drawPreview()) });
  const drawReach = () =>
    clear(
      reachBox,
      chip('browser', 'Browser', 'Traffic from your browser, through the proxy'),
      chip('bench', 'Bench', 'Requests you send from the Bench, Run and Access check'),
      chip('scans', 'Scans', 'Requests sent by Scans, crawls and extensions'),
      chip('scoped', 'In-scope hosts only', 'Leave hosts outside your scope alone'),
    );
  const draw = () => {
    drawKinds();
    drawSide();
    const header = st.kind !== 'replace';
    clear(
      fields,
      header ? h('div', { class: 'rlsiderow' }, h('span', { class: 'rlhint', text: 'Change' }), sideSeg) : null,
      header ? field('Header name', name) : null,
      st.kind === 'add_header' || st.kind === 'change_header' ? field('Value', value) : null,
      st.kind === 'add_header' ? h('label', { class: 'domrow' }, overwrite, 'If it’s already there, replace its value') : null,
      header ? null : field('In', target),
      header ? null : field('Find', find),
      header ? null : field('Replace with', repl),
      header ? null : h('label', { class: 'domrow' }, regex, 'Regular expression ($1 puts back what a group matched)'),
    );
    drawPreview();
  };
  for (const el of [name, value, find, repl, when]) el.addEventListener('input', drawPreview);
  for (const el of [overwrite, target]) el.addEventListener('change', drawPreview);
  drawReach();
  draw();

  const save = async () => {
    const header = st.kind !== 'replace';
    const body = {
      kind: header ? (st.kind === 'add_header' && overwrite.checked ? 'set_header' : st.kind) : 'replace',
      target: header ? st.side + '_header' : target.value,
      match: header ? name.value.trim() : find.value,
      replace: header ? (st.kind === 'remove_header' ? '' : value.value.trim()) : repl.value,
      regex: header ? false : regex.checked,
      browser: st.browser,
      bench: st.bench,
      scans: st.scans,
      in_scope_only: st.scoped,
      when: when.value.trim(),
      note: note.value,
    };
    if (!body.match) return (header ? name : find).focus();
    try {
      if (editing) await api('/api/replace/' + seed.id, { method: 'PATCH', body });
      else await api('/api/replace', { method: 'POST', body });
    } catch (e) {
      m.err.textContent = e.message;
      return;
    }
    closeModal();
    toast(editing ? 'Rule saved.' : 'Rule added. It applies to traffic from now on.', 'ok');
    await loadRules();
    if (S.view === 'rules') drawRules();
  };
  const m = modal(
    editing ? 'Edit rule' : 'New rule',
    [
      kinds,
      fields,
      h('div', { class: 'rlsection' }, h('span', { class: 'rlhint', text: 'Applies to' }), reachBox),
      field('Only when (optional)', when, 'A traffic search, the same as in Traffic: host:, path:, method:, status:, mime:, or any text.'),
      note,
      preview,
    ],
    [h('button', { class: 'btn', text: 'Cancel', onclick: closeModal }), h('button', { class: 'btn primary', text: editing ? 'Save' : 'Add rule', onclick: save })],
  );
  m.el.querySelector('.mcard').classList.add('rldialog');
  m.el.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') closeModal();
    if (e.key === 'Enter' && e.target.tagName === 'INPUT' && e.target.type === 'text') save();
  });
  (st.kind === 'replace' ? find : name).focus();
}

/** The "3 rules on" pill in the Traffic toolbar: always shows when rules change traffic. */
function drawRulesPill() {
  const slot = $('#rulespill');
  if (!slot) return;
  const d = RL.data;
  if (!d || !d.rules.length) return clear(slot);
  const n = activeRules().length;
  const on = d.enabled && n > 0;
  clear(
    slot,
    h(
      'button',
      {
        class: 'rlpill' + (on ? ' on' : ''),
        title: on ? 'Rules are changing traffic. Click to see them or pause them' : 'Rules are paused or all off',
        onclick: (e) => rulesMenu(e.currentTarget),
      },
      h('span', { class: 'dot' }),
      on ? `${n} rule${n === 1 ? '' : 's'} on` : 'Rules paused',
    ),
  );
}

function rulesMenu(anchor) {
  closePopover();
  const d = RL.data;
  const items = activeRules().map((r) => {
    const s = ruleSummary(r);
    return h('button', { role: 'menuitem', class: 'rlmenuitem', onclick: () => (closePopover(), ruleDialog(r)) }, h('span', { class: 'rlkind ' + s.cls, text: s.tag }), h('span', { class: 'mono', text: s.text }));
  });
  const menu = h(
    'div',
    { class: 'ctxmenu', role: 'menu' },
    items.length ? items : h('div', { class: 'mnote', text: d.enabled ? 'Every rule is switched off.' : 'All rules are paused.' }),
    h('div', { class: 'msep' }),
    h('button', { role: 'menuitem', text: d.enabled ? 'Pause all rules' : 'Resume rules', onclick: () => (closePopover(), setRulesOn(!d.enabled)) }),
    h('button', { role: 'menuitem', text: 'Open Rules', onclick: () => (closePopover(), leaveTo('rules')) }),
  );
  showPopover(menu, anchor.getBoundingClientRect());
}

/** Right-click on a header line in the Lens: make a rule from it, filled in. */
function lensHeaderMenu(e, pre, side) {
  const pos = document.caretRangeFromPoint ? document.caretRangeFromPoint(e.clientX, e.clientY) : null;
  if (!pos || !pre.contains(pos.startContainer)) return;
  const upto = document.createRange();
  upto.setStart(pre, 0);
  upto.setEnd(pos.startContainer, pos.startOffset);
  const text = pre.textContent;
  const at = upto.toString().length;
  const start = text.lastIndexOf('\n', at - 1) + 1;
  const end = text.indexOf('\n', at);
  const line = text.slice(start, end < 0 ? text.length : end);
  // Header lines only: not the first line, and before the blank line that starts the body.
  const head = text.slice(0, text.indexOf('\n\n') < 0 ? text.length : text.indexOf('\n\n'));
  if (start === 0 || start > head.length) return;
  const c = line.indexOf(':');
  if (c < 1) return;
  const name = line.slice(0, c).trim();
  const value = line.slice(c + 1).trim();
  if (!/^[!#$%&'*+.^_`|~0-9A-Za-z-]+$/.test(name)) return;
  e.preventDefault();
  closePopover();
  const target = side + '_header';
  const every = side === 'response' ? 'every response' : 'every request';
  const item = (label, hint, seed) =>
    h('button', { role: 'menuitem', class: 'rlctx', onclick: () => (closePopover(), ruleDialog({ target, pattern: name, replace: value, ...seed })) }, h('b', { text: label }), h('span', { text: hint }));
  const menu = h(
    'div',
    { class: 'ctxmenu', role: 'menu' },
    h('div', { class: 'mnote rlctxhead' }, h('b', { text: 'Create a rule from ' + name }), h('span', { text: 'You review it before it’s saved. It then applies to new traffic.' })),
    item('New rule: send on ' + every, `Adds ${name}: ${value.length > 24 ? value.slice(0, 24) + '…' : value} where it’s missing`, { kind: 'add_header' }),
    item('New rule: always use this value', `Gives ${name} this value on ${every}`, { kind: 'set_header' }),
    item('New rule: remove from ' + every, `Takes ${name} out before it ${side === 'response' ? 'reaches the browser' : 'leaves'}`, { kind: 'remove_header' }),
    h('div', { class: 'msep' }),
    h('button', { role: 'menuitem', text: 'Copy header', onclick: () => (closePopover(), copyText(line)) }),
  );
  showPopover(menu, { left: e.clientX, bottom: e.clientY - 6 });
}

/** Request as sent, the original before rules changed it, or both side by side with the changed lines marked. */
function ruleCompare(ex, view) {
  // The record keeps the original in HTTP/1.1 form; compare like with like.
  const sent = asPlain({ ...requestText(ex), lines: [`${ex.method} ${target(ex)} HTTP/1.1`, ...requestText(ex).lines.slice(1)] }).split('\n');
  const orig = (ex.original_request || '').replace(/\n$/, '').split('\n');
  const inSent = new Set(sent);
  const inOrig = new Set(orig);
  const pre = (lines, other, cls) => h('pre', { class: 'raw rlcmp' }, lines.map((l) => [other.has(l) || !l ? l : h('span', { class: cls, text: l }), '\n']));
  if (view === 'original') return pre(orig, inSent, 'd-del');
  return h(
    'div',
    { class: 'rlcmpgrid' },
    h('div', null, h('div', { class: 'rlhint', text: 'Original, before the rules' }), pre(orig, inSent, 'd-del')),
    h('div', null, h('div', { class: 'rlhint', text: 'As sent' }), pre(sent, inOrig, 'd-add')),
  );
}

/** In Settings, the switch stays; the rules themselves live on the Rules screen. */
function replacePanel() {
  return h(
    'div',
    { class: 'spanel replace' },
    h('p', { class: 'muted', text: 'Rules are kept on their own screen, where you can add, edit and switch them on or off one by one.' }),
    h('button', { class: 'btn', text: 'Open Rules', onclick: () => leaveTo('rules') }),
  );
}

/** Client certificates: list, remove, add from PEM or .p12 files. Keys never come back from the engine. */
function clientCertPanel() {
  const panel = h('div', { class: 'spanel replace certs' }, h('h4', { text: 'Certificates' }), h('p', { text: 'Loading…' }));
  const row = (c) => {
    const until = c.not_after ? new Date(c.not_after).toISOString().slice(0, 10) : '';
    return h(
      'div',
      { class: 'rrule' + (c.problem || c.expired ? ' off' : '') },
      h('span', { class: 'rtarget mono', text: c.host }),
      h('span', { text: c.subject || 'certificate #' + c.id, title: 'Issued by ' + (c.issuer || 'unknown') + '\nSHA-256 ' + c.fingerprint }),
      until ? h('span', { class: 'muted', text: (c.expired ? 'expired ' : 'until ') + until }) : null,
      c.chain > 1 ? h('span', { class: 'tag', text: c.chain + ' in chain' }) : null,
      c.problem ? h('span', { class: 'tag rej', text: 'cannot be used', title: c.problem }) : null,
      c.note ? h('span', { class: 'muted', text: c.note }) : null,
      h('button', {
        class: 'iconbtn',
        text: '✕',
        title: 'Remove this certificate',
        onclick: async () => {
          try {
            await api('/api/client-certs/' + c.id, { method: 'DELETE' });
            load();
          } catch (e) {
            toast(e.message, 'err');
          }
        },
      }),
    );
  };
  const fileText = (input, binary) =>
    new Promise((resolve, reject) => {
      const f = input.files && input.files[0];
      if (!f) return resolve(null);
      const r = new FileReader();
      r.onload = () => resolve(binary ? r.result.split(',')[1] || '' : r.result);
      r.onerror = () => reject(new Error('Could not read ' + f.name));
      if (binary) r.readAsDataURL(f);
      else r.readAsText(f);
    });
  const form = () => {
    const host = h('input', { type: 'text', placeholder: 'api.example.com or *.example.com', spellcheck: false });
    const cert = h('input', { type: 'file', accept: '.pem,.crt,.cer,.key,.p12,.pfx' });
    const key = h('input', { type: 'file', accept: '.pem,.key' });
    const password = h('input', { type: 'password', placeholder: '.p12 password', autocomplete: 'off' });
    const note = h('input', { type: 'text', placeholder: 'Note (optional)' });
    const keyRow = h('label', null, 'Key ', key);
    const passRow = h('label', { hidden: true }, password);
    cert.addEventListener('change', () => {
      const p12 = /\.(p12|pfx)$/i.test((cert.files[0] || {}).name || '');
      keyRow.hidden = p12;
      passRow.hidden = !p12;
    });
    const add = async () => {
      if (!host.value.trim()) return host.focus();
      if (!cert.files.length) return toast('Choose the certificate file (.pem, or .p12 with its key inside).', 'err');
      try {
        const p12 = !passRow.hidden;
        const body = { host: host.value.trim(), note: note.value };
        if (p12) {
          body.pkcs12_base64 = await fileText(cert, true);
          body.password = password.value;
        } else {
          body.cert_pem = await fileText(cert, false);
          const k = await fileText(key, false);
          if (k) body.key_pem = k;
        }
        const c = await api('/api/client-certs', { method: 'POST', body });
        toast(`Added. Plonix presents ${c.subject || 'it'} when ${c.host} asks for a certificate.`, 'ok');
        load();
      } catch (e) {
        toast(e.message, 'err');
      }
    };
    return h(
      'div',
      { class: 'rform' },
      h('div', { class: 'row' }, host),
      h('div', { class: 'row' }, h('label', null, 'Certificate ', cert), keyRow, passRow),
      h('div', { class: 'row' }, note, h('button', { class: 'btn primary', text: 'Add Certificate', onclick: add })),
      h('p', { class: 'muted', text: 'PEM: a certificate (chain) and an unencrypted key, in one file or two. PKCS#12: one .p12 or .pfx file and its password. The key is kept in this project and never shown again.' }),
    );
  };
  const load = () =>
    api('/api/client-certs')
      .then((v) => {
        const rows = [h('h4', { text: 'Certificates' })];
        if (!v.enabled) rows.push(h('p', { class: 'muted', text: 'Client certificates are switched off above; none is presented until it is on.' }));
        if (!v.certs.length) rows.push(h('p', { class: 'muted', text: 'No certificates yet.' }));
        rows.push(v.certs.map(row), form());
        clear(panel, rows);
      })
      .catch((e) => clear(panel, h('p', { text: e.message })));
  load();
  return panel;
}

function confirmPrune(st) {
  const m = modal(
    'Delete out-of-scope traffic?',
    [
      h('p', { text: `This permanently deletes ${st.out_of_scope} request(s) to hosts that are not in scope, then compacts the project file. Requests that findings point to are kept.` }),
      h('p', { class: 'muted', text: 'Scope suggestions that relied on that traffic go away too.' }),
    ],
    [
      h('button', { class: 'btn', text: 'Cancel', onclick: closeModal }),
      h('button', {
        class: 'btn primary',
        text: 'Delete',
        onclick: async () => {
          try {
            const r = await api('/api/storage/prune', { method: 'POST', body: { confirm: true } });
            closeModal();
            toast(r.skipped ? r.skipped : `Deleted ${r.removed} request(s). ${r.kept} kept.`, 'ok');
            exCache.clear();
            go('settings', true);
          } catch (e) {
            m.err.textContent = e.message;
          }
        },
      }),
    ],
  );
}
