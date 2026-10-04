/* Plonix settings forms, shared by the project window and the Start screen.
 *
 * Every settings section describes its own fields (see settings.rs), so this
 * file draws any section, including ones added later, without changes:
 *
 *   PlonixSettings.render(container, data, {
 *     save: async (section, values) => result,   // throws {problems} on bad values
 *     extra: (section, box) => {},               // optional: add panels under a section
 *     select: 'proxy',                           // optional: section to show first
 *   });
 */
'use strict';

(function () {
  function h(tag, props, ...kids) {
    const el = document.createElement(tag);
    for (const [k, v] of Object.entries(props || {})) {
      if (v == null || v === false) continue;
      if (k === 'class') el.className = v;
      else if (k === 'text') el.textContent = v;
      else if (k.startsWith('on')) el.addEventListener(k.slice(2), v);
      else if (k in el && typeof v !== 'string') el[k] = v;
      else el.setAttribute(k, v === true ? '' : v);
    }
    for (const kid of kids.flat(Infinity)) {
      if (kid == null || kid === false) continue;
      el.append(kid instanceof Node ? kid : document.createTextNode(String(kid)));
    }
    return el;
  }

  const LEVEL = { project: 'This project', global: 'All projects' };
  const APPLIES = { now: 'Saved. Changes apply right away.', next_open: 'Saved. Changes apply the next time the project opens.' };

  function same(a, b) {
    return JSON.stringify(a) === JSON.stringify(b);
  }

  /** One input for one field. Returns {el, get(), set(v), error(msg)}. */
  function control(field, value, onChange) {
    const id = 'f-' + field.key;
    let input;
    let get;
    let set;
    switch (field.type) {
      case 'toggle': {
        input = h('input', { type: 'checkbox', id, class: 'switch', onchange: onChange });
        get = () => input.checked;
        set = (v) => (input.checked = !!v);
        break;
      }
      case 'number': {
        input = h('input', { type: 'number', id, min: field.min, max: field.max, step: 1, oninput: onChange });
        get = () => (input.value.trim() === '' ? input.value : Number(input.value));
        set = (v) => (input.value = v);
        break;
      }
      case 'choice': {
        if (field.options.length <= 3) {
          const btns = field.options.map((o) =>
            h('button', { type: 'button', class: 'segbtn', 'data-v': o.value, text: o.label, onclick: () => { set(o.value); onChange(); } }),
          );
          input = h('div', { class: 'seg-ctl', role: 'radiogroup', id }, btns);
          let cur = value;
          get = () => cur;
          set = (v) => {
            cur = v;
            btns.forEach((b) => b.classList.toggle('on', b.dataset.v === v));
          };
        } else {
          input = h('select', { id, onchange: onChange }, field.options.map((o) => h('option', { value: o.value, text: o.label })));
          get = () => input.value;
          set = (v) => (input.value = v);
        }
        break;
      }
      case 'list': {
        input = h('textarea', { id, rows: 3, spellcheck: 'false', placeholder: field.placeholder || '', oninput: onChange });
        get = () => input.value.split('\n').map((s) => s.trim()).filter(Boolean);
        set = (v) => (input.value = (v || []).join('\n'));
        break;
      }
      case 'secret': {
        input = h('input', { type: 'password', id, autocomplete: 'off', spellcheck: 'false', oninput: onChange });
        get = () => input.value;
        set = (v) => (input.value = v || '');
        break;
      }
      default: {
        input = h('input', { type: 'text', id, autocomplete: 'off', spellcheck: 'false', placeholder: field.placeholder || '', oninput: onChange });
        get = () => input.value;
        set = (v) => (input.value = v == null ? '' : v);
      }
    }
    set(value);
    const err = h('div', { class: 'ferr' });
    const label = h('label', { for: id, class: 'flabel', text: field.label });
    const help = field.help ? h('div', { class: 'fhelp', text: field.help }) : null;
    const unit = field.unit ? h('span', { class: 'funit', text: field.unit }) : null;
    const row =
      field.type === 'toggle'
        ? h('div', { class: 'frow ftoggle' }, h('div', { class: 'fmain' }, input, label), help, err)
        : h('div', { class: 'frow' }, label, h('div', { class: 'finput' }, input, unit), help, err);
    return {
      el: row,
      get,
      set,
      error(msg) {
        err.textContent = msg || '';
        row.classList.toggle('bad', !!msg);
      },
    };
  }

  function form(section, opts) {
    const controls = {};
    let saved = { ...section.values };
    const status = h('span', { class: 'fstatus' });
    const saveBtn = h('button', { class: 'btn primary', text: 'Save', disabled: true });
    const revertBtn = h('button', { class: 'btn', text: 'Revert', disabled: true });
    const values = () => Object.fromEntries(Object.entries(controls).map(([k, c]) => [k, c.get()]));
    const dirty = () => !same(values(), saved);
    const changed = () => {
      const d = dirty();
      saveBtn.disabled = !d;
      revertBtn.disabled = !d;
      if (d) status.textContent = '';
    };
    const body = h('div', { class: 'sfields' });
    let group = null;
    for (const f of section.fields) {
      if ((f.group || '') !== (group || '')) {
        group = f.group || '';
        if (group) body.append(h('div', { class: 'sgroup', text: group }));
      }
      const c = control(f, section.values[f.key], changed);
      controls[f.key] = c;
      body.append(c.el);
    }
    revertBtn.addEventListener('click', () => {
      for (const [k, c] of Object.entries(controls)) {
        c.set(saved[k]);
        c.error('');
      }
      changed();
    });
    saveBtn.addEventListener('click', async () => {
      saveBtn.disabled = true;
      Object.values(controls).forEach((c) => c.error(''));
      status.className = 'fstatus';
      status.textContent = 'Saving…';
      try {
        const r = await opts.save(section.id, values());
        saved = { ...(r && r.values ? r.values : values()) };
        for (const [k, c] of Object.entries(controls)) c.set(saved[k]);
        section.values = saved;
        status.className = 'fstatus ok';
        status.textContent = (r && r.message) || APPLIES[(r && r.applies) || section.applies] || 'Saved.';
      } catch (e) {
        status.className = 'fstatus bad';
        const problems = (e && e.problems) || [];
        problems.forEach((p) => controls[p.field] && controls[p.field].error(p.message));
        status.textContent = problems.length ? 'Fix the highlighted settings.' : (e && e.message) || 'Could not save.';
      }
      changed();
    });
    const box = h(
      'div',
      { class: 'sform' },
      h('div', { class: 'shead' }, h('h2', { text: section.title }), h('span', { class: 'slevel ' + section.level, text: LEVEL[section.level] || '' })),
      section.description ? h('p', { class: 'sdesc', text: section.description }) : null,
      body,
      h('div', { class: 'sactions' }, status, revertBtn, saveBtn),
    );
    if (opts.extra) opts.extra(section, box);
    return box;
  }

  function render(container, data, opts) {
    const sections = (data.sections || []).filter((s) => !opts.only || opts.only.includes(s.level));
    const list = h('nav', { class: 'slist' });
    const pane = h('div', { class: 'spane' });
    const show = (id) => {
      const sec = sections.find((s) => s.id === id) || sections[0];
      if (!sec) return;
      for (const b of list.querySelectorAll('button')) b.classList.toggle('on', b.dataset.id === sec.id);
      pane.replaceChildren(form(sec, opts));
      if (opts.onSelect) opts.onSelect(sec.id);
    };
    for (const s of sections) {
      list.append(
        h('button', { 'data-id': s.id, onclick: () => show(s.id) }, h('span', { class: 'sl', text: s.title }), h('span', { class: 'sb', text: s.level === 'global' ? 'All' : '' })),
      );
    }
    container.replaceChildren(h('div', { class: 'settings' }, list, pane));
    show(opts.select);
    return { show };
  }

  window.PlonixSettings = { render, h };
})();
