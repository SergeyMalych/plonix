// Plonix window: Findings.
// Part of the project window's scripts (see index.html for the load order).
'use strict';

/* ======================================================================
   Findings
   ====================================================================== */

function renderFindings(main) {
  const exportBtn = h('button', { class: 'btn sm', text: 'Export ▾', title: 'Save the findings as a report, with their evidence requests', onclick: () => exportMenu(exportBtn) });
  clear(
    main,
    h(
      'div',
      { class: 'view' },
      h('div', { class: 'toolbar' }, h('h2', { text: 'Findings' }), h('span', { class: 'hint', text: 'Reproducible issues, each tied to the requests that prove it.' }), exportBtn, h('button', { class: 'btn primary sm', text: 'New finding', onclick: () => newFinding([], '') })),
      h('div', { class: 'traffic' }, h('div', { class: 'pane' }, h('div', { class: 'stack', id: 'findbody' })), h('div', { id: 'inspslot' })),
    ),
  );
  loadFindings();
}

const SEV_ORDER = { critical: 0, high: 1, medium: 2, low: 3, info: 4 };
const SEVERITIES = ['info', 'low', 'medium', 'high', 'critical'];
const FINDING_STATUSES = { open: 'Open', confirmed: 'Confirmed', false_positive: 'False positive', fixed: 'Fixed' };

async function loadFindings() {
  let list;
  try {
    list = await api('/api/findings');
  } catch (e) {
    return toast(e.message, 'err');
  }
  S.findingsCount = list.length;
  updateChrome();
  const box = $('#findbody');
  if (!box) return;
  if (!list.length) {
    return clear(
      box,
      h('div', { class: 'card' }, h('div', { class: 'empty' }, h('h3', { text: 'No findings yet' }), 'Open a request in Traffic or on the Bench and choose ', h('b', { text: 'New finding' }), ' to record what you found with the request as evidence.')),
    );
  }
  // Closed findings (false positives, fixed) go below the ones still to deal with.
  const closed = (f) => (f.status === 'false_positive' || f.status === 'fixed' ? 1 : 0);
  list.sort((a, b) => closed(a) - closed(b) || (SEV_ORDER[a.severity] ?? 9) - (SEV_ORDER[b.severity] ?? 9) || b.created_at - a.created_at);
  clear(box, list.map(findingCard));
}

function findingCard(f) {
  const status = h(
    'select',
    { class: 'fstatus', 'aria-label': 'Status', title: 'Where this finding stands', onchange: () => setFindingStatus(f, status) },
    Object.entries(FINDING_STATUSES).map(([v, label]) => h('option', { value: v, text: label, selected: v === f.status })),
  );
  const edited = f.updated_at > f.created_at ? ` · edited ${fmtDate(f.updated_at)}` : '';
  return h(
    'div',
    { class: 'card finding' + (f.status === 'false_positive' || f.status === 'fixed' ? ' closed' : '') },
    h(
      'div',
      { class: 'fh' },
      h('span', { class: 'sev ' + f.severity, text: f.severity }),
      h('span', { class: 'ft', text: f.title }),
      h('span', { class: 'fmeta', text: `#${f.id} · by ${f.created_by} · ${fmtDate(f.created_at)}${edited}` }),
      status,
      askButton({ kind: 'finding', id: f.id }),
      h('button', { class: 'btn sm', text: 'Edit', onclick: () => findingForm(f) }),
      h('button', { class: 'btn sm danger', text: 'Delete…', onclick: () => confirmDeleteFinding(f) }),
    ),
    f.description || f.exchange_ids.length
      ? h(
          'div',
          { class: 'fb' },
          f.description || null,
          f.exchange_ids.length ? h('div', { class: 'evid' }, f.exchange_ids.map((id) => h('button', { text: 'request #' + id, title: 'Open in the Lens', onclick: () => showExchange(id) }))) : null,
        )
      : null,
  );
}

async function setFindingStatus(f, select) {
  try {
    await api(`/api/findings/${f.id}`, { method: 'PATCH', body: { status: select.value } });
    toast(`Finding #${f.id}: ${FINDING_STATUSES[select.value]}`, 'ok');
    loadFindings();
  } catch (e) {
    select.value = f.status;
    toast(e.message, 'err');
  }
}

function confirmDeleteFinding(f) {
  const m = modal(
    'Delete this finding?',
    [h('p', { text: `#${f.id} ${f.title}` }), h('p', { class: 'muted', text: 'The finding is deleted for good. The requests it points to stay in Traffic. To keep it on record instead, set its status to False positive or Fixed.' })],
    [
      h('button', { class: 'btn', text: 'Cancel', onclick: closeModal }),
      h('button', {
        class: 'btn primary',
        text: 'Delete',
        onclick: async () => {
          try {
            await api(`/api/findings/${f.id}`, { method: 'DELETE' });
            closeModal();
            toast(`Finding #${f.id} deleted`, 'ok');
            loadFindings();
          } catch (e) {
            m.err.textContent = e.message;
          }
        },
      }),
    ],
  );
}

/** The findings report, as the engine writes it (false positives left out). */
async function fetchReport(format) {
  let resp;
  try {
    resp = await fetch('/api/findings/export?format=' + format, { headers: { Authorization: 'Bearer ' + S.token, 'X-Plonix-Client': 'gui' }, cache: 'no-store' });
  } catch (_) {
    throw new ApiError(0, 'engine_down', 'The Plonix engine is not reachable.');
  }
  if (!resp.ok) {
    const data = await resp.json().catch(() => null);
    throw new ApiError(resp.status, (data && data.code) || 'error', (data && data.error) || resp.statusText, data);
  }
  const name = ((resp.headers.get('content-disposition') || '').match(/filename="([^"]+)"/) || [])[1] || 'plonix-findings.' + format;
  return { name, blob: await resp.blob() };
}

function exportMenu(anchor) {
  closePopover();
  const save = async (format) => {
    closePopover();
    try {
      const { name, blob } = await fetchReport(format);
      const url = URL.createObjectURL(blob);
      const a = h('a', { href: url, download: name, hidden: true });
      document.body.append(a);
      a.click();
      a.remove();
      setTimeout(() => URL.revokeObjectURL(url), 10000);
      toast(`Exported ${name}`, 'ok');
    } catch (e) {
      toast(e.message, 'err');
    }
  };
  const copy = async () => {
    closePopover();
    try {
      copyText(await (await fetchReport('md')).blob.text());
    } catch (e) {
      toast(e.message, 'err');
    }
  };
  const item = (label, run) => h('button', { role: 'menuitem', text: label, onclick: run });
  const menu = h(
    'div',
    { class: 'ctxmenu', role: 'menu' },
    item('Markdown (.md)', () => save('md')),
    item('HTML page (.html)', () => save('html')),
    item('JSON (.json)', () => save('json')),
    h('div', { class: 'msep' }),
    item('Copy as Markdown', copy),
    h('div', { class: 'msep' }),
    h('div', { class: 'mnote', text: 'False positives are left out. Choose findings with plonix findings export.' }),
  );
  showPopover(menu, anchor.getBoundingClientRect());
}

function newFinding(ids, title) {
  findingForm(null, ids, title);
}

/** Records a new finding, or edits `f`. */
function findingForm(f, ids = [], title = '', hint = null) {
  const t = h('input', { value: f ? f.title : title || '', placeholder: 'e.g. IDOR on /v2/orders/{id} exposes other users’ addresses' });
  const sev = h('select', null, SEVERITIES.map((s) => h('option', { value: s, text: s, selected: s === (f ? f.severity : (hint && hint.severity) || 'medium') })));
  const desc = h('textarea', { placeholder: 'What happens, how to reproduce it, and why it matters.', value: f ? f.description : (hint && hint.description) || '' });
  const ex = f ? null : h('input', { value: ids.join(', '), placeholder: 'Request ids, e.g. 14, 22', oninput: () => !job.busy && writeIdle() });
  // Claude can write the title, severity and description from the evidence.
  const job = {};
  const writeNote = h('span', { class: 'muted fine' });
  const writeBtn = h('button', { class: 'btn sm askbtn', type: 'button' }, h('span', { class: 'askico', text: '✦' }), ' Write it with Claude');
  const evidence = () => (f ? f.exchange_ids : ex.value.split(/[\s,]+/).filter(Boolean).map((x) => Number(x.replace('#', '')))).filter((n) => Number.isInteger(n) && n > 0);
  const writeIdle = () => {
    writeBtn.disabled = false;
    writeBtn.lastChild.textContent = ' Write it with Claude';
    const n = evidence();
    writeNote.textContent = n.length ? `Shares request #${n[0]} and its response with Claude Code on ${THIS_COMPUTER}.` : 'Add an evidence request first.';
  };
  writeBtn.onclick = async () => {
    if (job.busy) {
      job.stop = true;
      if (job.run) api(`/api/agents/run/${job.run}`, { method: 'DELETE' }).catch(() => {});
      job.busy = false;
      return writeIdle();
    }
    const n = evidence();
    if (!n.length) return (m.err.textContent = 'Add the request that shows the issue as evidence first.');
    Object.assign(job, { busy: true, stop: false, run: null });
    m.err.textContent = '';
    writeBtn.lastChild.textContent = ' Stop';
    writeNote.textContent = 'Claude is writing the finding…';
    try {
      const out = await writeFindingWithClaude(n, hint && hint.note, (msg) => (writeNote.textContent = msg), job);
      if (!out || job.stop || !m.el.isConnected) return;
      t.value = out.title;
      if (out.severity) sev.value = out.severity;
      desc.value = out.description;
      writeNote.textContent = 'Written by Claude. Check it, edit anything, then save.';
    } catch (e) {
      if (!job.stop) m.err.textContent = e.message;
      writeIdle();
    } finally {
      job.busy = false;
      writeBtn.disabled = false;
      writeBtn.lastChild.textContent = ' Write it with Claude';
    }
  };
  writeIdle();
  const save = async () => {
    if (!t.value.trim()) return (m.err.textContent = 'Give the finding a title.');
    if (f) {
      try {
        await api(`/api/findings/${f.id}`, { method: 'PATCH', body: { title: t.value.trim(), severity: sev.value, description: desc.value } });
        closeModal();
        toast(`Finding #${f.id} saved`, 'ok');
        if (S.view === 'findings') loadFindings();
      } catch (e) {
        m.err.textContent = e.message;
      }
      return;
    }
    const exchange_ids = ex.value
      .split(/[\s,]+/)
      .filter(Boolean)
      .map((x) => Number(x.replace('#', '')));
    if (exchange_ids.some((n) => !Number.isInteger(n))) return (m.err.textContent = 'Request ids must be numbers.');
    try {
      const created = await api('/api/findings', { method: 'POST', body: { title: t.value.trim(), severity: sev.value, description: desc.value, exchange_ids } });
      closeModal();
      toast(`Finding #${created.id} recorded`, 'ok');
      S.findingsCount = (S.findingsCount || 0) + 1;
      updateChrome();
      if (S.view === 'findings') loadFindings();
    } catch (e) {
      m.err.textContent = e.message;
    }
  };
  const m = modal(
    f ? `Edit finding #${f.id}` : 'New finding',
    [
      hint ? h('div', { class: 'fhint', text: 'Plonix noticed: ' + hint.note }) : null,
      agentsOn() ? h('div', { class: 'fwrite' }, writeBtn, writeNote) : null,
      h('label', null, 'Title', t),
      h('label', null, 'Severity', sev),
      h('label', null, 'Description', desc),
      ex ? h('label', null, 'Evidence (request ids)', ex) : null,
    ],
    [h('button', { class: 'btn', text: 'Cancel', onclick: closeModal }), h('button', { class: 'btn primary', text: f ? 'Save' : 'Save finding', onclick: save })],
  );
  m.el.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) save();
  });
}
