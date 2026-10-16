#!/usr/bin/env node
// Builds the documentation pages of plonix.io from docs/*.md.
//
//   node scripts/build-docs.mjs           write site/docs/
//   node scripts/build-docs.mjs --check   fail if site/docs/ is out of date
//
// The site is static and has no build step, so the generated pages are
// committed; CI runs --check. No dependencies: the Markdown renderer below
// handles what the docs use (headings, paragraphs, lists, code blocks,
// tables, quotes, links, images, emphasis and inline code). Links between
// docs become links between pages, and links to other files in the
// repository go to GitHub.

import crypto from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const DOCS = path.join(ROOT, 'docs');
const OUT = path.join(ROOT, 'site', 'docs');
const REPO = 'https://github.com/SergeyMalych/plonix';
const RAW = 'https://raw.githubusercontent.com/SergeyMalych/plonix/main';

// Sidebar sections, in order; docs not listed here go under "More", by name.
const GROUPS = [
  ['Get started', ['getting-started', 'app', 'projects', 'setup']],
  ['Capture and explore', ['traffic', 'filters', 'scope', 'map', 'rules']],
  ['Experiment and prove', ['bench', 'users', 'callbacks', 'scanning', 'findings', 'programs']],
  ['AI and the Market', ['agents', 'market', 'detection-rules', 'extensions']],
  ['Your data', ['updates', 'privacy', 'crash-reports', 'code-signing']],
  ['Contributing', ['releasing']],
];
const ORDER = GROUPS.flatMap(([, names]) => names);

// ---------------------------------------------------------------- markdown

const esc = (s) => s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
const unesc = (s) => s.replace(/&lt;/g, '<').replace(/&gt;/g, '>').replace(/&quot;/g, '"').replace(/&#39;/g, "'").replace(/&amp;/g, '&');
const stripTags = (html) => unesc(html.replace(/<[^>]+>/g, ''));

/** GitHub's heading anchors: lower case, punctuation dropped, spaces to dashes. */
function slug(text) {
  return text.toLowerCase().trim().replace(/[^\p{L}\p{N}\s_-]/gu, '').replace(/\s/g, '-');
}

/** Renders inline Markdown. `link` rewrites link and image targets. */
function inline(src, link) {
  const held = [];
  const hold = (html) => `\u0000${held.push(html) - 1}\u0000`;
  let out = '';
  let i = 0;
  while (i < src.length) {
    const c = src[i];
    if (c === '\\' && i + 1 < src.length && /[!-\/:-@\[-`{-~]/.test(src[i + 1])) {
      out += hold(esc(src[i + 1]));
      i += 2;
      continue;
    }
    if (c === '`') {
      const run = /^`+/.exec(src.slice(i))[0];
      const end = src.indexOf(run, i + run.length);
      if (end > 0) {
        let code = src.slice(i + run.length, end);
        if (/^ .* $/.test(code) && code.trim()) code = code.slice(1, -1);
        out += hold(`<code>${esc(code)}</code>`);
        i = end + run.length;
        continue;
      }
      out += run;
      i += run.length;
      continue;
    }
    if (c === '<') {
      const auto = /^<(https?:\/\/[^\s>]+)>/.exec(src.slice(i));
      if (auto) {
        out += hold(`<a href="${esc(link(auto[1], false))}">${esc(auto[1])}</a>`);
        i += auto[0].length;
        continue;
      }
    }
    if (c === '[' || (c === '!' && src[i + 1] === '[')) {
      const image = c === '!';
      const m = matchLink(src, image ? i + 1 : i);
      if (m) {
        const href = link(m.href, image);
        const title = m.title ? ` title="${esc(m.title)}"` : '';
        if (image) {
          const alt = esc(stripTags(inline(m.text, link)));
          // A site screenshot with a dark twin shows the one that fits the theme.
          const pair = /^\.\.\/assets\/[\w-]+-light\.webp$/.test(href);
          const dark = href.replace(/-light\.webp$/, '-dark.webp');
          out += pair
            ? hold(`<img class="pic-light" src="${esc(href + assetVersion(href))}" alt="${alt}"${title} loading="lazy"><img class="pic-dark" src="${esc(dark + assetVersion(dark))}" alt="${alt}"${title} loading="lazy">`)
            : hold(`<img src="${esc(href)}" alt="${alt}"${title} loading="lazy">`);
        } else {
          const ext = /^https?:/.test(href) ? ' rel="noopener"' : '';
          out += hold(`<a href="${esc(href)}"${title}${ext}>${inline(m.text, link)}</a>`);
        }
        i = m.end;
        continue;
      }
    }
    out += c;
    i++;
  }
  // Plain text: escape, then emphasis and bare links. Held pieces are
  // markers made of NUL and digits, which none of these patterns touch.
  out = esc(out)
    .replace(/\*\*(?=\S)([\s\S]*?\S)\*\*/g, '<strong>$1</strong>')
    .replace(/(^|[^\w])__(?=\S)([\s\S]*?\S)__(?!\w)/g, '$1<strong>$2</strong>')
    .replace(/(^|[^\w*])\*(?=[^\s*])([\s\S]*?[^\s*])\*(?![\w*])/g, '$1<em>$2</em>')
    .replace(/(^|[^\w])_(?=[^\s_])([\s\S]*?[^\s_])_(?!\w)/g, '$1<em>$2</em>')
    .replace(/~~(?=\S)([\s\S]*?\S)~~/g, '<del>$1</del>')
    .replace(/(^|[\s(])(https?:\/\/[^\s<\u0000]*[^\s<\u0000.,;:!?)'"])/g, (_, pre, url) => `${pre}<a href="${url}" rel="noopener">${url}</a>`);
  while (out.includes('\u0000')) out = out.replace(/\u0000(\d+)\u0000/g, (_, n) => held[+n]);
  return out;
}

/** `[text](href "title")` starting at `at`, with nested brackets and parentheses. */
function matchLink(src, at) {
  let depth = 0;
  let j = at;
  for (; j < src.length; j++) {
    if (src[j] === '\\') { j++; continue; }
    if (src[j] === '[') depth++;
    else if (src[j] === ']' && --depth === 0) break;
  }
  if (j >= src.length || src[j + 1] !== '(') return null;
  const text = src.slice(at + 1, j);
  let k = j + 2;
  let parens = 1;
  for (; k < src.length; k++) {
    if (src[k] === '(') parens++;
    else if (src[k] === ')' && --parens === 0) break;
  }
  if (k >= src.length) return null;
  const inner = src.slice(j + 2, k).trim();
  const m = /^<?([^\s>]*)>?(?:\s+["'(](.*)["')])?$/.exec(inner);
  if (!m) return null;
  return { text, href: m[1], title: m[2], end: k + 1 };
}

const FENCE = /^(\s*)(`{3,}|~{3,})\s*([\w+#.-]*).*$/;
const HEADING = /^ {0,3}(#{1,6})\s+(.*?)\s*#*\s*$/;
const RULE = /^ {0,3}([-*_])(\s*\1){2,}\s*$/;
const ITEM = /^( {0,3})([-*+]|\d{1,9}[.)])(\s+|$)(.*)$/;
const TABLE_SEP = /^\s*\|?\s*:?-+:?\s*(\|\s*:?-+:?\s*)*\|?\s*$/;
const indentOf = (line) => /^\s*/.exec(line)[0].replace(/\t/g, '    ').length;
const blank = (line) => !line.trim();

/** Splits a table row into cells, minding escaped pipes and code spans. */
function cells(row) {
  let s = row.trim();
  if (s.startsWith('|')) s = s.slice(1);
  if (s.endsWith('|') && !s.endsWith('\\|')) s = s.slice(0, -1);
  const out = [];
  let cur = '';
  let code = false;
  for (let i = 0; i < s.length; i++) {
    if (s[i] === '\\' && s[i + 1] === '|') { cur += '|'; i++; continue; }
    if (s[i] === '`') code = !code;
    if (s[i] === '|' && !code) { out.push(cur.trim()); cur = ''; continue; }
    cur += s[i];
  }
  out.push(cur.trim());
  return out;
}

/**
 * Sizes table columns from their text. A column whose cells are all short
 * keeps to one line ("nw"); the rest share the width by how much they hold,
 * so a column of terse labels never takes half the table from a column of
 * sentences, and no column is squeezed until its words break apart.
 */
const text = (html) => html.replace(/<[^>]*>/g, '').replace(/&[a-z]+;|&#\d+;/g, 'x').trim();

function columns(rows) {
  const n = rows[0].length;
  const cols = [];
  for (let c = 0; c < n; c++) {
    const lens = rows.map((r) => text(r[c] ?? '').length);
    const longest = Math.max(...lens);
    const body = lens.slice(1);
    const avg = body.length ? body.reduce((a, b) => a + b, 0) / body.length : longest;
    const short = longest <= 24;
    cols.push({ short, weight: short ? Math.max(longest, 6) : Math.max(avg, 16) });
  }
  const total = cols.reduce((a, c) => a + c.weight, 0);
  for (const c of cols) c.width = Math.round((c.weight / total) * 1000) / 10;
  return cols;
}

function startsBlock(line, next) {
  return FENCE.test(line) || HEADING.test(line) || RULE.test(line) || /^ {0,3}>/.test(line) || ITEM.test(line) || (line.includes('|') && next !== undefined && TABLE_SEP.test(next) && next.includes('-'));
}

/** Renders a run of Markdown lines as HTML blocks. */
function blocks(lines, ctx) {
  const html = [];
  let i = 0;
  while (i < lines.length) {
    const line = lines[i];
    if (blank(line)) { i++; continue; }

    let m = FENCE.exec(line);
    if (m) {
      const [, ind, marker, lang] = m;
      const body = [];
      i++;
      while (i < lines.length && !new RegExp(`^\\s*${marker[0]}{${marker.length},}\\s*$`).test(lines[i])) {
        body.push(lines[i].startsWith(ind) ? lines[i].slice(ind.length) : lines[i].trimStart());
        i++;
      }
      i++;
      const cls = lang ? ` class="language-${esc(lang)}"` : '';
      html.push(`<pre><code${cls}>${esc(body.join('\n'))}</code></pre>`);
      continue;
    }

    if ((m = HEADING.exec(line))) {
      const level = m[1].length;
      const content = inline(m[2], ctx.link);
      let id = slug(stripTags(content));
      if (ctx.ids.has(id)) {
        let n = 1;
        while (ctx.ids.has(`${id}-${n}`)) n++;
        id = `${id}-${n}`;
      }
      ctx.ids.add(id);
      if (level === 1 && !ctx.title) ctx.title = stripTags(content);
      if (level === 2) ctx.toc.push({ id, text: stripTags(content) });
      const anchor = level > 1 ? `<a class="anchor" href="#${id}" aria-label="Link to this section">#</a>` : '';
      html.push(`<h${level} id="${id}">${content}${anchor}</h${level}>`);
      i++;
      continue;
    }

    if (RULE.test(line)) { html.push('<hr>'); i++; continue; }

    if (/^ {0,3}>/.test(line)) {
      const quote = [];
      while (i < lines.length && !blank(lines[i]) && (/^ {0,3}>/.test(lines[i]) || quote.length)) {
        quote.push(lines[i].replace(/^ {0,3}> ?/, ''));
        i++;
      }
      html.push(`<blockquote>${blocks(quote, ctx)}</blockquote>`);
      continue;
    }

    if (line.includes('|') && i + 1 < lines.length && TABLE_SEP.test(lines[i + 1]) && lines[i + 1].includes('-')) {
      const head = cells(line);
      const align = cells(lines[i + 1]).map((c) => (c.startsWith(':') && c.endsWith(':') ? 'center' : c.endsWith(':') ? 'right' : c.startsWith(':') ? 'left' : ''));
      const style = (n) => (align[n] ? ` style="text-align:${align[n]}"` : '');
      i += 2;
      const rows = [];
      while (i < lines.length && !blank(lines[i]) && lines[i].includes('|')) rows.push(cells(lines[i++]));
      const body = rows.map((r) => head.map((_, n) => inline(r[n] ?? '', ctx.link)));
      const cols = columns([head.map((c) => inline(c, ctx.link)), ...body]);
      const label = head.map((c) => inline(c, ctx.link).replace(/<[^>]*>/g, '').trim());
      const cell = (tag, n, c) => `<${tag}${cols[n].short ? ' class="nw"' : ''}${tag === 'td' ? ` data-label="${label[n]}"` : ''}${style(n)}>${c}</${tag}>`;
      html.push(
        `<div class="table"><table><colgroup>${cols.map((c) => `<col style="width:${c.width}%">`).join('')}</colgroup>` +
          `<thead><tr>${head.map((c, n) => cell('th', n, inline(c, ctx.link))).join('')}</tr></thead>` +
          `<tbody>${body.map((r) => `<tr>${r.map((c, n) => cell('td', n, c)).join('')}</tr>`).join('')}</tbody></table></div>`,
      );
      continue;
    }

    if ((m = ITEM.exec(line))) {
      const ordered = /\d/.test(m[2]);
      const start = ordered ? parseInt(m[2], 10) : 1;
      const items = [];
      let loose = false;
      while (i < lines.length) {
        const im = ITEM.exec(lines[i]);
        if (!im || /\d/.test(im[2]) !== ordered) break;
        const width = im[1].length + im[2].length + Math.min(Math.max(im[3].length, 1), 4);
        const body = [im[4]];
        i++;
        let sawBlank = false;
        while (i < lines.length) {
          const l = lines[i];
          if (blank(l)) { sawBlank = true; body.push(''); i++; continue; }
          if (indentOf(l) >= width) { body.push(l.replace(/\t/g, '    ').slice(width)); if (sawBlank) loose = true; sawBlank = false; i++; continue; }
          // A lazy continuation of the item's paragraph.
          if (!sawBlank && !startsBlock(l, lines[i + 1])) { body.push(l.trim()); i++; continue; }
          break;
        }
        while (body.length && blank(body[body.length - 1])) body.pop();
        items.push(body);
        if (sawBlank && i < lines.length && ITEM.test(lines[i])) loose = true;
        if (i < lines.length && blank(lines[i - 1] ?? '') && !ITEM.test(lines[i])) break;
      }
      const tag = ordered ? 'ol' : 'ul';
      const startAttr = ordered && start !== 1 ? ` start="${start}"` : '';
      const lis = items.map((body) => {
        let inner = blocks(body, ctx);
        if (!loose) inner = inner.replace(/^<p>([\s\S]*?)<\/p>/, '$1');
        return `<li>${inner}</li>`;
      });
      html.push(`<${tag}${startAttr}>${lis.join('')}</${tag}>`);
      continue;
    }

    const para = [];
    while (i < lines.length && !blank(lines[i]) && (para.length === 0 || !startsBlock(lines[i], lines[i + 1]))) {
      para.push(lines[i].trim());
      i++;
    }
    html.push(`<p>${inline(para.join('\n'), ctx.link)}</p>`);
  }
  return html.join('\n');
}

/** Renders one doc. Returns its title, its sections and its HTML. */
function render(md, names) {
  const ctx = { ids: new Set(), toc: [], title: '', link: (href, image) => rewrite(href, image, names) };
  const body = blocks(md.replace(/\r\n?/g, '\n').split('\n'), ctx);
  return { title: ctx.title, toc: ctx.toc, body };
}

/** Links between docs become links between pages; other files in the
 *  repository open on GitHub. */
function rewrite(href, image, names) {
  if (!href || href.startsWith('#') || /^[a-z][a-z0-9+.-]*:/i.test(href) || href.startsWith('//')) return href;
  const [p, hash = ''] = href.split(/(?=#)/);
  const resolved = path.posix.normalize(path.posix.join('docs', p));
  if (resolved.startsWith('..')) return href;
  const doc = /^docs\/([^/]+)\.md$/.exec(resolved);
  if (doc && names.includes(doc[1])) return `${doc[1]}.html${hash}`;
  // Pictures that are part of the site load from the site itself.
  if (image && resolved.startsWith('site/')) return `../${resolved.slice(5)}`;
  if (image) return `${RAW}/${resolved}`;
  const full = path.join(ROOT, resolved);
  const kind = fs.existsSync(full) && fs.statSync(full).isDirectory() ? 'tree' : 'blob';
  return `${REPO}/${kind}/main/${resolved}${hash}`;
}

/** The first paragraph, as plain text, for descriptions. */
function summary(body) {
  // The first paragraph with words in it: a picture alone does not count.
  const paras = [...body.matchAll(/<p>([\s\S]*?)<\/p>/g)].map((m) => stripTags(m[1]).replace(/\s+/g, ' ').trim());
  const text = paras.find(Boolean) || '';
  return text.length > 180 ? `${text.slice(0, 177).replace(/\s+\S*$/, '')}…` : text;
}

// ------------------------------------------------------------------ pages

// Both marks, as on the home page: CSS shows the one for the current style.
const MARK = '<svg class="mark-classic" viewBox="0 0 32 32" aria-hidden="true"><rect width="32" height="32" rx="8" fill="#4b55d6"/><g fill="none" stroke="#fff" stroke-width="3" stroke-linecap="round"><path d="M9.5 7v19"/><circle cx="16.6" cy="13.6" r="6.6"/></g><circle cx="16.6" cy="13.6" r="1.9" fill="#fff"/></svg>' +
  '<svg class="mark-studio" viewBox="0 0 32 32" aria-hidden="true"><rect width="32" height="32" rx="7" fill="#e5401f"/><rect x="7" y="6" width="5" height="20" fill="#1e1c19"/><circle cx="18" cy="13" r="7" fill="#f1b11b"/><path d="M18 6a7 7 0 0 1 0 14z" fill="#1d4fc4"/></svg>';
const STYLE_ICON = '<svg viewBox="0 0 22 14" aria-hidden="true"><circle cx="5" cy="7" r="5" fill="#e5401f"/><path d="M8 14a5 5 0 0 1 10 0z" fill="#f1b11b"/><circle cx="17" cy="7" r="5" fill="#1d4fc4"/></svg>';
const GITHUB = '<svg viewBox="0 0 16 16" fill="currentColor" aria-hidden="true"><path d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27.68 0 1.36.09 2 .27 1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.013 8.013 0 0016 8c0-4.42-3.58-8-8-8z"/></svg>';

// Applies the saved theme and style before the page paints, then wires the
// switches. Both are shared with the home page (the same localStorage keys).
const THEME_HEAD = `<script>try{const r=document.documentElement,t=localStorage.getItem('plonix-theme');if(t==='light'||t==='dark')r.dataset.theme=t;if(localStorage.getItem('plonix-style')==='classic')r.removeAttribute('data-style')}catch(e){}</script>`;
const THEME_SCRIPT = `<script>
(() => {
  const root = document.documentElement, btn = document.getElementById('theme');
  const sys = matchMedia('(prefers-color-scheme: dark)');
  const SUN = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true"><circle cx="12" cy="12" r="4.2"/><path d="M12 2.5v2.2M12 19.3v2.2M4.6 4.6l1.6 1.6M17.8 17.8l1.6 1.6M2.5 12h2.2M19.3 12h2.2M4.6 19.4l1.6-1.6M17.8 6.2l1.6-1.6"/></svg>';
  const MOON = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M20 14.5A8 8 0 0 1 9.5 4a8 8 0 1 0 10.5 10.5z"/></svg>';
  const current = () => root.dataset.theme || (sys.matches ? 'dark' : 'light');
  const paint = () => { const d = current() === 'dark'; btn.innerHTML = d ? SUN : MOON; btn.setAttribute('aria-label', d ? 'Switch to light mode' : 'Switch to dark mode'); };
  paint();
  sys.addEventListener('change', paint);
  btn.addEventListener('click', () => {
    const next = current() === 'dark' ? 'light' : 'dark';
    root.dataset.theme = next; paint();
    try { localStorage.setItem('plonix-theme', next); } catch (e) {}
  });

  // On a phone the list of docs starts folded, so the page comes first.
  const menu = document.querySelector('.side-menu');
  if (menu && matchMedia('(max-width: 900px)').matches) menu.open = false;

  // Studio is the default; Classic is the plainer look. Screenshots and the icon follow.
  const sbtn = document.getElementById('style');
  const shots = [...document.querySelectorAll('.doc img')].filter((i) => i.getAttribute('src').startsWith('../assets/studio-'));
  shots.forEach((i) => { i.dataset.studio = i.getAttribute('src'); });
  const setStyle = (on) => {
    if (on) root.dataset.style = 'studio'; else delete root.dataset.style;
    sbtn.setAttribute('aria-pressed', on ? 'true' : 'false');
    sbtn.querySelector('span').textContent = on ? 'Try classic' : 'I like style';
    sbtn.title = on ? 'Switch to the Classic look' : 'Back to the Studio look';
    shots.forEach((i) => { const src = on ? i.dataset.studio : i.dataset.studio.replace('/assets/studio-', '/assets/'); if (i.getAttribute('src') !== src) i.src = src; });
    document.getElementById('favicon').href = on ? '../assets/studio-mark.svg?v=2' : '../assets/plonix-mark.svg?v=2';
  };
  setStyle(root.dataset.style === 'studio');
  sbtn.addEventListener('click', () => {
    const on = root.dataset.style !== 'studio';
    setStyle(on);
    try { localStorage.setItem('plonix-style', on ? 'studio' : 'classic'); } catch (e) {}
  });
})();
</script>`;

function page({ file, title, description, sidebar, main, source }) {
  const canonical = file === 'index.html' ? 'https://plonix.io/docs/' : `https://plonix.io/docs/${file.replace(/\.html$/, '')}`;
  const generated = source ? `Generated from ${source} by scripts/build-docs.mjs. Edit the Markdown, not this file.` : 'Generated by scripts/build-docs.mjs. Do not edit.';
  return `<!doctype html>
<!-- ${generated} -->
<html lang="en" data-style="studio">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">
<title>${esc(title)}</title>
<meta name="description" content="${esc(description)}">
<meta property="og:title" content="${esc(title)}">
<meta property="og:description" content="${esc(description)}">
<meta property="og:url" content="${canonical}">
<meta property="og:image" content="https://plonix.io/assets/studio-window-light.webp">
<link rel="canonical" href="${canonical}">
<meta name="theme-color" content="#ece5d6" media="(prefers-color-scheme: light)">
<meta name="theme-color" content="#13120f" media="(prefers-color-scheme: dark)">
<link rel="icon" id="favicon" href="../assets/studio-mark.svg?v=2" type="image/svg+xml">
<link rel="preconnect" href="https://fonts.googleapis.com">
<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
<link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=Bricolage+Grotesque:opsz,wght@12..96,400;12..96,600;12..96,800&family=Hanken+Grotesk:wght@400;500;600&family=JetBrains+Mono:wght@400;500&family=Jost:wght@400..700&display=swap">
<link rel="stylesheet" href="docs.css?v=${CSS_VERSION}">
${THEME_HEAD}
</head>
<body>
<header class="nav">
  <div class="wrap">
    <a class="brand" href="../" aria-label="Plonix home">${MARK}Plonix</a>
    <nav class="nav-links" aria-label="Site">
      <a href="../">Home</a>
      <a href="./" aria-current="page">Docs</a>
      <a href="../analytics">Analytics</a>
      <a href="privacy.html">Privacy</a>
    </nav>
    <button type="button" class="theme-btn" id="theme" aria-label="Switch theme" title="Switch theme"></button>
    <button type="button" class="style-btn" id="style" aria-pressed="true">${STYLE_ICON}<span>Try classic</span></button>
    <a class="btn small" href="${REPO}">${GITHUB}GitHub</a>
  </div>
</header>
<div class="wrap docs">
<aside class="side">
<details class="side-menu" open>
<summary>Documentation</summary>
${sidebar}
</details>
</aside>
<main class="doc">
${main}
</main>
</div>
<footer>
  <div class="wrap">
    <a class="brand" href="../" aria-label="Plonix home">${MARK}Plonix</a>
    <span>Open-source, AI-first web security workbench. Apache-2.0.</span>
    <nav aria-label="Footer">
      <a href="${REPO}">GitHub</a>
      <a href="./">Docs</a>
      <a href="${REPO}/releases">Releases</a>
      <a href="${REPO}/issues">Issues</a>
      <a href="privacy.html">Privacy</a>
    </nav>
  </div>
</footer>
${THEME_SCRIPT}
</body>
</html>
`;
}

/** The docs in their sidebar sections: [[heading, docs], …]. */
function grouped(docs) {
  const sections = GROUPS.map(([heading, names]) => [heading, names.map((n) => docs.find((d) => d.name === n)).filter(Boolean)]);
  const rest = docs.filter((d) => !ORDER.includes(d.name));
  if (rest.length) sections.push(['More', rest]);
  return sections.filter(([, list]) => list.length);
}

function sidebar(docs, current) {
  const item = (d) => {
    const here = d.name === current;
    const toc = here && d.toc.length ? `<ul class="toc">${d.toc.map((t) => `<li><a href="#${t.id}">${esc(t.text)}</a></li>`).join('')}</ul>` : '';
    return `<li${here ? ' class="here"' : ''}><a href="${d.name}.html"${here ? ' aria-current="page"' : ''}>${esc(d.title)}</a>${toc}</li>`;
  };
  const overview = current === null ? ' class="here"' : '';
  const sections = grouped(docs).map(([heading, list]) => `<li class="side-head">${esc(heading)}</li>${list.map(item).join('')}`);
  return `<nav aria-label="Documentation"><ul class="side-list"><li${overview}><a href="./"${current === null ? ' aria-current="page"' : ''}>Overview</a></li>${sections.join('')}</ul></nav>`;
}

const CSS = `/* Generated by scripts/build-docs.mjs. Do not edit. The tokens match site/index.html: Studio is the default style, Classic the alternative. */
:root {
  --ink: #f5f5fa; --ink-2: #ffffff; --ink-3: #eceef6; --line: #e0e2ee;
  --fg: #15162b; --muted: #565a78; --faint: #8b8ea8;
  --indigo: #4b55d6; --indigo-deep: #4b55d6; --amber: #f7c64f;
  --code-bg: #eef0f7; --code-fg: #2f3352; --pre-bg: #13141f; --pre-fg: #dfe1f0;
  --display: "Bricolage Grotesque", "Avenir Next", "Segoe UI", system-ui, sans-serif;
  --body: "Hanken Grotesk", -apple-system, "Helvetica Neue", system-ui, sans-serif;
  --mono: "JetBrains Mono", ui-monospace, "SF Mono", Menlo, monospace;
  --wrap: 1180px;
  color-scheme: light;
}
@media (prefers-color-scheme: dark) {
  :root:not([data-theme="light"]) {
    --ink: #0d0e13; --ink-2: #15161e; --ink-3: #1d1f2a; --line: #292b39;
    --fg: #ecedf4; --muted: #a2a5bb; --faint: #6f7289;
    --indigo: #8b93ff; --indigo-deep: #4b55d6; --amber: #f5bd55;
    --code-bg: #1d1f2a; --code-fg: #d6d8ea; --pre-bg: #08090d; --pre-fg: #d6d8ea;
    color-scheme: dark;
  }
}
:root[data-theme="dark"] {
    --ink: #0d0e13; --ink-2: #15161e; --ink-3: #1d1f2a; --line: #292b39;
    --fg: #ecedf4; --muted: #a2a5bb; --faint: #6f7289;
    --indigo: #8b93ff; --indigo-deep: #4b55d6; --amber: #f5bd55;
    --code-bg: #1d1f2a; --code-fg: #d6d8ea; --pre-bg: #08090d; --pre-fg: #d6d8ea;
    color-scheme: dark;
}
* { box-sizing: border-box; }
html { scroll-behavior: smooth; scroll-padding-top: 84px; }
@media (prefers-reduced-motion: reduce) { html { scroll-behavior: auto; } }
body { margin: 0; background: var(--ink); color: var(--fg); font: 400 17px/1.65 var(--body); -webkit-font-smoothing: antialiased; overflow-x: hidden; }
a { color: inherit; }
:focus-visible { outline: 2px solid var(--indigo); outline-offset: 3px; border-radius: 6px; }
.wrap { max-width: var(--wrap); margin: 0 auto; padding-inline: 24px; }
@media (max-width: 520px) { .wrap { padding-inline: 16px; } }

/* nav, as on the home page */
.nav { position: sticky; top: env(safe-area-inset-top, 0px); z-index: 20; background: color-mix(in srgb, var(--ink) 82%, transparent);
  backdrop-filter: blur(14px) saturate(140%); -webkit-backdrop-filter: blur(14px) saturate(140%);
  border-bottom: 1px solid color-mix(in srgb, var(--line) 60%, transparent); }
.nav .wrap { display: flex; align-items: center; gap: 28px; height: 62px; }
.brand { display: flex; align-items: center; gap: 10px; text-decoration: none; font: 800 1.2rem var(--display); letter-spacing: -0.02em; }
.brand svg { width: 28px; height: 28px; }
.nav-links { display: flex; gap: 22px; margin-left: auto; font-size: 0.95rem; color: var(--muted); }
.nav-links a { text-decoration: none; }
.nav-links a:hover, .nav-links a[aria-current] { color: var(--fg); }
.btn { display: inline-flex; align-items: center; gap: 9px; height: 36px; padding-inline: 14px; border-radius: 10px; font: 600 0.9rem var(--body);
  text-decoration: none; border: 1px solid var(--line); background: var(--ink-2); color: var(--fg); transition: transform .15s ease, border-color .15s ease; }
.btn:hover { border-color: var(--indigo); transform: translateY(-1px); }
.btn svg { width: 18px; height: 18px; flex: none; }
.theme-btn { width: 38px; height: 36px; border-radius: 10px; border: 1px solid var(--line); background: var(--ink-2); color: var(--fg); display: grid; place-items: center; cursor: pointer; margin-left: 4px; }
.theme-btn:hover { border-color: var(--indigo); }
.theme-btn svg { width: 18px; height: 18px; }
.style-btn { height: 36px; border-radius: 999px; border: 1px solid var(--line); background: var(--ink-2); color: var(--fg); display: inline-flex; align-items: center; gap: 8px; cursor: pointer; margin-left: 4px; padding: 0 14px 0 10px; font: 600 .9rem var(--body); white-space: nowrap; }
.style-btn:hover { border-color: var(--fg); }
.style-btn svg { width: 22px; height: 14px; flex: none; }
.brand .mark-studio { display: none; }
/* phones: the links take a row of their own under the logo, scrolling sideways if they ever outgrow it */
@media (max-width: 760px) {
  .nav .wrap { flex-wrap: wrap; height: auto; column-gap: 14px; row-gap: 0; padding-top: 13px; }
  .nav .theme-btn { margin-left: auto; }
  .nav-links { order: 1; flex: 1 0 100%; margin-left: 0; height: 42px; align-items: center; gap: 22px; overflow-x: auto; scrollbar-width: none; white-space: nowrap; }
  .nav-links::-webkit-scrollbar { display: none; }
  .nav-links a[aria-current] { text-decoration: underline 2px var(--indigo); text-underline-offset: 7px; }
  [data-style="studio"] .nav-links a[aria-current] { text-decoration-color: var(--st-red); }
  html { scroll-padding-top: 120px; }
  .style-btn span { display: none; } .style-btn { padding: 0 9px; }
}
@media (max-width: 420px) { .nav .btn.small { display: none; } }

/* layout: a sidebar of docs beside the page */
.docs { display: grid; grid-template-columns: 230px minmax(0, 1fr); gap: 56px; padding-top: 44px; }
.side-menu > summary { display: none; }
.side nav { padding-bottom: 24px; }
.side-list, .toc { list-style: none; margin: 0; padding: 0; }
.side-list > .side-head { margin: 18px 0 4px; padding: 0 12px; font-size: .74rem; font-weight: 600; letter-spacing: .08em; text-transform: uppercase; color: var(--faint); }
.side-list > li > a { display: block; padding: 6px 12px; border-radius: 9px; text-decoration: none; color: var(--muted); font-size: .95rem; }
.side-list > li > a:hover { color: var(--fg); background: var(--ink-3); }
.side-list > li.here > a { color: var(--fg); background: var(--ink-2); box-shadow: inset 0 0 0 1px var(--line); font-weight: 600; }
.toc { margin: 4px 0 10px 12px; border-left: 1px solid var(--line); }
.toc a { display: block; padding: 3px 12px; text-decoration: none; color: var(--faint); font-size: .86rem; line-height: 1.4; }
.toc a:hover { color: var(--indigo); }
@media (max-width: 900px) {
  .docs { grid-template-columns: minmax(0, 1fr); gap: 20px; padding-top: 20px; }
  .side-menu { border: 1px solid var(--line); border-radius: 12px; background: var(--ink-2); }
  .side-menu > summary { display: block; cursor: pointer; padding: 10px 14px; font: 600 .95rem var(--body); }
  .side-menu:not([open]) > summary { color: var(--muted); }
  .side nav { padding: 0 8px 10px; }
}

/* the page */
.doc { min-width: 0; max-width: 780px; padding-bottom: 40px; }
.doc h1, .doc h2, .doc h3, .doc h4 { font-family: var(--display); letter-spacing: -0.02em; line-height: 1.15; text-wrap: balance; position: relative; }
.doc h1 { font-size: clamp(2.1rem, 4.4vw, 3rem); font-weight: 800; margin: 0 0 20px; }
.doc h2 { font-size: 1.7rem; font-weight: 800; margin: 52px 0 14px; padding-top: 22px; border-top: 1px solid var(--line); }
.doc h3 { font-size: 1.22rem; font-weight: 600; letter-spacing: -0.01em; margin: 34px 0 10px; }
.doc h4 { font-size: 1.05rem; font-weight: 600; margin: 26px 0 8px; }
.doc .anchor { margin-left: .4em; color: var(--faint); text-decoration: none; opacity: 0; font-weight: 400; }
.doc h2:hover .anchor, .doc h3:hover .anchor, .doc h4:hover .anchor, .doc .anchor:focus { opacity: 1; }
.doc p, .doc ul, .doc ol, .doc blockquote, .doc pre, .doc .table { margin: 0 0 16px; }
.doc ul, .doc ol { padding-left: 1.4em; }
.doc li { margin: 4px 0; }
.doc li > ul, .doc li > ol { margin: 4px 0 0; }
.doc li::marker { color: var(--faint); }
.doc a { color: var(--indigo); text-decoration: underline; text-decoration-color: color-mix(in srgb, var(--indigo) 35%, transparent); text-underline-offset: 3px; }
.doc a:hover { text-decoration-color: currentColor; }
.doc strong { font-weight: 600; }
.doc code { font-family: var(--mono); font-size: .86em; background: var(--code-bg); color: var(--code-fg); padding: .12em .38em; border-radius: 6px; overflow-wrap: anywhere; }
.doc pre { background: var(--pre-bg); color: var(--pre-fg); border-radius: 12px; padding: 16px 18px; overflow-x: auto; border: 1px solid var(--line); }
.doc pre code { background: none; color: inherit; padding: 0; font-size: .84rem; line-height: 1.6; overflow-wrap: normal; white-space: pre; }
.doc blockquote { margin-left: 0; padding: 4px 18px; border-left: 3px solid var(--indigo); color: var(--muted); background: var(--ink-2); border-radius: 0 10px 10px 0; }
.doc blockquote p { margin: 10px 0; }
.doc hr { border: 0; border-top: 1px solid var(--line); margin: 36px 0; }
.doc img { max-width: 100%; height: auto; border-radius: 10px; }
.doc img.pic-light, .doc img.pic-dark { display: block; border: 1px solid var(--line); margin: 6px 0 16px; }
.doc img.pic-dark { display: none; }
@media (prefers-color-scheme: dark) {
  :root:not([data-theme="light"]) .doc img.pic-light { display: none; }
  :root:not([data-theme="light"]) .doc img.pic-dark { display: block; }
}
:root[data-theme="dark"] .doc img.pic-light { display: none; }
:root[data-theme="dark"] .doc img.pic-dark { display: block; }
.doc .table { overflow-x: auto; border: 1px solid var(--line); border-radius: 12px; background: var(--ink-2); }
.doc table { border-collapse: collapse; width: 100%; font-size: .93rem; }
.doc th, .doc td { text-align: left; padding: 9px 14px; border-bottom: 1px solid var(--line); vertical-align: top; overflow-wrap: normal; }
.doc td code, .doc th code { overflow-wrap: normal; }
@media (min-width: 641px) { .doc th.nw, .doc td.nw { white-space: nowrap; } }
@media (max-width: 640px) {
  .doc table, .doc tbody, .doc tr, .doc td { display: block; width: auto !important; }
  .doc colgroup, .doc thead { display: none; }
  .doc tr { padding: 10px 16px; border-bottom: 1px solid var(--line); }
  .doc tr:last-child { border-bottom: 0; }
  .doc td { padding: 3px 0; border: 0; overflow-wrap: anywhere; text-align: left !important; }
  .doc td::before { content: attr(data-label); display: block; font-size: .78rem; font-weight: 600; color: var(--faint); }
  .doc td:first-child { font-weight: 600; }
  .doc td:first-child::before { display: none; }
  .doc td:empty { display: none; }
  .doc td code { overflow-wrap: anywhere; }
}
.doc th { font-weight: 600; background: var(--ink-3); }
.doc tr:last-child td { border-bottom: 0; }
.doc .edit { margin-top: 48px; padding-top: 18px; border-top: 1px solid var(--line); font-size: .9rem; color: var(--faint); }
.doc .edit a { color: var(--muted); }
.lede { color: var(--muted); font-size: 1.12rem; max-width: 62ch; }
.cards { list-style: none; padding: 0 !important; display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 14px; margin-top: 28px !important; }
.cards li { margin: 0; }
.cards a { display: grid; gap: 6px; height: 100%; padding: 16px 18px; border: 1px solid var(--line); border-radius: 14px; background: var(--ink-2); text-decoration: none; color: var(--fg); transition: border-color .15s ease, transform .15s ease; }
.cards a:hover { border-color: var(--indigo); transform: translateY(-2px); }
.cards b { font: 600 1.08rem var(--display); letter-spacing: -0.01em; }
.cards span { color: var(--muted); font-size: .9rem; line-height: 1.5; }
@media (max-width: 640px) { .cards { grid-template-columns: minmax(0, 1fr); } }

/* ---------- Studio: flat red, yellow, blue and green shapes on cream paper, round controls, Jost ---------- */
:root[data-style="studio"] {
  --st-red: #e5401f; --st-blue: #1d4fc4; --st-yellow: #f1b11b; --st-green: #16875a; --st-on: #1e1c19;
  --ink: #ece5d6; --ink-2: #faf6ee; --ink-3: #e4dccb; --line: #d6ccb8; --fg: #1e1c19; --muted: #4f493f; --faint: #6f675a;
  --indigo: #1d4fc4; --indigo-deep: #1d4fc4; --amber: #f1b11b;
  --code-bg: #e4dccb; --code-fg: #3d3830; --pre-bg: #1e1c19; --pre-fg: #f1eadd;
  --display: "Jost", "Avenir Next", system-ui, sans-serif; --body: "Jost", -apple-system, "Helvetica Neue", system-ui, sans-serif;
}
@media (prefers-color-scheme: dark) {
  :root[data-style="studio"]:not([data-theme="light"]) {
    --st-red: #ff5a36; --st-blue: #5a86ff; --st-yellow: #ffc533; --st-green: #3dbb83;
    --ink: #13120f; --ink-2: #1c1a17; --ink-3: #25221e; --line: #36312a; --fg: #f1eadd; --muted: #c0b7a6; --faint: #8d8576;
    --indigo: #7da0ff; --indigo-deep: #1d4fc4; --amber: #ffc533;
    --code-bg: #25221e; --code-fg: #ddd5c4; --pre-bg: #0c0b09; --pre-fg: #e6dfd0;
  }
}
:root[data-style="studio"][data-theme="dark"] {
  --st-red: #ff5a36; --st-blue: #5a86ff; --st-yellow: #ffc533; --st-green: #3dbb83;
  --ink: #13120f; --ink-2: #1c1a17; --ink-3: #25221e; --line: #36312a; --fg: #f1eadd; --muted: #c0b7a6; --faint: #8d8576;
  --indigo: #7da0ff; --indigo-deep: #1d4fc4; --amber: #ffc533;
  --code-bg: #25221e; --code-fg: #ddd5c4; --pre-bg: #0c0b09; --pre-fg: #e6dfd0;
}
[data-style="studio"] body { font-size: 18px; }
[data-style="studio"] .brand svg.mark-classic { display: none; }
[data-style="studio"] .brand .mark-studio { display: block; }
[data-style="studio"] .style-btn { background: var(--st-yellow); border-color: var(--st-yellow); color: var(--st-on); }
[data-style="studio"] .style-btn path { fill: var(--st-on); }
[data-style="studio"] .btn, [data-style="studio"] .theme-btn { border-radius: 999px; }
[data-style="studio"] .btn:hover, [data-style="studio"] .theme-btn:hover { border-color: var(--fg); }
[data-style="studio"] .side-list > li > a { border-radius: 999px; }
[data-style="studio"] .side-list > li.here > a { background: var(--st-yellow); color: var(--st-on); box-shadow: none; }
[data-style="studio"] .side-list > .side-head { color: var(--muted); }
[data-style="studio"] .toc { border-left: 2px solid var(--st-yellow); }
[data-style="studio"] .toc a:hover { color: var(--st-red); }
[data-style="studio"] .side-menu { border-radius: 22px; }
[data-style="studio"] .doc h1, [data-style="studio"] .doc h2, [data-style="studio"] .doc h3 { font-weight: 700; }
[data-style="studio"] .doc h1 { font-size: clamp(2.4rem, 5vw, 3.6rem); line-height: 1.02; }
[data-style="studio"] .doc h1::before { content: ""; display: block; width: 12px; height: 12px; margin: 6px 0 22px; border-radius: 50%; background: var(--st-red); box-shadow: 18px 0 0 var(--st-yellow), 36px 0 0 var(--st-blue), 54px 0 0 var(--st-green); }
[data-style="studio"] .doc h2 { border-top: 0; padding-top: 0; font-size: 1.85rem; }
[data-style="studio"] .doc h2::before { content: ""; display: inline-block; width: .55em; height: .55em; margin-right: .45em; border-radius: 50%; background: var(--band, var(--st-red)); vertical-align: .08em; }
[data-style="studio"] .doc h2:nth-of-type(4n+2) { --band: var(--st-yellow); }
[data-style="studio"] .doc h2:nth-of-type(4n+3) { --band: var(--st-blue); }
[data-style="studio"] .doc h2:nth-of-type(4n+4) { --band: var(--st-green); }
[data-style="studio"] .doc .anchor { color: var(--st-red); }
[data-style="studio"] .doc li::marker { color: var(--st-red); }
[data-style="studio"] .doc code { border-radius: 8px; }
[data-style="studio"] .doc pre { border-radius: 20px; border: 0; padding: 18px 22px; }
[data-style="studio"] .doc blockquote { border-left: 0; border-radius: 20px; padding: 6px 22px 6px 52px; position: relative; color: var(--fg); }
[data-style="studio"] .doc blockquote::before { content: ""; position: absolute; left: 20px; top: 20px; width: 14px; height: 14px; border-radius: 50%; background: var(--st-blue); }
[data-style="studio"] .doc .table { border: 0; border-radius: 20px; }
[data-style="studio"] .doc th { background: var(--ink-3); }
[data-style="studio"] .doc img { border-radius: 18px; }
[data-style="studio"] .doc img.pic-light, [data-style="studio"] .doc img.pic-dark { border: 0; box-shadow: 0 24px 50px -30px #3a2e1a88; }
[data-style="studio"] .lede { color: var(--muted); }
[data-style="studio"] .cards a { border: 0; border-radius: 22px; padding: 18px 20px; }
[data-style="studio"] .cards a:hover { transform: translateY(-2px); box-shadow: inset 0 0 0 2px var(--fg); }
[data-style="studio"] .cards b::before { content: ""; display: inline-block; width: .62em; height: .62em; margin-right: .45em; border-radius: 50%; background: var(--band, var(--st-red)); }
[data-style="studio"] .cards li:nth-child(4n+2) { --band: var(--st-yellow); }
[data-style="studio"] .cards li:nth-child(4n+3) { --band: var(--st-blue); }
[data-style="studio"] .cards li:nth-child(4n+4) { --band: var(--st-green); }
[data-style="studio"] .doc .edit { border-top: 0; }
[data-style="studio"] footer { border-top: 0; background: var(--ink-3); }

footer { border-top: 1px solid var(--line); margin-top: 80px; padding-block: 40px 56px; color: var(--faint); font-size: .9rem; }
footer .wrap { display: flex; flex-wrap: wrap; gap: 18px 32px; align-items: center; }
footer nav { display: flex; flex-wrap: wrap; gap: 22px; margin-left: auto; }
footer a { text-decoration: none; color: var(--muted); }
footer a:hover { color: var(--fg); }
`;

// Site screenshots load by their content hash too, so a new picture shows up
// at once instead of after the cache runs out.
function assetVersion(href) {
  const file = path.join(ROOT, 'site', href.replace(/^\.\.\//, ''));
  if (!fs.existsSync(file)) return '';
  return '?v=' + crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex').slice(0, 10);
}

// Pages ask for docs.css by its content hash, so a browser never keeps an old
// stylesheet under new pages.
const CSS_VERSION = crypto.createHash('sha256').update(CSS).digest('hex').slice(0, 10);

// ------------------------------------------------------------------- main

function build() {
  const files = fs.readdirSync(DOCS).filter((f) => f.endsWith('.md'));
  const rank = (n) => (ORDER.includes(n) ? ORDER.indexOf(n) : ORDER.length);
  const names = files.map((f) => f.slice(0, -3)).sort((a, b) => rank(a) - rank(b) || a.localeCompare(b));
  const docs = names.map((name) => {
    const md = fs.readFileSync(path.join(DOCS, `${name}.md`), 'utf8');
    const r = render(md, names);
    return { name, ...r, title: r.title || name, description: summary(r.body) };
  });
  const out = new Map();
  out.set('docs.css', CSS);
  for (const d of docs) {
    const source = `docs/${d.name}.md`;
    const main = `<article>\n${d.body}\n</article>\n<p class="edit">This page is <a href="${REPO}/blob/main/${source}">${source}</a> in the Plonix repository. Spotted something wrong? <a href="${REPO}/edit/main/${source}">Suggest a change</a>.</p>`;
    out.set(`${d.name}.html`, page({ file: `${d.name}.html`, title: `${d.title} · Plonix docs`, description: d.description, sidebar: sidebar(docs, d.name), main, source }));
  }
  const card = (d) => `<li><a href="${d.name}.html"><b>${esc(d.title)}</b><span>${esc(d.description)}</span></a></li>`;
  const sections = grouped(docs).map(([heading, list]) => `<h2 id="${slug(heading)}">${esc(heading)}</h2>\n<ul class="cards">\n${list.map(card).join('\n')}\n</ul>`).join('\n');
  const overview = `<h1>Plonix documentation</h1>
<p class="lede">How to use Plonix, tool by tool: capture traffic, explore it, experiment on the Bench, prove what you find, and work with Claude. Plus how Plonix keeps your data and updates on your terms.</p>
<p>New to Plonix? Start with <a href="getting-started.html">Getting started</a>, or open the demo project from the Start screen and take the tour. The <code>plonix</code> command explains itself too: run <code>plonix --help</code>.</p>
${sections}`;
  out.set('index.html', page({ file: 'index.html', title: 'Plonix docs', description: 'Documentation for Plonix, the open-source, AI-first web security workbench for a new era of web assessments.', sidebar: sidebar(docs, null), main: overview }));
  return out;
}

const check = process.argv.includes('--check');
const pages = build();
const stale = [];
const existing = fs.existsSync(OUT) ? fs.readdirSync(OUT) : [];
for (const [name, content] of pages) {
  const file = path.join(OUT, name);
  const current = fs.existsSync(file) ? fs.readFileSync(file, 'utf8') : null;
  if (current === content) continue;
  stale.push(name);
  if (!check) {
    fs.mkdirSync(OUT, { recursive: true });
    fs.writeFileSync(file, content);
  }
}
for (const name of existing.filter((n) => !pages.has(n))) {
  stale.push(`${name} (no longer generated)`);
  if (!check) fs.rmSync(path.join(OUT, name));
}
if (check) {
  if (stale.length) {
    console.error(`site/docs is out of date with docs/: ${stale.join(', ')}\nRun: node scripts/build-docs.mjs`);
    process.exit(1);
  }
  console.log(`site/docs is up to date (${pages.size} files).`);
} else {
  console.log(stale.length ? `Wrote ${stale.join(', ')}` : 'Nothing to change.');
}
