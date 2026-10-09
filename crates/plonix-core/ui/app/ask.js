// Plonix window: Ask Claude Code, and the AI agents settings panel.
// Part of the project window's scripts (see index.html for the load order).
'use strict';

/* ======================================================================
   Ask Claude Code: hand the agent the context for one spot in the app
   ====================================================================== */

async function loadAgentSettings() {
  try {
    S.agentSettings = await api('/api/agents/settings');
  } catch (_) {
    S.agentSettings = null;
  }
  for (const b of document.querySelectorAll('.askbtn')) b.hidden = !agentsOn();
}

const agentsOn = () => !S.agentSettings || S.agentSettings.settings.enabled;

/** A small "Ask Claude" button for a request, finding or host. */
function askButton(subject, title) {
  return h('button', {
    class: 'btn sm askbtn',
    hidden: !agentsOn(),
    title: title || 'Ask Claude Code about this, with just this context',
    onclick: (e) => {
      e.stopPropagation();
      askClaude(subject);
    },
  }, h('span', { class: 'askico', text: '✦' }), ' Ask Claude');
}

const fmtTok = (n) => (n >= 1000 ? (n / 1000).toFixed(n >= 10000 ? 0 : 1) + 'k' : String(n));
const fmtDur = (ms) => {
  const s = Math.floor(ms / 1000);
  return Math.floor(s / 60) + ':' + String(s % 60).padStart(2, '0');
};
/** How much Claude has read and written so far, and for how long: "38k read · 120 written · 0:14". */
const claudeStats = (p) => [p.tokens_in ? fmtTok(p.tokens_in) + ' tokens read' : null, p.tokens_out ? fmtTok(p.tokens_out) + ' written' : null, fmtDur(p.elapsed_ms)].filter(Boolean).join(' · ');
/**
 * Claude's Markdown answer as DOM nodes: headings, paragraphs, lists, quotes,
 * tables, rules, fenced code (with Copy), and inline code, bold, italics and
 * links. Built with text nodes only, never innerHTML, since answers quote
 * captured traffic. An unclosed fence (mid-stream) runs to the end.
 */
function mdNodes(src) {
  const lines = String(src).replace(/\r\n?/g, '\n').split('\n');
  const out = [];
  const isRule = (l) => /^\s{0,3}([-*_])(\s*\1){2,}\s*$/.test(l);
  const isRow = (l) => /^\s*\|.*\|\s*$/.test(l);
  const cells = (l) => l.trim().replace(/^\||\|$/g, '').split('|').map((c) => c.trim());
  const listRe = /^(\s*)([-*+]|\d+[.)])\s+(.*)$/;
  const startsBlock = (l) => /^\s*(```|~~~|#{1,6}\s|>)/.test(l) || listRe.test(l) || isRule(l) || isRow(l);
  let i = 0;
  while (i < lines.length) {
    const l = lines[i];
    if (!l.trim()) {
      i++;
      continue;
    }
    const fence = l.match(/^\s*(```|~~~)\s*([\w+-]*)/);
    if (fence) {
      const body = [];
      for (i++; i < lines.length && !lines[i].trim().startsWith(fence[1]); i++) body.push(lines[i]);
      i++;
      const code = body.join('\n').replace(/\n+$/, '');
      const copy = h('button', { class: 'mdcopy', text: 'Copy', title: 'Copy to the clipboard' });
      copy.onclick = async () => {
        await copyText(code);
        copy.textContent = 'Copied';
        setTimeout(() => (copy.textContent = 'Copy'), 1200);
      };
      out.push(h('div', { class: 'mdcode' }, copy, h('pre', null, h('code', { text: code }))));
      continue;
    }
    const head = l.match(/^\s*(#{1,6})\s+(.*?)\s*#*\s*$/);
    if (head) {
      out.push(h('h' + Math.min(6, head[1].length + 2), { class: 'mdh' }, mdInline(head[2])));
      i++;
      continue;
    }
    if (isRule(l)) {
      out.push(h('hr'));
      i++;
      continue;
    }
    if (/^\s*>/.test(l)) {
      const body = [];
      for (; i < lines.length && /^\s*>/.test(lines[i]); i++) body.push(lines[i].replace(/^\s*>\s?/, ''));
      out.push(h('blockquote', null, mdNodes(body.join('\n'))));
      continue;
    }
    if (isRow(l) && i + 1 < lines.length && /^\s*\|?[\s:|-]+\|?\s*$/.test(lines[i + 1]) && lines[i + 1].includes('-')) {
      const headCells = cells(l);
      const rows = [];
      for (i += 2; i < lines.length && isRow(lines[i]); i++) rows.push(cells(lines[i]));
      out.push(
        h(
          'div',
          { class: 'mdtable' },
          h('table', null, h('thead', null, h('tr', null, headCells.map((c) => h('th', null, mdInline(c))))), h('tbody', null, rows.map((r) => h('tr', null, r.map((c) => h('td', null, mdInline(c))))))),
        ),
      );
      continue;
    }
    const li = l.match(listRe);
    if (li) {
      const ordered = /\d/.test(li[2]);
      const indent = li[1].length;
      const items = [];
      while (i < lines.length) {
        const m = lines[i].match(listRe);
        if (m && m[1].length <= indent + 1 && /\d/.test(m[2]) === ordered) {
          items.push({ text: [m[3]], start: Number.parseInt(m[2], 10) });
          i++;
        } else if (lines[i].trim() && items.length && (/^\s{2,}/.test(lines[i]) || !startsBlock(lines[i])) && !(m && m[1].length <= indent)) {
          // A wrapped line or a nested item: it belongs to the last item.
          items[items.length - 1].text.push(lines[i].replace(new RegExp('^\\s{0,' + (indent + 3) + '}'), ''));
          i++;
        } else if (!lines[i].trim() && i + 1 < lines.length && (/^\s{2,}\S/.test(lines[i + 1]) || ((lines[i + 1].match(listRe) || [])[1] || '').length === indent)) {
          i++;
        } else break;
      }
      const list = h(ordered ? 'ol' : 'ul', ordered && items[0].start !== 1 ? { start: items[0].start } : null);
      for (const it of items) list.append(h('li', null, it.text.length > 1 ? mdNodes(it.text.join('\n')) : mdInline(it.text[0])));
      out.push(list);
      continue;
    }
    const para = [];
    for (; i < lines.length && lines[i].trim() && (!para.length || !startsBlock(lines[i])); i++) para.push(lines[i].trim());
    const p = h('p');
    para.forEach((t, k) => {
      if (k) p.append(h('br'));
      append(p, mdInline(t));
    });
    out.push(p);
  }
  return out;
}

/** Inline Markdown: `code`, **bold**, *italics*, ~~strike~~ and [links](url). */
function mdInline(t) {
  const out = [];
  const re = /(`+)([\s\S]*?[^`])\1(?!`)|\*\*([\s\S]+?)\*\*|__([\s\S]+?)__|~~([\s\S]+?)~~|\[([^\]]+)\]\((https?:\/\/[^)\s]+)\)|(?<![\w*])\*(?!\s)([^*]+?)\*(?![\w*])|(?<!\w)_(?!\s)([^_]+?)_(?!\w)/g;
  let last = 0;
  for (let m; (m = re.exec(t)); ) {
    if (m.index > last) out.push(t.slice(last, m.index));
    if (m[1]) out.push(h('code', { text: m[2].replace(/^ (.*) $/, '$1') }));
    else if (m[3] || m[4]) out.push(h('strong', null, mdInline(m[3] || m[4])));
    else if (m[5]) out.push(h('s', null, mdInline(m[5])));
    else if (m[6]) out.push(h('a', { href: m[7], target: '_blank', rel: 'noopener noreferrer', title: m[7] }, mdInline(m[6])));
    else out.push(h('em', null, mdInline(m[8] || m[9])));
    last = re.lastIndex;
  }
  if (last < t.length) out.push(t.slice(last));
  return out;
}

/** Claude Code says nothing for this long: tell the user it is still waiting, not frozen. */
const CLAUDE_QUIET_MS = 15000;

/**
 * A live in-app conversation with Claude Code: the transcript, the progress
 * line while Claude works, and the follow-up box. Used by the Ask sheet and
 * the Agents screen. Turns started with `ask` are saved as a chat the Agents
 * screen lists; follow-ups go into the same chat and resume its session.
 *
 * Hooks in `o`: onRunning(on) when a turn starts or ends, onError() when it
 * fails (to offer fallbacks), onDone({ proposed }) when a turn has finished,
 * onChat(id) when the turn was saved, and onStart() as a turn is sent.
 */
function claudeConvo(o = {}) {
  const convo = { id: null, since: 0, sessionId: null, running: false, proposed: false, chat: o.chat || null };
  const transcript = h('div', { class: 'convo' + (o.tall ? ' tall' : '') });
  const followIn = h('textarea', { class: 'cfollow', rows: 1, placeholder: 'Ask a follow-up…' });
  const sendBtn = h('button', { class: 'btn primary sm', text: 'Send' });
  const followRow = h('div', { class: 'cfollowrow', hidden: true }, followIn, sendBtn);

  const scroll = () => (transcript.scrollTop = transcript.scrollHeight);
  // While Claude works: what it is doing, tokens read and written, time.
  let thinking = null;
  let draft = null;
  const setThinking = (on) => {
    if (on && !thinking) {
      thinking = h(
        'div',
        { class: 'cthink' },
        h('span', { class: 'dots' }, h('span', { class: 'dot' }), h('span', { class: 'dot' }), h('span', { class: 'dot' })),
        h('span', { class: 'cstep', text: 'Starting Claude Code' }),
        h('span', { class: 'cstat' }),
        h('div', { class: 'cquiet', hidden: true }),
      );
      transcript.append(thinking);
      scroll();
    } else if (!on && thinking) {
      thinking.remove();
      thinking = null;
    }
    if (!on) dropDraft();
  };
  const dropDraft = () => {
    if (draft) draft.remove();
    draft = null;
  };
  const showProgress = (p) => {
    if (!p || !thinking) return;
    thinking.querySelector('.cstep').textContent = p.step;
    thinking.querySelector('.cstat').textContent = claudeStats(p);
    const quiet = thinking.querySelector('.cquiet');
    quiet.hidden = p.idle_ms < CLAUDE_QUIET_MS;
    quiet.textContent = `No word from Claude Code for ${Math.round(p.idle_ms / 1000)}s. It may be busy or slow to connect; it is stopped after 2 minutes of silence.`;
    // The answer as it is being written, replaced by the finished text.
    if (p.draft) {
      if (!draft) {
        draft = answer('', 'draft');
        add(draft);
      }
      const atEnd = transcript.scrollHeight - transcript.scrollTop - transcript.clientHeight < 40;
      if (draft.dataset.src !== p.draft) {
        draft.dataset.src = p.draft;
        clear(draft.firstChild, mdNodes(p.draft));
      }
      if (atEnd) scroll();
    }
  };
  const add = (node) => {
    if (thinking) transcript.insertBefore(node, thinking);
    else transcript.append(node);
    scroll();
  };
  const bubble = (role, text) => h('div', { class: 'cmsg ' + role }, h('div', { class: 'cbub', text }));
  // Claude answers in Markdown; show it formatted.
  const answer = (text, extra = '') => h('div', { class: 'cmsg bot ' + extra }, h('div', { class: 'cbub md' }, mdNodes(text)));
  const sayError = (text) => add(h('div', { class: 'cerr', text }));
  const toolLine = (text) => h('div', { class: 'ctool', text: '✦ ' + text });

  const alive = () => document.body.contains(transcript);
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const running = (on) => {
    convo.running = on;
    followIn.disabled = sendBtn.disabled = on;
    if (o.onRunning) o.onRunning(on);
  };

  const pollLoop = async () => {
    while (convo.running && alive()) {
      let snap;
      try {
        snap = await api(`/api/agents/run/${convo.id}?since=${convo.since}`);
      } catch (e) {
        setThinking(false);
        sayError(e.message);
        running(false);
        break;
      }
      for (const ev of snap.events) {
        convo.since = ev.seq + 1;
        if (ev.type === 'text') {
          dropDraft();
          add(answer(ev.text));
        } else if (ev.type === 'tool') {
          add(toolLine(ev.text));
        } else if (ev.type === 'proposal') {
          add(toolLine(ev.text));
          convo.proposed = true;
        } else if (ev.type === 'error') {
          setThinking(false);
          sayError(ev.text);
          if (o.onError) o.onError();
        }
      }
      if (snap.session_id) convo.sessionId = snap.session_id;
      if (snap.status !== 'running') {
        setThinking(false);
        const p = snap.progress;
        if (snap.status === 'done' && p) add(h('div', { class: 'cdone', text: `Answered in ${claudeStats(p).replace(/^(.*) · ([\d:]+)$/, '$2 · $1')}` }));
        running(false);
        if (snap.status === 'done') followRow.hidden = false;
        const proposed = convo.proposed;
        convo.proposed = false;
        if (o.onDone) o.onDone({ proposed, ok: snap.status === 'done' });
        break;
      }
      // Keep the working indicator alive between turns.
      if (!thinking) setThinking(true);
      showProgress(snap.progress);
      await sleep(500);
    }
  };

  /** Sends one turn: `prompt` goes to Claude, `ask` (what the user typed) is saved with the chat. */
  const start = async (prompt, ask, extra = {}) => {
    convo.since = 0;
    convo.proposed = false;
    followRow.hidden = true;
    if (o.onStart) o.onStart();
    add(bubble('user', ask));
    running(true);
    setThinking(true);
    try {
      const body = { prompt, ask, ...extra };
      if (convo.chat) body.chat = convo.chat;
      else if (convo.sessionId) body.resume = convo.sessionId;
      const r = await api('/api/agents/run', { method: 'POST', body });
      convo.id = r.id;
      if (r.chat && r.chat !== convo.chat) {
        convo.chat = r.chat;
        if (o.onChat) o.onChat(r.chat);
      }
    } catch (e) {
      setThinking(false);
      running(false);
      sayError(e.message);
      if (o.onError) o.onError();
      return;
    }
    pollLoop();
  };

  /** Shows a saved chat's turns, ready for a follow-up. */
  const load = (chat) => {
    clear(transcript);
    convo.chat = chat.id;
    convo.sessionId = chat.session_id || null;
    for (const t of chat.turns || []) {
      add(bubble('user', t.ask));
      for (const tool of t.tools || []) add(toolLine(tool));
      if (t.answer) add(answer(t.answer));
      if (t.error) add(h('div', { class: 'cerr', text: t.error }));
      if (t.status === 'running') add(h('div', { class: 'cdone', text: 'Still answering. Open this conversation again in a moment to see the answer.' }));
    }
    const last = (chat.turns || [])[chat.turns.length - 1];
    followRow.hidden = !!(last && last.status === 'running');
  };

  const stop = async () => {
    if (!convo.id || !convo.running) return;
    running(false);
    setThinking(false);
    try {
      await api(`/api/agents/run/${convo.id}`, { method: 'DELETE' });
    } catch (_) {}
    sayError('Stopped.');
    if (o.onError) o.onError();
  };

  const reset = () => {
    convo.id = convo.sessionId = convo.chat = null;
    clear(transcript);
    followRow.hidden = true;
  };

  sendBtn.onclick = () => {
    const t = followIn.value.trim();
    if (!t || convo.running) return;
    followIn.value = '';
    start(t, t);
  };
  followIn.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      sendBtn.onclick();
    }
  });

  return { el: h('div', { class: 'convobox' }, transcript, followRow), transcript, add, start, load, stop, reset, isRunning: () => convo.running, chat: () => convo.chat, focus: () => followIn.focus() };
}

/** What an Ask Claude subject is about, in a few words: "Request #42", "api.example.com". */
function subjectLabel(sub) {
  if (!sub) return '';
  if (sub.kind === 'request') return 'Request #' + sub.id;
  if (sub.kind === 'finding') return 'Finding #' + sub.id;
  if (sub.kind === 'host') return sub.host;
  if (sub.kind === 'draft') return 'Bench request';
  return '';
}

/** The part of a subject worth keeping with a saved chat: enough to link back. */
function subjectRef(sub) {
  if (!sub) return null;
  if (sub.kind === 'request' || sub.kind === 'finding') return { kind: sub.kind, id: sub.id };
  if (sub.kind === 'host') return { kind: 'host', host: sub.host };
  if (sub.kind === 'draft') return { kind: 'draft' };
  return null;
}

/** Opens what a saved chat is about, where it lives in the app. */
function openSubject(sub) {
  if (!sub) return;
  if (sub.kind === 'request') showExchange(sub.id);
  else if (sub.kind === 'finding') leaveTo('findings');
  else if (sub.kind === 'host') {
    M.sel = sub.host;
    leaveTo('map');
  } else if (sub.kind === 'draft') leaveTo('bench');
}

/**
 * The Ask sheet: shows exactly what will be shared and how big it is, lets
 * the user edit the question, drop parts and shorten bodies, and asks for
 * an explicit confirmation when it is larger than their limit.
 */
async function askClaude(subject, opts = {}) {
  const st = { exclude: [], question: opts.question || null, max: null, bundle: null, confirmBig: false };

  /* ---- compose view (what gets shared) ---- */
  const q = h('textarea', { class: 'askq', rows: 3, value: opts.question || '' });
  const partsBox = h('div', { class: 'askparts' });
  const meter = h('div', { class: 'askmeter' });
  const warn = h('div', { class: 'askwarn', hidden: true });
  const clipSel = h('select', { title: 'Each request and response body is clipped to this length' }, [1000, 2000, 4000, 8000, 16000, 50000].map((n) => h('option', { value: n, text: fmtTok(n) + ' chars' })));
  const cliHint = h('p', { class: 'muted fine', hidden: true });
  const composeView = h(
    'div',
    null,
    h('label', null, 'Your question', q),
    h('div', { class: 'askhead' }, h('span', { text: 'What Claude Code gets' }), h('label', { class: 'askclip' }, 'Bodies up to ', clipSel)),
    partsBox,
    meter,
    warn,
    h('p', { class: 'muted fine', text: 'Only what is ticked is sent, straight from this Mac to Claude Code. It may include passwords or session tokens from captured traffic.' }),
    cliHint,
  );

  /* ---- footer buttons ---- */
  const copyBtn = h('button', { class: 'btn', text: 'Copy prompt' });
  const termBtn = h('button', { class: 'btn', text: 'Open in Terminal' });
  const askBtn = h('button', { class: 'btn primary ai', text: '✦ Ask Claude' });
  const stopBtn = h('button', { class: 'btn', text: 'Stop', hidden: true });
  const newBtn = h('button', { class: 'btn', text: 'New question', hidden: true });

  /* ---- conversation view (the answer, in-app, saved on the Agents screen) ---- */
  const cv = claudeConvo({
    onStart: () => (m.err.textContent = ''),
    onRunning: (on) => (stopBtn.hidden = !on),
    onError: () => (copyBtn.hidden = termBtn.hidden = false),
    onDone: ({ proposed }) => proposed && offerReview(),
  });
  const savedNote = h('p', { class: 'muted fine', text: 'Saved in Agents, where you can pick this conversation up again later.' });
  const convoView = h('div', { hidden: true }, cv.el, savedNote);

  let askInApp = true;
  let timer;
  const rebuild = async () => {
    try {
      st.bundle = await api('/api/agents/ask', { method: 'POST', body: { ...subject, question: st.question, exclude: st.exclude, max_body_chars: st.max } });
    } catch (e) {
      m.err.textContent = e.message;
      return;
    }
    m.err.textContent = '';
    draw();
  };
  const later = () => {
    clearTimeout(timer);
    timer = setTimeout(rebuild, 350);
  };
  const draw = () => {
    const b = st.bundle;
    if (st.question == null) q.value = b.question;
    clipSel.value = String(b.max_body_chars);
    if (![...clipSel.options].some((o) => o.value === String(b.max_body_chars))) clipSel.append(h('option', { value: b.max_body_chars, text: fmtTok(b.max_body_chars) + ' chars', selected: true }));
    clear(
      partsBox,
      b.parts.map((p) => {
        const box = h('input', {
          type: 'checkbox',
          checked: p.included,
          onchange: () => {
            st.exclude = box.checked ? st.exclude.filter((x) => x !== p.id) : [...st.exclude, p.id];
            st.confirmBig = false;
            rebuild();
          },
        });
        const pre = h('pre', { class: 'askpre', hidden: true, text: p.text });
        return h(
          'div',
          { class: 'askpart' + (p.included ? '' : ' off') },
          h('label', null, box, h('span', { class: 'pl', text: p.label }), p.clipped ? h('span', { class: 'clipped', text: 'clipped' }) : null, h('span', { class: 'pt', text: '~' + fmtTok(p.tokens) + ' tokens' })),
          h('button', { class: 'link', text: 'preview', onclick: () => (pre.hidden = !pre.hidden) }),
          pre,
        );
      }),
    );
    const pct = Math.min(100, Math.round((b.tokens / b.budget) * 100));
    clear(meter, h('div', { class: 'bar' + (b.over_budget ? ' over' : '') }, h('i', { style: { width: pct + '%' } })), h('span', { text: `~${fmtTok(b.tokens)} of your ${fmtTok(b.budget)}-token limit` }));
    warn.hidden = !b.over_budget;
    if (b.over_budget) {
      const ok = h('input', { type: 'checkbox', checked: st.confirmBig, onchange: () => ((st.confirmBig = ok.checked), sync()) });
      clear(
        warn,
        h('b', { text: `This is about ${fmtTok(b.tokens)} tokens, over your limit of ${fmtTok(b.budget)}.` }),
        ' A large context makes answers slower and less focused. Untick parts or shorten the bodies, or ',
        h('label', null, ok, ' send it anyway'),
        '. The limit is in Settings › AI agents.',
      );
    }
    sync();
  };
  const sync = () => {
    const blocked = !st.bundle || (st.bundle.over_budget && !st.confirmBig);
    copyBtn.disabled = blocked;
    termBtn.disabled = blocked;
    askBtn.disabled = blocked || !askInApp;
  };

  /* ---- view switching ---- */
  const showCompose = () => {
    composeView.hidden = false;
    convoView.hidden = true;
    copyBtn.hidden = termBtn.hidden = askBtn.hidden = false;
    stopBtn.hidden = newBtn.hidden = true;
    sync();
  };
  const showConvo = () => {
    composeView.hidden = true;
    convoView.hidden = false;
    copyBtn.hidden = termBtn.hidden = askBtn.hidden = true;
    newBtn.hidden = false;
  };

  // Claude suggested an edit to the Bench draft: point to the review there.
  // Nothing has changed yet; the Bench shows the diff with Apply and Discard.
  const offerReview = async () => {
    if (subject.kind !== 'draft' || !subject.draft_id) return;
    let list = [];
    try {
      list = (await api('/api/bench/proposals?draft=' + encodeURIComponent(subject.draft_id))).proposals || [];
    } catch (_) {}
    if (!list.length || !document.body.contains(cv.transcript)) return;
    cv.add(
      h(
        'div',
        { class: 'cprop' },
        h('span', { text: 'Claude suggested an edit to this request. Your draft is unchanged until you apply it.' }),
        h('button', {
          class: 'btn primary sm',
          text: 'Review on the Bench',
          onclick: () => {
            closeModal();
            if (S.view === 'bench') drawProposal(R.tabs[R.active], $('#main'));
            else leaveTo('bench');
          },
        }),
      ),
    );
  };

  q.addEventListener('input', () => {
    st.question = q.value;
    later();
  });
  clipSel.addEventListener('change', () => {
    st.max = Number(clipSel.value);
    st.confirmBig = false;
    rebuild();
  });
  askBtn.onclick = () => {
    if (!st.bundle) return;
    showConvo();
    cv.start(st.bundle.prompt, st.bundle.question, { subject: subjectRef(subject) });
  };
  stopBtn.onclick = () => cv.stop();
  newBtn.onclick = () => {
    if (cv.isRunning()) return;
    cv.reset();
    showCompose();
  };
  copyBtn.onclick = async () => {
    await copyText(st.bundle.prompt);
    toast('Prompt copied', 'ok');
  };
  termBtn.onclick = async () => {
    try {
      await api('/api/agents/launch', { method: 'POST', body: { prompt: st.bundle.prompt } });
      toast('Opened Claude Code in Terminal', 'ok');
    } catch (e) {
      if (e.code === 'unsupported') {
        await copyText(st.bundle.prompt);
        m.err.textContent = 'Opening a terminal works on macOS only. The prompt is on your clipboard: paste it into Claude Code.';
      } else m.err.textContent = e.message;
    }
  };

  const m = modal('Ask Claude Code', [composeView, convoView], [h('button', { class: 'btn', text: 'Close', onclick: closeModal }), newBtn, stopBtn, copyBtn, termBtn, askBtn]);
  m.el.querySelector('.mcard').classList.add('wide');

  // Is the Claude Code CLI installed here? If not, keep Terminal/Copy only.
  try {
    const pol = await api('/api/agents');
    askInApp = pol.ask_in_app !== false;
  } catch (_) {}
  if (!askInApp) {
    cliHint.hidden = false;
    cliHint.textContent = 'Claude Code is not installed on this machine, so the answer cannot run inside Plonix yet. Install it from claude.com/claude-code, or use Open in Terminal.';
  }
  await rebuild();
}

/** What agents may do, in one line, with the way to change it: Settings › AI agents. */
async function renderAgentSettings(box) {
  let cfg;
  try {
    cfg = await api('/api/agents/settings');
  } catch (e) {
    return clear(box, h('div', { class: 'ab rerr', text: e.message }));
  }
  S.agentSettings = cfg;
  const st = cfg.settings;
  const on = cfg.groups.filter((g) => g.on).length;
  const summary = st.enabled
    ? `Agents see ${st.data === 'all' ? 'everything captured' : 'in-scope hosts only'} · ${on} of ${cfg.groups.length} kinds of data · Ask Claude up to ${fmtTok(st.context_budget)} tokens`
    : 'Agent access is off: every agent request is refused.';
  const open = () => {
    S.settingsSection = 'agents';
    leaveTo('settings');
  };
  clear(
    box,
    h(
      'div',
      { class: 'ab setrow' },
      h('span', null, h('b', { text: 'Agent settings' }), h('br'), h('span', { class: 'muted', text: summary })),
      h('button', { class: 'btn sm', text: 'Change in Settings…', onclick: open }),
    ),
  );
}
