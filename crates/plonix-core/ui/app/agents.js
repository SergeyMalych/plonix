// Plonix window: Agents: ask Claude, conversations, the inbox and activity.
// Part of the project window's scripts (see index.html for the load order).
'use strict';

/* ======================================================================
   Agents: ask Claude about the project, pick up past conversations, and
   follow what agents read. Setup lives behind the Setup button.
   ====================================================================== */

// An agent counts as connected while its MCP server checks in (every 20 s).
const AGENT_LIVE_MS = 60000;
// Requests one agent makes this close together read as one burst in the feed.
const AGENT_BURST_MS = 120000;

const EXAMPLE_PROMPTS = [
  'Use Plonix to find in-scope API endpoints that returned errors, then read the most interesting request and tell me what stands out.',
  'Using Plonix, list every endpoint on the target that takes an id parameter and group them by host.',
  'Look at Plonix scope suggestions and explain which ones really belong to the target and why.',
  'Summarize my Plonix findings and point to the requests that prove each one.',
];

// Questions offered under the ask box when nothing has been typed yet.
const STARTER_QUESTIONS = [
  'What should I look at next in this project?',
  'Which endpoints take an id, and which look worth checking for access control?',
  'Summarize what this application does from its traffic.',
];

const AG = { chat: null, cv: null, setup: null, chats: [], policy: null, hits: [], skills: [], watch: null, inboxAll: false, lookAsked: 0 };

function renderAgents(main) {
  AG.cv = null;
  const setupBtn = h('button', { class: 'btn sm', id: 'agsetupbtn', text: 'Setup', onclick: () => toggleAgentSetup() });
  clear(
    main,
    h(
      'div',
      { class: 'view' },
      h('div', { class: 'toolbar' }, h('h2', { text: 'Agents' }), h('span', { class: 'hint', text: 'Ask Claude about this project, pick up past conversations, and see what agents looked at.' }), setupBtn),
      h(
        'div',
        { class: 'pane' },
        h(
          'div',
          { class: 'agws' },
          h('div', { class: 'agmain' }, h('div', { class: 'card aginbox', id: 'aginbox' }), h('div', { class: 'card agask', id: 'agask' }), h('div', { id: 'agbody' })),
          h('aside', { class: 'agside' }, h('div', { class: 'card agfeed', id: 'agfeed' }, h('div', { class: 'ab muted', text: 'Loading…' }))),
        ),
        h('div', { class: 'stack agsetup', id: 'agentsbody', hidden: true }),
      ),
    ),
  );
  drawAgentAsk();
  drawAgentBody();
  loadAgents(true);
}

function toggleAgentSetup(open) {
  AG.setup = open === undefined ? !AG.setup : open;
  const box = $('#agentsbody');
  const btn = $('#agsetupbtn');
  if (!box) return;
  box.hidden = !AG.setup;
  if (btn) btn.classList.toggle('on', !!AG.setup);
  if (AG.setup) {
    drawAgentSetup();
    box.scrollIntoView({ block: 'start', behavior: 'smooth' });
  }
}

/** Refreshes what changes on its own: who is connected, the feed, the conversations. */
async function loadAgents(first) {
  S.agentsAt = Date.now();
  let a, act, list, watch;
  try {
    [a, act, list, watch] = await Promise.all([api('/api/agents'), api('/api/agents/activity?limit=200'), api('/api/agents/chats'), api('/api/agents/watch')]);
  } catch (e) {
    return toast(e.message, 'err');
  }
  if (S.view !== 'agents') return;
  AG.policy = a;
  AG.hits = act.hits || [];
  AG.chats = list.chats || [];
  AG.watch = watch;
  if (first) {
    // Nothing has ever happened here: show how to get started.
    if (AG.setup === null) AG.setup = !AG.chats.length && !(a.clients || []).length && a.ask_in_app === false;
    toggleAgentSetup(AG.setup);
    drawAgentAsk();
    loadAgentStarters();
  } else if (AG.setup) drawAgentSetup();
  drawAgentInbox();
  drawAgentFeed();
  if (!AG.chat) drawAgentBody();
}

/* ---- the ask box ---- */

function drawAgentAsk() {
  const box = $('#agask');
  if (!box) return;
  const a = AG.policy;
  if (a && !a.enabled) {
    return clear(box, h('div', { class: 'ab' }, h('b', { text: 'Agent access is off.' }), ' Turn it on in Settings › AI agents to ask Claude about this project. ', h('button', { class: 'link', text: 'Open Settings', onclick: () => ((S.settingsSection = 'agents'), leaveTo('settings')) })));
  }
  if (a && a.ask_in_app === false) {
    return clear(
      box,
      h(
        'div',
        { class: 'ab' },
        h('b', { text: 'Claude Code is not installed on this Mac.' }),
        ' Install it from claude.com/claude-code to ask about this project right here. Other agents can still connect: ',
        h('button', { class: 'link', text: 'see Setup', onclick: () => toggleAgentSetup(true) }),
        '.',
      ),
    );
  }
  const q = h('textarea', { class: 'agq', id: 'agq', rows: 2, placeholder: 'Ask Claude about this project: what to look at next, what an endpoint does, which hosts belong to the target…' });
  const go = h('button', { class: 'btn primary ai', text: '✦ Ask', disabled: true });
  const send = () => {
    const t = q.value.trim();
    if (!t) return;
    q.value = '';
    go.disabled = true;
    startAgentChat(t, t);
  };
  q.addEventListener('input', () => (go.disabled = !q.value.trim()));
  q.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      send();
    }
  });
  go.onclick = send;
  clear(
    box,
    h('div', { class: 'agqrow' }, q, go),
    h('div', { class: 'agstarters', id: 'agstarters' }),
    h('div', { id: 'agarg' }),
    h('p', { class: 'muted fine agnote', text: 'Claude reads this project through Plonix, read-only. It can suggest; it cannot send requests or change anything.' }),
  );
  drawAgentStarters();
}

async function loadAgentStarters() {
  try {
    AG.skills = ((await api('/api/skills')).skills || []).filter((s) => s.available);
  } catch (_) {
    AG.skills = [];
  }
  drawAgentStarters();
}

/** One-click starts: skills (playbooks) and a few plain questions. */
function drawAgentStarters() {
  const box = $('#agstarters');
  if (!box) return;
  clear(
    box,
    AG.skills.map((sk) => h('button', { class: 'chip k-ai', title: sk.description, onclick: () => runSkill(sk) }, h('span', { text: '✦ ' + sk.title }))),
    STARTER_QUESTIONS.map((t) => h('button', { class: 'chip', title: 'Ask this', onclick: () => startAgentChat(t, t) }, h('span', { text: t }))),
  );
}

/** Runs a skill; one that needs something (a host, a request id) asks for it first. */
async function runSkill(sk, values = {}) {
  const need = (sk.arguments || []).filter((x) => x.required && !values[x.name]);
  const slot = $('#agarg');
  if (need.length) {
    if (!slot) return;
    const hosts = ((S.facets && S.facets.hosts) || []).map((x) => x.value);
    const inputs = need.map((x) => {
      const guess = x.name === 'host' ? (M.sel && hosts.includes(M.sel) ? M.sel : hosts[0] || '') : x.name === 'id' && T.sel ? String(T.sel) : '';
      return { x, el: h('input', { value: guess, placeholder: x.description, list: x.name === 'host' ? 'aghosts' : null, spellcheck: 'false' }) };
    });
    const ok = () => {
      const v = { ...values };
      for (const { x, el } of inputs) v[x.name] = el.value.trim();
      if (inputs.some(({ x }) => !v[x.name])) return;
      clear(slot);
      runSkill(sk, v);
    };
    for (const { el } of inputs) el.addEventListener('keydown', (e) => e.key === 'Enter' && ok());
    clear(
      slot,
      h(
        'div',
        { class: 'agargrow' },
        h('b', { text: sk.title }),
        inputs.map(({ x, el }) => h('label', null, h('span', { text: x.name }), el)),
        h('datalist', { id: 'aghosts' }, hosts.map((v) => h('option', { value: v }))),
        h('button', { class: 'btn primary sm', text: 'Run', onclick: ok }),
        h('button', { class: 'btn sm ghost', text: 'Cancel', onclick: () => clear(slot) }),
      ),
    );
    inputs[0].el.focus();
    return;
  }
  let r;
  try {
    r = await api('/api/skills/' + encodeURIComponent(sk.name) + '?' + new URLSearchParams(values));
  } catch (e) {
    return toast(e.message, 'err');
  }
  if (!r.prompt) return toast(r.needs || 'This skill needs more to go on.', 'err');
  const detail = Object.values(values).join(', ');
  startAgentChat(r.prompt, sk.title + (detail ? ': ' + detail : ''));
}

/* ---- the inbox: what the background watcher found ---- */

const INBOX_SHOW = 8;
const ITEM_KIND = { lead: { label: 'Lead', cls: 'lead' }, note: { label: 'Note', cls: 'note' }, digest: { label: 'New', cls: 'digest' } };
const NEXT_ACTION = {
  lens: 'Open request',
  bench: 'Open in Bench',
  scan: 'Scan this host',
  access: 'Check as other users',
  finding: 'Write it up',
  map: 'Open in Map',
  scope: 'Open Scope',
};

/** Acts on an inbox item where it points: the request, the Bench, Scans, Access or a new finding. */
async function actOnItem(it) {
  markItems([it.id], false);
  const id = it.request;
  const next = it.next || 'lens';
  if (next === 'map' || next === 'scope') {
    if (next === 'map' && it.host) M.sel = it.host;
    return leaveTo(next);
  }
  if (!id) return;
  if (next === 'bench') return sendToBench(id);
  if (next === 'finding') return newFinding([id], it.title);
  if (next === 'access' && toolOn('access-check')) return startAccessCheck({ targets: [id], sourceLabel: it.title });
  if (next === 'scan') {
    try {
      const ex = await api('/api/traffic/' + id);
      SC.host = ex.host;
      return leaveTo('scans');
    } catch (_) {}
  }
  showExchange(id);
}

async function markItems(ids, dismiss) {
  try {
    const r = await api('/api/agents/watch/items', { method: 'POST', body: { ids, dismiss } });
    if (S.status) S.status.agent_inbox_unread = r.unread;
    updateChrome();
  } catch (e) {
    return toast(e.message, 'err');
  }
  if (dismiss || !ids.length) loadAgents();
}

async function saveWatch(patch) {
  try {
    AG.watch = await api('/api/agents/watch', { method: 'PUT', body: { ...AG.watch.settings, ...patch } });
  } catch (e) {
    return toast(e.message, 'err');
  }
  drawAgentInbox();
}

async function lookNow() {
  try {
    await api('/api/agents/watch/look', { method: 'POST' });
  } catch (e) {
    return toast(e.message, 'err');
  }
  AG.lookAsked = Date.now();
  drawAgentInbox();
  toast('Claude will look at your latest traffic in a moment', 'ok');
}

function drawAgentInbox() {
  const box = $('#aginbox');
  const w = AG.watch;
  if (!box || !w) return;
  const a = AG.policy;
  const st = w.settings;
  const items = w.items || [];
  // Asked to look: show it as looking until the look starts and ends (or it gives up).
  const asked = AG.lookAsked && Date.now() - AG.lookAsked < 30000 && !(w.state.last_look_at >= AG.lookAsked);
  const looking = w.running || asked;
  const used = w.state.tokens_today || 0;
  const pct = Math.min(100, Math.round((used / st.daily_tokens) * 100));
  const status = !st.enabled
    ? 'Off'
    : w.capped
      ? 'Paused until tomorrow: today’s tokens are used up'
      : looking
        ? 'Looking at your traffic…'
        : w.waiting
          ? `${w.waiting} new request${w.waiting === 1 ? '' : 's'} waiting; Claude looks when you pause`
          : 'Watching your traffic';
  const toggle = h('input', { type: 'checkbox', checked: st.enabled, onchange: () => saveWatch({ enabled: toggle.checked }) });
  const budget = h(
    'select',
    { title: 'Most tokens Claude may use per day watching this project', onchange: (e) => saveWatch({ daily_tokens: Number(e.target.value) }) },
    w.budgets.map((n) => h('option', { value: n, text: fmtTok(n) + ' a day', selected: n === st.daily_tokens })),
  );
  const head = h(
    'div',
    { class: 'aghead' },
    h('b', { text: 'From Claude' }),
    w.unread ? h('span', { class: 'qn', text: w.unread }) : null,
    h('span', { class: 'agstat' + (looking ? ' busy' : ''), text: status }),
    h('span', { class: 'sp' }),
    w.unread ? h('button', { class: 'btn sm ghost', text: 'Mark all read', onclick: () => markItems([], false) }) : null,
    h('button', { class: 'btn sm', text: looking ? 'Looking…' : 'Look now', disabled: looking || !a || !a.enabled || a.ask_in_app === false, title: 'Have Claude look at the latest in-scope traffic now', onclick: lookNow }),
    h('label', { class: 'agswitch', title: 'Let Claude read new in-scope traffic in the background and leave notes and leads here' }, toggle, h('span', { text: 'Watch my traffic' })),
  );
  let body;
  if (!items.length && !st.enabled) {
    body = h(
      'div',
      { class: 'agempty' },
      h('div', { class: 'agbig', text: 'Let Claude watch while you browse' }),
      h('p', { text: 'When you pause, Claude reads the new in-scope traffic and leaves notes and leads here: what stands out, what to check next, and where. It only reads. Nothing is sent unless you click.' }),
      h('div', { class: 'agrowbtns' }, h('button', { class: 'btn primary', text: 'Turn on', onclick: () => saveWatch({ enabled: true }) }), h('span', { class: 'muted fine', text: 'Up to ' + fmtTok(st.daily_tokens) + ' tokens a day. You can change this or turn it off any time.' })),
    );
  } else if (!items.length) {
    body = h('div', { class: 'ab muted', text: w.state.last_error || 'Nothing yet. Browse the target; when you pause, Claude looks at what is new. Or press Look now.' });
  } else {
    const shown = AG.inboxAll ? items : items.slice(0, INBOX_SHOW);
    body = h(
      'div',
      { class: 'aglist' },
      shown.map((it) => {
        const k = ITEM_KIND[it.kind] || ITEM_KIND.note;
        // Access needs its Market tool; without it the lead opens the request.
        const next = it.next === 'access' && !toolOn('access-check') ? 'lens' : it.next || 'lens';
        const act = it.kind === 'digest' ? null : NEXT_ACTION[next];
        return h(
          'div',
          { class: 'agitem' + (it.read ? '' : ' unread') },
          h('span', { class: 'agkind ' + k.cls, text: k.label }),
          h(
            'div',
            { class: 'agimain' },
            h('div', { class: 'agititle', text: it.title }),
            it.detail ? h('div', { class: 'agidetail', text: it.detail }) : null,
            h(
              'div',
              { class: 'agimeta' },
              it.request ? h('button', { class: 'link', text: 'Request #' + it.request, onclick: () => (markItems([it.id], false), showExchange(it.request)) }) : null,
              h('span', { text: agoText(it.at) }),
            ),
          ),
          h(
            'div',
            { class: 'agiacts' },
            act && (it.request || it.next === 'map' || it.next === 'scope') ? h('button', { class: 'btn sm' + (it.kind === 'lead' ? ' primary' : ''), text: act, onclick: () => actOnItem(it) }) : null,
            !it.read ? h('button', { class: 'mini', text: '✓', title: 'Mark read', onclick: () => markItems([it.id], false) }) : null,
            h('button', { class: 'mini', text: '✕', title: it.kind === 'digest' ? 'Remove' : 'Dismiss: not useful. Claude will suggest fewer like it.', onclick: () => markItems([it.id], true) }),
          ),
        );
      }),
      items.length > INBOX_SHOW
        ? h('button', { class: 'link agmore', text: AG.inboxAll ? 'Show fewer' : `Show all ${items.length}`, onclick: () => ((AG.inboxAll = !AG.inboxAll), drawAgentInbox()) })
        : null,
    );
  }
  clear(
    box,
    head,
    body,
    st.enabled || used
      ? h(
          'div',
          { class: 'agfoot' },
          h('div', { class: 'askmeter' }, h('div', { class: 'bar' + (w.capped ? ' over' : '') }, h('i', { style: { width: pct + '%' } })), h('span', { text: `${fmtTok(used)} of ${fmtTok(st.daily_tokens)} tokens today` + (w.state.looks_today ? ` · ${w.state.looks_today} look${w.state.looks_today === 1 ? '' : 's'}` : '') })),
          h('span', { class: 'sp' }),
          budget,
        )
      : null,
  );
}

/* ---- conversations ---- */

/** Starts a new conversation and opens it. */
function startAgentChat(prompt, ask) {
  AG.chat = 'new';
  const cv = openAgentChat(null, ask);
  cv.start(prompt, ask);
}

/** Shows one conversation in place of the list: a saved one, or a new one (titled `ask`) when `chat` is null. */
function openAgentChat(chat, ask) {
  const body = $('#agbody');
  const stopBtn = h('button', { class: 'btn sm', text: 'Stop', hidden: true });
  const delBtn = h('button', { class: 'btn sm ghost', text: 'Delete', hidden: !chat });
  const title = h('b', { class: 'agctitle', text: chat ? chat.title : (ask || 'New conversation').split('\n')[0] });
  const sub = chat && chat.subject ? h('button', { class: 'chip', title: 'Open it', onclick: () => openSubject(chat.subject) }, h('span', { text: subjectLabel(chat.subject) })) : null;
  const cv = claudeConvo({
    tall: true,
    onRunning: (on) => (stopBtn.hidden = !on),
    onChat: (id) => {
      AG.chat = id;
      delBtn.hidden = false;
    },
    onDone: () => loadAgents(),
  });
  AG.cv = cv;
  stopBtn.onclick = () => cv.stop();
  delBtn.onclick = async () => {
    if (cv.isRunning() || !cv.chat()) return;
    try {
      await api('/api/agents/chats/' + encodeURIComponent(cv.chat()), { method: 'DELETE' });
    } catch (e) {
      return toast(e.message, 'err');
    }
    closeAgentChat();
  };
  clear(
    body,
    h(
      'div',
      { class: 'card agchat' },
      h('div', { class: 'agchead' }, h('button', { class: 'btn sm ghost', text: '← Conversations', onclick: closeAgentChat }), title, sub, h('span', { class: 'sp' }), stopBtn, delBtn),
      h('div', { class: 'agcbody' }, cv.el),
    ),
  );
  if (chat) cv.load(chat);
  return cv;
}

function closeAgentChat() {
  AG.chat = null;
  AG.cv = null;
  drawAgentBody();
  loadAgents();
}

async function showAgentChat(id) {
  let chat;
  try {
    chat = await api('/api/agents/chats/' + encodeURIComponent(id));
  } catch (e) {
    return toast(e.message, 'err');
  }
  if (S.view !== 'agents') return;
  AG.chat = id;
  openAgentChat(chat).focus();
}

/** The conversation list, shown while no conversation is open. */
function drawAgentBody() {
  const body = $('#agbody');
  if (!body || AG.chat) return;
  const del = async (e, c) => {
    e.stopPropagation();
    try {
      await api('/api/agents/chats/' + encodeURIComponent(c.id), { method: 'DELETE' });
    } catch (err) {
      return toast(err.message, 'err');
    }
    loadAgents();
  };
  clear(
    body,
    h('div', { class: 'sechead' }, h('h3', { text: 'Conversations' }), AG.chats.length ? h('span', { class: 'muted fine', text: String(AG.chats.length) }) : null),
    h(
      'div',
      { class: 'card aglist' },
      AG.chats.length
        ? AG.chats.map((c) =>
            h(
              'div',
              { class: 'agrow', role: 'button', tabindex: 0, onclick: () => showAgentChat(c.id), onkeydown: (e) => e.key === 'Enter' && showAgentChat(c.id) },
              h(
                'div',
                { class: 'agrmain' },
                h('div', { class: 'agrtop' }, h('b', { text: c.title }), c.subject ? h('span', { class: 'tag', text: subjectLabel(c.subject) }) : null, c.running ? h('span', { class: 'tag in', text: 'answering' }) : null),
                c.preview ? h('div', { class: 'agrprev', text: c.preview }) : null,
              ),
              h('span', { class: 'agrmeta', text: (c.turns > 1 ? c.turns + ' questions · ' : '') + agoText(c.updated_at) }),
              h('button', { class: 'mini', text: '✕', title: 'Delete this conversation', onclick: (e) => del(e, c) }),
            ),
          )
        : h('div', { class: 'ab muted', text: 'Questions you ask here, or with ✦ Ask Claude anywhere in Plonix, are kept here so you can pick them up again later.' }),
    ),
  );
}

/* ---- the activity feed ---- */

/** What one agent request was, in words, and where it leads in the app. */
function hitInfo(x) {
  const p = x.path;
  const q = new URLSearchParams(x.query || '');
  const host = (m) => decodeURIComponent(m);
  const toMap = (hst) => () => {
    if (hst) M.sel = hst;
    leaveTo('map');
  };
  let m;
  if (p === '/api/traffic') {
    const t = q.get('q') || '';
    return { ico: '⌕', text: t ? 'Searched traffic for ' + t : 'Listed recent traffic', go: () => setQuery(t) };
  }
  if ((m = p.match(/^\/api\/traffic\/(\d+)(?:\/(insights|messages))?$/))) {
    const what = m[2] === 'insights' ? 'Spotted values in request #' : m[2] === 'messages' ? 'WebSocket messages of request #' : 'Request #';
    return { ico: '⇅', text: what + m[1], go: () => showExchange(Number(m[1])) };
  }
  if (p === '/api/hosts') return { ico: '⊞', text: 'Hosts seen', go: toMap() };
  if ((m = p.match(/^\/api\/hosts\/([^/]+)\/endpoints$/))) return { ico: '⊞', text: 'Endpoints on ' + host(m[1]), go: toMap(host(m[1])) };
  if (p === '/api/tech') return { ico: '⊞', text: 'Technologies on every host', go: toMap() };
  if ((m = p.match(/^\/api\/tech\/([^/]+)$/))) return { ico: '⊞', text: 'Technologies on ' + host(m[1]), go: toMap(host(m[1])) };
  if (p === '/api/scope') return { ico: '◉', text: 'Scope and suggested domains', go: () => leaveTo('scope') };
  if (p === '/api/findings') return { ico: '⚑', text: 'Findings', go: () => leaveTo('findings') };
  if (p === '/api/findings/export') return { ico: '⚑', text: 'Findings report', go: () => leaveTo('findings') };
  if ((m = p.match(/^\/api\/findings\/(\d+)$/))) return { ico: '⚑', text: 'Finding #' + m[1], go: () => leaveTo('findings') };
  if (p === '/api/scan/catalog') return { ico: '◎', text: 'Available scan checks', go: () => leaveTo('scans') };
  if ((m = p.match(/^\/api\/scan\/(?:suggest|plan)\/([^/]+)$/))) return { ico: '◎', text: 'Scan plan for ' + host(m[1]), go: () => leaveTo('scans') };
  if (p === '/api/bench/proposals' && x.method === 'POST') return { ico: '⎇', text: 'Suggested an edit on the Bench', go: () => leaveTo('bench') };
  if (p === '/api/status') return { ico: '•', text: 'Engine status' };
  return { ico: '•', text: x.method + ' ' + p };
}

/** Who made a burst of requests: an Ask Claude conversation by its title, else the agent's name. */
function hitWho(client) {
  const [name, run] = client.split('/');
  if (name === 'plonix-watch') return { label: 'Watching your traffic', ask: true };
  if (name !== 'plonix-ask') return { label: name };
  const chat = run && AG.chats.find((c) => (c.runs || []).includes(run));
  return chat ? { label: chat.title, chat: chat.id, ask: true } : { label: 'Ask Claude', ask: true };
}

function drawAgentFeed() {
  const box = $('#agfeed');
  const a = AG.policy;
  if (!box || !a) return;
  const now = Date.now();
  const live = (a.clients || []).filter((c) => now - c.last_seen < AGENT_LIVE_MS && !c.name.startsWith('plonix-'));
  // Consecutive requests from one agent, close together, form one burst.
  const bursts = [];
  for (const x of AG.hits) {
    const b = bursts[bursts.length - 1];
    if (b && b.client === x.client && b.last - x.at < AGENT_BURST_MS) {
      b.hits.push(x);
      b.last = x.at;
    } else bursts.push({ client: x.client, at: x.at, last: x.at, hits: [x] });
  }
  const SHOW = 6;
  clear(
    box,
    h(
      'div',
      { class: 'agfhead' },
      h('span', { class: 'adot' + (live.length ? ' on' : '') }),
      h('b', { text: 'Activity' }),
      h('span', { class: 'muted fine', text: live.length ? (live.length === 1 ? live[0].name + ' connected' : live.length + ' agents connected') : '' }),
      h('span', { class: 'mode', text: 'Read-only', title: 'Agents can look, never send or change. See Setup.' }),
    ),
    bursts.length
      ? h(
          'div',
          { class: 'agfbody' },
          bursts.slice(0, 30).map((b) => {
            const who = hitWho(b.client);
            const items = b.hits.map((x) => {
              const info = hitInfo(x);
              return h(
                info.go ? 'button' : 'div',
                { class: 'agfhit' + (x.refused ? ' refused' : ''), title: x.method + ' ' + x.path + (x.query ? '?' + x.query : ''), onclick: info.go || null },
                h('span', { class: 'ico', text: info.ico }),
                h('span', { class: 'tx', text: (x.refused ? 'Refused: ' : '') + info.text }),
              );
            });
            const more = items.length - SHOW;
            const rest = more > 0 ? items.slice(SHOW) : [];
            for (const r of rest) r.hidden = true;
            return h(
              'div',
              { class: 'agfburst' },
              h(
                'div',
                { class: 'agfwho' },
                who.chat ? h('button', { class: 'link', text: '✦ ' + who.label, title: 'Open this conversation', onclick: () => showAgentChat(who.chat) }) : h('b', { text: (who.ask ? '✦ ' : '') + who.label }),
                h('span', { class: 'muted', text: agoText(b.at) }),
              ),
              items.slice(0, SHOW),
              rest,
              more > 0
                ? h('button', {
                    class: 'link agfmore',
                    text: `+${more} more`,
                    onclick: (e) => {
                      for (const r of rest) r.hidden = false;
                      e.target.remove();
                    },
                  })
                : null,
            );
          }),
        )
      : h('div', { class: 'ab muted', text: 'When Claude or another agent reads this project, each thing it looks at shows up here, so you can follow along and open it yourself.' }),
  );
}

/* ---- setup: connecting agents, what they may do, skills ---- */

function drawAgentSetup() {
  const box = $('#agentsbody');
  const a = AG.policy;
  if (!box || !a) return;
  const now = Date.now();
  const clients = a.clients || [];
  const live = clients.filter((c) => now - c.last_seen < AGENT_LIVE_MS);
  const cmd = (a.connect && a.connect.command) || 'plonix connect claude';
  // The settings card keeps its own state across the status refreshes.
  let settingsCard = $('#agentsettings');
  const fresh = !settingsCard;
  if (fresh) settingsCard = h('div', { class: 'card', id: 'agentsettings' });
  clear(
    box,
    h('div', { class: 'sechead' }, h('h3', { text: 'Setup' }), h('span', { class: 'shacts' }, h('button', { class: 'btn sm ghost', text: 'Hide', onclick: () => toggleAgentSetup(false) }))),
    h(
      'div',
      { class: 'card agentstatus' },
      h(
        'div',
        { class: 'ah' },
        h('span', { class: 'adot' + (live.length ? ' on' : '') }),
        h('b', { text: live.length ? (live.length === 1 ? '1 agent connected' : live.length + ' agents connected') : 'No agent connected' }),
        h('span', { class: 'mode', text: 'Read-only' }),
      ),
      clients.length
        ? h(
            'table',
            { class: 'grid' },
            h('tr', null, h('th', { text: 'Agent' }), h('th', { text: 'Status' }), h('th', { text: 'Requests' }), h('th', { text: 'Last request' })),
            clients.map((c) =>
              h(
                'tr',
                null,
                h('td', { class: 'mono', text: c.name === 'plonix-ask' ? 'Ask Claude (in Plonix)' : c.name === 'plonix-watch' ? 'Watcher (in Plonix)' : c.name }),
                h('td', { text: now - c.last_seen < AGENT_LIVE_MS ? 'connected' : 'last seen ' + agoText(c.last_seen) }),
                h('td', { text: c.requests + (c.refused ? ` (${c.refused} refused)` : '') }),
                h('td', { class: 'mono muted', text: c.last_request }),
              ),
            ),
          )
        : h('div', { class: 'ab muted', text: 'When an agent starts the Plonix MCP server it shows up here, with every request it makes.' }),
    ),
    settingsCard,
    h('div', { class: 'sechead' }, h('h3', { text: 'Connect Claude Code' })),
    h(
      'div',
      { class: 'card' },
      h(
        'div',
        { class: 'ab' },
        h('p', null, 'Run this once in a terminal. It adds Plonix to Claude Code for all your projects:'),
        h('div', { class: 'cmdline' }, h('code', { text: cmd }), h('button', { class: 'btn sm', text: 'Copy', onclick: () => copyText(cmd) })),
        h(
          'p',
          { class: 'muted' },
          'Any other MCP client can run ',
          h('code', { text: 'plonix mcp' }),
          ' as a stdio server. The ',
          h('code', { text: 'plonix' }),
          ' command comes from ',
          h('code', { text: 'cargo install --path crates/plonix-cli' }),
          '. The agent reads whichever engine is running, including this one.',
        ),
      ),
    ),
    h('div', { class: 'sechead' }, h('h3', { text: 'What agents can do' })),
    h(
      'div',
      { class: 'card capgrid' },
      h('div', null, h('div', { class: 'caph ok', text: '✓ Allowed' }), [...new Set((a.capabilities || []).filter((c) => c.path !== '/api/agents').map((c) => c.what))].map((t) => h('div', { class: 'cap', text: t }))),
      h('div', null, h('div', { class: 'caph no', text: '✗ Not allowed' }), (a.not_allowed || []).map((t) => h('div', { class: 'cap', text: t }))),
    ),
    h(
      'p',
      { class: 'muted fine' },
      'The engine enforces this: agents sign in with their own token, and anything outside this list is refused. Captured traffic never leaves this Mac through Plonix, but it can hold passwords and session tokens, so connect only agents you trust.',
    ),
    h('div', { class: 'sechead' }, h('h3', { text: 'Skills' }), h('button', { class: 'btn sm ghost', text: 'Get more in the Market', onclick: () => leaveTo('market') })),
    h('div', { class: 'card', id: 'agentskills' }, h('div', { class: 'ab muted', text: 'Loading skills…' })),
    h('div', { class: 'sechead' }, h('h3', { text: 'Try asking from your terminal' })),
    h(
      'div',
      { class: 'card' },
      EXAMPLE_PROMPTS.map((p) => h('div', { class: 'prompt' }, h('span', { text: '“' + p + '”' }), h('button', { class: 'btn sm ghost', text: 'Copy', onclick: () => copyText(p) }))),
    ),
  );
  if (fresh) renderAgentSettings(settingsCard);
  loadAgentSkills();
}

/** Skills on the Agents screen: what agents are offered, and what is switched off. */
async function loadAgentSkills() {
  let r;
  try {
    r = await api('/api/skills');
  } catch (e) {
    return;
  }
  const box = $('#agentskills');
  if (!box) return;
  const skills = r.skills || [];
  clear(
    box,
    h('div', { class: 'ab muted', text: 'Playbooks agents follow for a job in Plonix. Claude Code offers them as slash commands; any MCP client sees them as prompts.' }),
    skills.map((sk) =>
      h(
        'div',
        { class: 'skillrow' + (sk.available ? '' : ' off') },
        h('div', { class: 'sk-main' }, h('b', { text: sk.title }), h('span', { class: 'muted', text: sk.description })),
        h('code', { class: 'sk-cmd', text: '/mcp__plonix__' + sk.name }),
        trustBadge(sk.verification, false),
        sk.available
          ? h('span', { class: 'tag in', text: 'offered' })
          : h('span', { class: 'tag out', title: 'Uses ' + sk.missing.map(groupLabel).join(', ') + ', which is switched off in Settings', text: 'off' }),
      ),
    ),
  );
}
