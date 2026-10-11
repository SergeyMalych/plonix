// Helpers shared by the project window (app/*.js), the Start screen
// (launcher.js) and the settings forms (settings.js). Loaded first on both
// pages; plain JavaScript, no build step.
//
// Captured traffic is attacker-controlled, so nothing from the API is ever
// parsed as HTML: the DOM is built with h() and text nodes only.
'use strict';

/** Plonix runs on Macs and Windows PCs; text that names the computer says which. */
const ON_WINDOWS = /Windows/.test(navigator.userAgent);
const THIS_COMPUTER = ON_WINDOWS ? 'this PC' : 'this Mac';

/* ---------- DOM helpers ---------- */

function h(tag, props, ...kids) {
  const el = document.createElement(tag);
  if (props) {
    for (const [k, v] of Object.entries(props)) {
      if (v == null || v === false) continue;
      if (k === 'class') el.className = v;
      else if (k === 'text') el.textContent = v;
      else if (k === 'value' || k === 'checked' || k === 'disabled' || k === 'selected') el[k] = v;
      else if (k === 'style') Object.assign(el.style, v);
      else if (k.startsWith('on') && typeof v === 'function') el.addEventListener(k.slice(2), v);
      else el.setAttribute(k, v === true ? '' : String(v));
    }
  }
  append(el, kids);
  return el;
}

function append(el, kids) {
  for (const c of kids.flat(Infinity)) {
    if (c == null || c === false) continue;
    el.append(c instanceof Node ? c : String(c));
  }
  return el;
}

function clear(el, ...kids) {
  el.replaceChildren();
  return append(el, kids);
}

const $ = (sel, root = document) => root.querySelector(sel);

/* ---------- formatting ---------- */

function fmtSize(n) {
  if (n == null || n < 0) return '';
  if (n < 1024) return n + ' B';
  if (n < 1024 * 1024) return (n / 1024).toFixed(n < 10240 ? 1 : 0) + ' KB';
  if (n < 1024 * 1024 * 1024) return (n / 1048576).toFixed(1) + ' MB';
  return (n / 1073741824).toFixed(2) + ' GB';
}

/* ---------- browser storage ---------- */

function store(key, value) {
  try {
    if (value === undefined) return JSON.parse(localStorage.getItem(key));
    if (value === null) localStorage.removeItem(key);
    else localStorage.setItem(key, JSON.stringify(value));
  } catch (_) {
    return null;
  }
}

/* ---------- API errors ---------- */

class ApiError extends Error {
  constructor(status, code, message, data) {
    super(message);
    this.status = status;
    this.code = code;
    this.problems = data && data.problems;
    this.data = data;
  }
}

/* ---------- toasts & modal ---------- */

function toast(msg, kind = '') {
  let box = $('.toasts');
  if (!box) document.body.append((box = h('div', { class: 'toasts' })));
  const t = h('div', { class: 'toast ' + kind, text: msg });
  box.append(t);
  setTimeout(() => t.remove(), kind === 'err' ? 6000 : 3200);
}

function closeModal() {
  const m = $('.modal');
  if (m) m.remove();
}

function modal(title, body, actions, wide) {
  closeModal();
  const err = h('span', { class: 'err' });
  const m = h(
    'div',
    { class: 'modal', onmousedown: (e) => e.target === m && closeModal() },
    h('div', { class: 'mcard' + (wide ? ' wide' : ''), role: 'dialog' }, h('h3', { text: title }), h('div', { class: 'mb' }, body), h('div', { class: 'mf' }, err, actions)),
  );
  document.body.append(m);
  const first = m.querySelector('input, textarea, select');
  if (first) first.focus();
  return { el: m, err };
}

/* ---------- text size: the same in every window ---------- */

/** The sizes ⌘+ and ⌘− step through, in percent (settings::TEXT_SIZES). */
const TEXT_SIZES = [80, 90, 100, 110, 120, 135, 150];
const MOD_KEY = ON_WINDOWS ? 'Ctrl+' : '⌘';
let textSize = 100;

/** Takes the saved text size. The desktop app zooms every window to it and
 *  calls this after ⌘+, ⌘− or ⌘0 with the `step` that was asked for, so the
 *  window that has the keyboard says what happened. */
function showTextSize(n, step) {
  n = TEXT_SIZES.includes(Number(n)) ? Number(n) : 100;
  const same = n === textSize;
  textSize = n;
  store('plonix.text', n);
  for (const el of document.querySelectorAll('.tsize')) paintTextSize(el);
  if (step == null) return;
  if (same && step > 0 && n === TEXT_SIZES[TEXT_SIZES.length - 1]) toast('Text is at its largest size.');
  else if (same && step < 0 && n === TEXT_SIZES[0]) toast('Text is at its smallest size.');
  else if (!same) toast('Text size ' + n + '%');
}
window.plonixTextSize = showTextSize;

/** Saves a new text size for every window. */
function setTextSize(n, step) {
  return api('/api/ui/style', { method: 'PUT', body: { text: n } })
    .then((r) => showTextSize(r.text, step))
    .catch((e) => toast(e.message, 'err'));
}

function stepTextSize(step) {
  const i = Math.max(0, TEXT_SIZES.indexOf(textSize));
  const n = step === 0 ? 100 : TEXT_SIZES[Math.min(TEXT_SIZES.length - 1, Math.max(0, i + Math.sign(step)))];
  return setTextSize(n, step);
}

/** Settings › Appearance › Text size. It applies right away, like ⌘+ and ⌘−. */
function textSizeField() {
  return {
    key: 'text',
    label: 'Text size',
    type: 'custom',
    render: textSizeControl,
    help: window.__PLONIX_APP__
      ? `Every Plonix window follows it. ${MOD_KEY}+ and ${MOD_KEY}− change it from anywhere, ${MOD_KEY}0 goes back to 100%.`
      : 'Every Plonix app window follows it. In a web browser, use the browser\'s own zoom.',
  };
}

function textSizeControl() {
  const el = h(
    'div',
    { class: 'tsize' },
    h(
      'div',
      { class: 'seg-ctl' },
      h('button', { type: 'button', class: 'segbtn ts-down', title: 'Smaller text (' + MOD_KEY + '−)', 'aria-label': 'Smaller text', onclick: () => stepTextSize(-1) }, h('span', { class: 'ts-a sm', text: 'A' })),
      h('span', { class: 'ts-val', 'aria-live': 'polite' }),
      h('button', { type: 'button', class: 'segbtn ts-up', title: 'Bigger text (' + MOD_KEY + '+)', 'aria-label': 'Bigger text', onclick: () => stepTextSize(1) }, h('span', { class: 'ts-a', text: 'A' })),
    ),
    h('button', { type: 'button', class: 'btn ts-reset', text: 'Reset', title: 'Back to 100% (' + MOD_KEY + '0)', onclick: () => stepTextSize(0) }),
  );
  paintTextSize(el);
  return el;
}

function paintTextSize(el) {
  el.querySelector('.ts-val').textContent = textSize + '%';
  el.querySelector('.ts-down').disabled = textSize <= TEXT_SIZES[0];
  el.querySelector('.ts-up').disabled = textSize >= TEXT_SIZES[TEXT_SIZES.length - 1];
  el.querySelector('.ts-reset').hidden = textSize === 100;
}

// In the app, View › Bigger Text (⌘=) and Smaller Text (⌘−) are menu
// items; this catches the other keys people use for them: ⌘+ with Shift and
// the number pad.
if (window.__PLONIX_APP__) {
  addEventListener('keydown', (e) => {
    if (!(ON_WINDOWS ? e.ctrlKey : e.metaKey) || e.altKey) return;
    const step = e.key === '+' || e.code === 'NumpadAdd' ? 1 : e.code === 'NumpadSubtract' ? -1 : e.code === 'Numpad0' ? 0 : null;
    if (step == null) return;
    e.preventDefault();
    stepTextSize(step);
  });
}
