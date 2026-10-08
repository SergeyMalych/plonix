// Helpers shared by the project window (app.js), the Start screen
// (launcher.js) and the settings forms (settings.js). Loaded first on both
// pages; plain JavaScript, no build step.
//
// Captured traffic is attacker-controlled, so nothing from the API is ever
// parsed as HTML: the DOM is built with h() and text nodes only.
'use strict';

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
