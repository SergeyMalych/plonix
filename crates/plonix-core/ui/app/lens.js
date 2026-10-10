// Plonix window: Mind Reader suggestions, leads into Scans, detector packs, Claude-written findings, copy as curl.
// Part of the project window's scripts (see index.html for the load order).
'use strict';

/* ======================================================================
   Mind Reader — context-aware quick actions
   Each reader looks at one exchange and works out the single most useful
   next move for what the researcher is looking at, or returns nothing. The
   chips they drive only SUGGEST: nothing is sent until a click, and anything
   that sends is scope-gated by the engine. Surfaced in the Lens "Suggested"
   row and the Traffic row menu.
   ====================================================================== */

const REDIRECT_PARAMS = /^(url|uri|next|returnurl|return_to|return|redirect_uri|redirect_url|redirect|dest|destination|continue|goto|forward|callback|rurl)$/i;
const LOGIN_PATH = /log-?in|sign-?in|sign-?on|\/auth|session|token|oauth|sso/i;
const SESSION_COOKIE = /^(sess|sid|session|auth|token|jwt|connect\.sid|jsessionid|phpsessid|asp\.net|_session)/i;

/** The name=value pairs of a request's query string. */
function queryPairsOf(ex) {
  const out = [];
  const q = ex.query || (ex.url && ex.url.includes('?') ? ex.url.split('?')[1] : '') || '';
  for (const part of q.split('&')) {
    if (!part) continue;
    const i = part.indexOf('=');
    const dec = (s) => { try { return decodeURIComponent(s.replace(/\+/g, ' ')); } catch (_) { return s; } };
    out.push([dec(i < 0 ? part : part.slice(0, i)), i < 0 ? '' : dec(part.slice(i + 1))]);
  }
  return out;
}

/** An id-shaped value in the path or query — the "could I read someone else's?" smell. */
function idTargetOf(ex) {
  for (const s of (ex.path || '').split('/').filter(Boolean)) {
    if (/^\d{1,15}$/.test(s)) return { kind: 'number', value: s, where: 'the path' };
    if (/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(s)) return { kind: 'uuid', value: s, where: 'the path' };
  }
  for (const [k, v] of queryPairsOf(ex)) {
    if (/(^id$|_id$|^uid$|^uuid$|guid)/i.test(k) && v) return { kind: 'param', value: v, where: `the "${k}" parameter`, name: k };
  }
  return null;
}

/** Request values that come straight back in an HTML/text response, verbatim. */
function reflectedValues(ex) {
  const body = ex.resp_text;
  if (!body || body.length > 2_000_000) return [];
  if (!/html|xml|text\/plain/i.test(header(ex.resp_headers, 'content-type') || '')) return [];
  const hits = [];
  const seen = new Set();
  const consider = (name, value, where) => {
    const v = (value || '').trim();
    if (v.length < 5 || v.length > 200 || seen.has(v)) return;
    if (/^[\d.\s,-]+$/.test(v)) return; // bare numbers reflect everywhere
    if (body.includes(v)) { hits.push({ name, value: v, where }); seen.add(v); }
  };
  for (const [k, v] of queryPairsOf(ex)) consider(k, v, 'query');
  const rb = ex.req_text || '';
  if (rb.length < 100_000 && /[=&]/.test(rb) && !/^[[{]/.test(rb.trim())) {
    for (const part of rb.split('&')) {
      const i = part.indexOf('=');
      if (i <= 0) continue;
      const dec = (s) => { try { return decodeURIComponent(s.replace(/\+/g, ' ')); } catch (_) { return s; } };
      consider(dec(part.slice(0, i)), dec(part.slice(i + 1)), 'body');
    }
  }
  return hits.slice(0, 3);
}

/** A permissive cross-origin policy on the response. */
function corsIssue(ex) {
  const acao = (header(ex.resp_headers, 'access-control-allow-origin') || '').trim();
  if (!acao) return null;
  const creds = /true/i.test(header(ex.resp_headers, 'access-control-allow-credentials') || '');
  const origin = (header(ex.req_headers, 'origin') || '').trim();
  // The site's own front end, allowed by name, is how most APIs work — not an echo worth a finding.
  const site = (host) => (host || '').toLowerCase().split('.').slice(-2).join('.');
  let originHost = '';
  try {
    originHost = new URL(origin).hostname;
  } catch (_) {}
  if (acao === '*' && creds) return { severity: 'medium', note: 'The response sets Access-Control-Allow-Origin to * while allowing credentials, so any site could read it on behalf of a signed-in user.' };
  if (origin && acao === origin && creds && site(originHost) !== site(ex.host)) return { severity: 'medium', note: `The response echoes the request Origin (${origin}) into Access-Control-Allow-Origin with credentials allowed, so an attacker-chosen origin may be trusted.` };
  if (acao === '*') return { severity: 'low', note: 'The response sets Access-Control-Allow-Origin to *, so any site can read it.' };
  return null;
}

/** A parameter that carries a URL or path the server might follow. */
function redirectParam(ex) {
  for (const [k, raw] of queryPairsOf(ex)) {
    if (!REDIRECT_PARAMS.test(k)) continue;
    // Apps often encode the target twice (next=%252Faccount), so peel off a layer or two.
    let v = raw;
    for (let i = 0; i < 2 && /%[0-9a-f]{2}/i.test(v); i++) {
      try {
        v = decodeURIComponent(v);
      } catch (_) {
        break;
      }
    }
    if (/^(https?:\/\/|\/\/|\/)[^\s]/i.test(v)) return { name: k, value: v };
  }
  return null;
}

/** Whether this request looks like GraphQL. */
function isGraphql(ex) {
  if (/\/graphql\b|\/gql\b/i.test(ex.path || '')) return true;
  if (/application\/graphql/i.test(header(ex.req_headers, 'content-type') || '')) return true;
  const rb = ex.req_text || '';
  return /"query"\s*:/.test(rb) && /\b(query|mutation|subscription)\b/.test(rb);
}

/** A login/session handed back in a response — the makings of a saved user. */
function sessionGrant(ex) {
  const sc = (ex.resp_headers || []).filter(([k]) => /^set-cookie$/i.test(k)).map(([, v]) => v);
  if (!sc.length) return null;
  const sessiony = sc.some((v) => SESSION_COOKIE.test(v));
  if (!LOGIN_PATH.test(ex.path || '') && !sessiony) return null;
  const pairs = sc.map((v) => v.split(';')[0].trim()).filter(Boolean);
  if (!pairs.length) return null;
  return { cookie: pairs.join('; '), count: pairs.length };
}

/** The saved user an exchange was sent as, from its notes, or null. */
const SENT_AS = 'sent as saved user: ';
const sentAs = (ex) => ((ex.replaced || []).find((r) => r.startsWith(SENT_AS)) || '').slice(SENT_AS.length) || null;
const ruleNotes = (ex) => (ex.replaced || []).filter((r) => !r.startsWith(SENT_AS));

/** A friendly default name for a user captured from a request. */
function suggestUserName(ex) {
  for (const [k, v] of queryPairsOf(ex)) if (/^(user|username|login|email|account|name)$/i.test(k) && v) return v.split('@')[0].slice(0, 40);
  const rb = ex.req_text || '';
  const m = rb.match(/"?(user(name)?|email|login)"?\s*[:=]\s*"?([^"&,}\s]{2,40})/i);
  if (m) {
    let v = m[3];
    try {
      v = decodeURIComponent(v.replace(/\+/g, ' '));
    } catch (_) {}
    return v.split('@')[0];
  }
  return 'User from ' + (ex.host || 'capture');
}

/** Opens the Saved users sheet with a new user pre-filled from a captured login. */
function saveUserFrom(ex) {
  const g = sessionGrant(ex);
  if (!g) return toast('No session cookie found on this response.', 'err');
  manageUsers(() => toast('Saved. Act as this user from the title bar, or pick it in the Bench “As…” menu.', 'ok'), { name: suggestUserName(ex), note: `Captured from ${ex.method} ${ex.path}`, headers: [['Cookie', g.cookie]] });
}

/** The suggestions that need a built-in tool: each asks to switch it on first if it is off. */
const saveLoginAsUser = (ex) => useTool('saved-users', 'Save login as a user').then((ok) => ok && saveUserFrom(ex));
const checkIdAcrossUsers = (ex) => useTool('access-check', 'Check this id across users').then((ok) => ok && startAccessCheck({ targets: [ex.id], sourceLabel: `${ex.method} ${ex.path}` }));
const replaySignedOut = (ex) => useTool('access-check', 'Replay signed out').then((ok) => ok && startAccessCheck({ targets: [ex.id], sourceLabel: `${ex.method} ${ex.path}`, onlyAnon: true }));

const IDEAS_QUESTION =
  'Suggest up to three things worth trying next on this endpoint. For each, say in plain words what to change, what result would mean there is a problem, and give the exact request to send from the Plonix Bench. Start with the most promising one.';

/** A quick look at whether a response is an API description (OpenAPI or Swagger). */
function looksLikeApiSpec(ex) {
  const t = ex.resp_text;
  return !!t && t.length < 8_000_000 && /"(openapi|swagger)"\s*:/.test(t.slice(0, 4000)) && /"paths"\s*:/.test(t);
}

/* ---------- Leads into the Scans tab ----------
   Some endpoint shapes have a natural follow-up that lives in Scans: a file
   upload, an endpoint that takes inputs. Rather than "scan the whole host",
   these leads hand exactly the one endpoint to Scans with the fitting checks
   pre-picked, so the researcher reviews and runs — the human decides, nothing
   fires on its own. Each lead is a label plus the OWASP categories to focus;
   coverage stays at the category level, concrete checks come from the Market. */

const UPLOAD_PATH = /\/(upload|uploads|file|files|attachment|attachments|media|document|documents|import|avatar|avatars|photo|photos|image|images)(?:\/|$|\?)/i;

/** An endpoint that takes a file — the "what does it accept, and what happens then?" smell. */
function uploadTarget(ex) {
  if (!/^(POST|PUT|PATCH)$/i.test(ex.method || '')) return null;
  const ct = (header(ex.req_headers, 'content-type') || '').toLowerCase();
  if (ct.includes('multipart/form-data')) return { how: 'sends a multipart form, the usual shape of a file upload' };
  if (/\bfilename\s*=/.test(ex.req_text || '')) return { how: 'carries a filename in the body' };
  if (ct.includes('application/octet-stream')) return { how: 'posts a raw file body' };
  if (UPLOAD_PATH.test(ex.path || '')) return { how: 'is a write to an upload-shaped path' };
  return null;
}

/** Whether a request carries inputs worth checking how the server handles. */
function hasInputs(ex) {
  if (queryPairsOf(ex).length) return true;
  const rb = ex.req_text || '';
  return rb.length < 200_000 && /[^&=]=[^&=]/.test(rb) && !/^[[{]/.test(rb.trim());
}

/** Does a value look like a hostname, URL or IP the server might fetch? */
function looksLikeHostOrUrl(v) {
  const s = (v || '').trim();
  if (s.length < 4 || s.length > 2048 || /\s|@/.test(s)) return null;
  if (/^(https?:)?\/\/[^/\s]/i.test(s)) return 'a URL';
  if (/^\d{1,3}(\.\d{1,3}){3}(:\d+)?(\/|$)/.test(s)) return 'an IP address';
  // A bare hostname: dotted, ending in an alphabetic label, not a file name.
  if (/^[a-z0-9][a-z0-9.-]*\.[a-z]{2,}(:\d+)?(\/|$)/i.test(s) && !/\.(js|css|png|jpe?g|gif|svg|webp|woff2?|map|json|xml|txt|ico)$/i.test(s)) return 'a hostname';
  return null;
}

/** An input whose value points at another server — the "does it fetch this?" smell. */
function ssrfTarget(ex) {
  for (const [k, v] of queryPairsOf(ex)) {
    const what = looksLikeHostOrUrl(v);
    if (what) return { name: k, where: `the "${k}" parameter`, what, value: v };
  }
  const rb = ex.req_text || '';
  if (rb.length < 100_000 && /[=&]/.test(rb) && !/^[[{]/.test(rb.trim())) {
    for (const part of rb.split('&')) {
      const i = part.indexOf('=');
      if (i <= 0) continue;
      const dec = (s) => { try { return decodeURIComponent(s.replace(/\+/g, ' ')); } catch (_) { return s; } };
      const k = dec(part.slice(0, i));
      const what = looksLikeHostOrUrl(dec(part.slice(i + 1)));
      if (what) return { name: k, where: `the "${k}" field`, what, value: dec(part.slice(i + 1)) };
    }
  }
  return null;
}

/**
 * The single most specific follow-up for this endpoint that belongs in Scans,
 * as a one-item list (or none). Self-describing so the chip and the Scans focus
 * banner can explain exactly what will run. `categories` are OWASP ids to
 * pre-pick; an empty array pre-picks none. Order: the most specific wins.
 */
function scanLeadsFor(ex) {
  if (!ex || decide(ex.host) !== 'accepted') return [];
  const where = `${ex.method} ${ex.path}`;
  const up = uploadTarget(ex);
  if (up) {
    return [{
      kind: 'upload',
      chip: 'Check this upload in Scans',
      focus: 'file handling',
      categories: [],
      title: `an upload endpoint (${where})`,
      why: `This ${up.how}. Open it in Scans to check how the upload is handled — allowed types, where files land, what the server does with them.`,
      note: 'Plonix has no built-in file-handling check yet, so nothing is pre-picked here. Add a file-handling check from the Market, or experiment with the upload field on the Bench.',
    }];
  }
  const ssrf = ssrfTarget(ex);
  if (ssrf) {
    return [{
      kind: 'ssrf',
      chip: 'Scan this input for SSRF',
      focus: 'server-side requests (SSRF)',
      categories: ['A10'],
      title: `server-side requests on ${where}`,
      why: `${ssrf.where} carries ${ssrf.what} ("${ssrf.value.slice(0, 40)}") that the server may fetch. Open it in Scans to check whether it can be pointed at somewhere it shouldn't reach.`,
      note: 'Plonix has no built-in SSRF check yet, so nothing is pre-picked here. Add one from the Market, or change the value on the Bench and watch where the request goes.',
    }];
  }
  if (hasInputs(ex)) {
    return [{
      kind: 'inputs',
      chip: "Scan this endpoint's inputs",
      focus: 'input handling',
      categories: ['A03'],
      title: `input handling on ${where}`,
      why: 'This endpoint takes inputs. Open it in Scans to run the input-handling checks that fit, scoped to just this endpoint.',
      note: '',
    }];
  }
  return [];
}

/* ---------- Detector packs (Mind Reader suggestions as data) ----------
   A detector is declarative: it recognizes a shape in the exchange and offers
   a chip that hands off to another tab — an upload to check in Scans, a token
   to tweak on the Bench, a cookie to write up as a finding. The engine only
   serves the definitions; the matching runs here, next to the chips. A
   detector can't run code or send anything: a match produces one suggestion
   chip, and only for a host already in scope. Community packs plug in the same
   way, so these grow without an app release. */

let DETECTORS = null;
let DETECTORS_PENDING = null;
/** Loads the detectors in effect once, then serves them from memory. */
async function loadDetectors() {
  if (DETECTORS) return DETECTORS;
  if (!DETECTORS_PENDING) {
    DETECTORS_PENDING = api('/api/detectors')
      .then((d) => ((DETECTORS = (d && d.detectors) || []), DETECTORS))
      .catch(() => ((DETECTORS = []), DETECTORS));
  }
  return DETECTORS_PENDING;
}

const DET_RE_CACHE = new Map();
/** A case-insensitive RegExp for a pattern, compiled once; null if invalid. */
function detRe(p) {
  if (DET_RE_CACHE.has(p)) return DET_RE_CACHE.get(p);
  let re = null;
  try {
    re = new RegExp(p, 'i');
  } catch (_) {
    re = null;
  }
  DET_RE_CACHE.set(p, re);
  return re;
}

/** Does a value look like a file path or name (and not a host or URL)? */
function looksLikePathOrFile(v) {
  const s = (v || '').trim();
  if (s.length < 2 || s.length > 2048 || /\s/.test(s)) return null;
  if (looksLikeHostOrUrl(s)) return null;
  if (/(^|[/\\])\.\.([/\\]|$)/.test(s)) return 'a path that climbs directories';
  if (/[/\\]/.test(s)) return 'a file path';
  if (/^[\w.-]+\.[a-z0-9]{1,8}$/i.test(s)) return 'a file name';
  return null;
}

/** Does a value look like a JWT (three base64url segments, the middle a claims set)? */
function looksLikeJwt(v) {
  return /eyj[a-z0-9_-]+\.eyj[a-z0-9_-]+\.[a-z0-9_-]+/i.test((v || '').trim()) ? 'a token' : null;
}

const DET_VALUE_CLASS = { host_or_url: looksLikeHostOrUrl, path_or_file: looksLikePathOrFile, jwt: looksLikeJwt };

/** The request body as key/value pairs, when it is a urlencoded form. */
function formPairsOf(ex) {
  const rb = ex.req_text || '';
  if (rb.length > 200_000 || !/[=&]/.test(rb) || /^[[{]/.test(rb.trim())) return [];
  const dec = (s) => {
    try {
      return decodeURIComponent(s.replace(/\+/g, ' '));
    } catch (_) {
      return s;
    }
  };
  const out = [];
  for (const part of rb.split('&')) {
    const i = part.indexOf('=');
    if (i <= 0) continue;
    out.push([dec(part.slice(0, i)), dec(part.slice(i + 1))]);
  }
  return out;
}

/** Whether a header condition holds. The header must exist; then any sub-test given must pass. */
function headerCondMatch(headers, c) {
  const vals = (headers || []).filter(([k]) => (k || '').toLowerCase() === c.name.toLowerCase()).map(([, v]) => v || '');
  if (!vals.length) return false;
  if (c.contains && !vals.some((v) => v.toLowerCase().includes(c.contains.toLowerCase()))) return false;
  if (c.regex) {
    const re = detRe(c.regex);
    if (!re || !vals.some((v) => re.test(v))) return false;
  }
  if (c.absent_regex) {
    const re = detRe(c.absent_regex);
    // The header is present, but at least one value lacks the pattern
    // (e.g. one Set-Cookie has HttpOnly and another does not).
    if (!re || !vals.some((v) => !re.test(v))) return false;
  }
  return true;
}

/** Runs a detector's `when` over an exchange. Returns a capture (to fill the
 *  chip text) when every condition holds, or null. */
function detectorMatch(ex, when) {
  const cap = { method: ex.method || '', path: ex.path || '' };
  if (when.method && when.method.length && !when.method.some((m) => m.toUpperCase() === (ex.method || '').toUpperCase())) return null;
  if (when.req_content_type) {
    const ct = (header(ex.req_headers, 'content-type') || '').toLowerCase();
    if (!ct.includes(when.req_content_type.toLowerCase())) return null;
    cap.ct = ct;
  }
  if (when.resp_content_type) {
    const ct = (header(ex.resp_headers, 'content-type') || '').toLowerCase();
    if (!ct.includes(when.resp_content_type.toLowerCase())) return null;
    cap.ct = ct;
  }
  if (when.path_regex) {
    const re = detRe(when.path_regex);
    if (!re || !re.test(ex.path || '')) return null;
  }
  if (when.req_body_regex) {
    const re = detRe(when.req_body_regex);
    if (!re || !re.test((ex.req_text || '').slice(0, 200_000))) return null;
  }
  if (when.resp_body_regex) {
    const re = detRe(when.resp_body_regex);
    if (!re || !re.test((ex.resp_text || '').slice(0, 200_000))) return null;
  }
  if (when.req_header && !headerCondMatch(ex.req_headers, when.req_header)) return null;
  if (when.resp_header && !headerCondMatch(ex.resp_headers, when.resp_header)) return null;
  if (when.status) {
    const s = ex.status || 0;
    if (when.status.min != null && s < when.status.min) return null;
    if (when.status.max != null && s > when.status.max) return null;
  }
  if (when.param) {
    const test = DET_VALUE_CLASS[when.param.value_class];
    if (!test) return null;
    const places = when.param.in && when.param.in.length ? when.param.in : ['query', 'body'];
    const pairs = [];
    if (places.includes('query')) pairs.push(...queryPairsOf(ex));
    if (places.includes('body')) pairs.push(...formPairsOf(ex));
    let hit = null;
    for (const [k, v] of pairs) {
      const what = test(v);
      if (what) {
        hit = { param: k, value: v, what };
        break;
      }
    }
    if (!hit) return null;
    cap.param = hit.param;
    cap.value = hit.value;
    cap.what = hit.what;
  }
  return cap;
}

/** Fills `{method}`, `{path}`, `{param}`, `{value}`, `{ct}`, `{what}` in a string. */
function fillTemplate(s, cap) {
  if (!s) return s;
  return s.replace(/\{(method|path|param|value|ct|what)\}/g, (_, k) => {
    const v = cap[k] != null ? String(cap[k]) : '';
    return k === 'value' ? v.slice(0, 60) : v;
  });
}

/** Detector suggestions for this exchange, already in priority order. Each is
 *  the served detector plus the capture used to fill its text. In-scope only. */
async function detectorLeads(ex) {
  if (!ex || decide(ex.host) !== 'accepted') return [];
  const defs = await loadDetectors();
  const out = [];
  for (const d of defs) {
    // An access hand-off only makes sense when the Access check is switched on.
    if (d.suggest.handler === 'access' && !toolOn('access-check')) continue;
    const cap = detectorMatch(ex, d.when || {});
    if (cap) out.push({ d, cap });
  }
  return out;
}

/** Detector suggestions without awaiting: used where a menu is built on the
 *  spot. Returns nothing until the detectors have loaded (kicking that off),
 *  which they have after the first Lens draw. */
function detectorLeadsSync(ex) {
  if (!ex || decide(ex.host) !== 'accepted') return [];
  if (!DETECTORS) {
    loadDetectors();
    return [];
  }
  const out = [];
  for (const d of DETECTORS) {
    if (d.suggest.handler === 'access' && !toolOn('access-check')) continue;
    const cap = detectorMatch(ex, d.when || {});
    if (cap) out.push({ d, cap });
  }
  return out;
}

const DET_CHIP_CLASS = { scan: 'k-scan', bench: 'k-bench', finding: 'k-warn', access: 'k-access' };

/** Acts on a detector suggestion: pre-fills the handler's tab, nothing is sent. */
function runDetectorLead(ex, d, cap) {
  const s = d.suggest;
  const title = fillTemplate(s.title, cap);
  const note = fillTemplate(s.note, cap);
  switch (s.handler) {
    case 'scan':
      scanEndpoint(ex, {
        kind: d.id,
        chip: fillTemplate(s.chip, cap),
        focus: s.focus,
        categories: Array.isArray(s.categories) ? s.categories : [],
        title,
        why: fillTemplate(s.why, cap),
        note,
      });
      break;
    case 'bench':
      benchWithNote(ex.id, note);
      break;
    case 'finding':
      findingForm(null, [ex.id], title, { severity: s.severity, note });
      break;
    case 'access':
      useTool('access-check', fillTemplate(s.chip, cap)).then((ok) => ok && startAccessCheck({ targets: [ex.id], sourceLabel: `${ex.method} ${ex.path}`, onlyAnon: s.mode === 'anon' }));
      break;
  }
}

/** The "Suggested" row under the Lens header. Every chip is one click to act on, and nothing is sent until clicked. */
async function drawLensSuggestions(slot, ex, list) {
  const chips = [];
  const inScope = decide(ex.host) === 'accepted';
  const hint = inScope ? findingHint(ex, list) : null;
  if (hint) {
    chips.push(
      h('button', { class: 'chip k-warn', title: 'Record this as a finding, with this request as evidence. Claude can write it up for you.', onclick: () => findingForm(null, [ex.id], hint.title, hint) }, h('span', { text: '+ Finding: ' + hint.chip })),
    );
  }
  // An error or stack trace you're looking at usually isn't the only one.
  if ((ex.status >= 500 || (list || []).some((i) => i.kind === 'stack-trace')) && ex.host) {
    const cls = Math.floor((ex.status || 500) / 100) + 'xx';
    chips.push(h('button', { class: 'chip', title: `Show every ${cls} response from ${ex.host} in Traffic, so you can see how far this reaches.`, onclick: () => setQuery(`host:${ex.host} status:${cls}`) }, h('span', { text: 'Find others like this' })));
  }
  // A permissive cross-origin policy — one header combo that's easy to miss.
  const cors = corsIssue(ex);
  if (cors) {
    chips.push(h('button', { class: 'chip k-warn', title: cors.note + ' Record it as a finding.', onclick: () => findingForm(null, [ex.id], `Permissive cross-origin policy on ${ex.method} ${ex.path}`, { severity: cors.severity, note: cors.note }) }, h('span', { text: '+ Finding: open CORS policy' })));
  }
  // A login handed back a session — offer to keep it as a saved user.
  if (sessionGrant(ex)) {
    chips.push(h('button', { class: 'chip k-user', title: 'Save the session this response just set as a reusable user, ready in the Bench “As…” picker and the Access check.', onclick: () => saveLoginAsUser(ex) }, h('span', { text: 'Save login as a user' })));
  }
  if (inScope) {
    // An id in the path or a param — check whether other users' records answer too.
    const id = idTargetOf(ex);
    if (id) {
      chips.push(h('button', { class: 'chip k-access', title: `Replay this request as each saved user and signed out, to see if ${id.where} (${id.value.slice(0, 24)}) lets you reach records that aren’t yours.`, onclick: () => checkIdAcrossUsers(ex) }, h('span', { text: 'Check this id across users' })));
    }
    // An authenticated request — does it still work with the login removed?
    if (authHeadersOf(ex.req_headers || []).length && ex.status >= 200 && ex.status < 300) {
      chips.push(h('button', { class: 'chip k-access', title: 'Replay this request with your login removed, to see whether it needs you signed in at all.', onclick: () => replaySignedOut(ex) }, h('span', { text: 'Replay signed out' })));
    }
    // A value that comes straight back — set it up as a Bench experiment.
    const refl = reflectedValues(ex);
    if (refl.length) {
      const r = refl[0];
      chips.push(h('button', { class: 'chip k-bench', title: `The ${r.where} value “${r.value.slice(0, 32)}” comes back unescaped in the response. Open this request on the Bench to vary it and compare.`, onclick: () => benchWithNote(ex.id, `“${r.name}” is reflected in the response — vary it and compare.`, r.where === 'query' ? r.name : null) }, h('span', { text: 'Reflected value → Bench' })));
    }
    // A redirect-shaped parameter — open it ready to follow.
    const rd = redirectParam(ex);
    if (rd) {
      chips.push(h('button', { class: 'chip k-bench', title: `The “${rd.name}” parameter carries a URL the server may follow. Open this request on the Bench to change it and watch where it lands.`, onclick: () => benchWithNote(ex.id, `“${rd.name}” carries a redirect target — change it and follow where it goes.`, rd.name) }, h('span', { text: 'Trace this redirect' })));
    }
    // GraphQL — enumerate the schema in Scans, or open it on the Bench.
    if (isGraphql(ex)) {
      if (inScope) {
        chips.push(
          h(
            'button',
            {
              class: 'chip k-scan',
              title: 'Check whether this GraphQL endpoint exposes its full schema through introspection. Scans will send one read-only introspection query.',
              onclick: () =>
                scanEndpoint(ex, {
                  focus: 'GraphQL schema',
                  categories: ['API9'],
                  title: `${ex.method} ${ex.path}`,
                  note: 'Plonix will send one read-only introspection query and flag the endpoint if the full schema comes back.',
                }),
            },
            h('span', { text: 'Enumerate schema in Scans' }),
          ),
        );
      }
      chips.push(h('button', { class: 'chip k-bench', title: 'Open this GraphQL request on the Bench to edit the operation and explore the schema.', onclick: () => benchWithNote(ex.id, 'GraphQL endpoint — edit the operation to explore what it exposes.') }, h('span', { text: 'GraphQL → Bench' })));
    }
    // Something on this endpoint is worth taking into Scans, scoped to it.
    for (const lead of scanLeadsFor(ex)) {
      chips.push(h('button', { class: 'chip k-scan', title: lead.why, onclick: () => scanEndpoint(ex, lead) }, h('span', { text: lead.chip })));
    }
    // Detector packs: the same idea as data, so the community can add more.
    for (const { d, cap } of await detectorLeads(ex)) {
      const title = fillTemplate(d.suggest.why || d.suggest.note || d.suggest.title || d.suggest.chip, cap);
      chips.push(
        h('button', { class: `chip ${DET_CHIP_CLASS[d.suggest.handler] || ''}`, title, onclick: () => runDetectorLead(ex, d, cap) }, h('span', { text: fillTemplate(d.suggest.chip, cap) })),
      );
    }
  }
  if (looksLikeApiSpec(ex)) {
    let spec = null;
    try {
      spec = await api(`/api/traffic/${ex.id}/spec`);
    } catch (_) {}
    if (spec && spec.endpoints.length && slot.isConnected) {
      const todo = spec.endpoints.filter((e) => !e.visited).length;
      chips.push(
        h(
          'button',
          { class: 'chip k-path', title: `${spec.title || 'API description'}: ${spec.endpoints.length} endpoints for ${spec.host}. Show them in the Map.`, onclick: () => showSpecInMap(spec.host) },
          h('span', { text: 'API description' }),
          h('span', { class: 'n', text: todo ? `${todo} not visited` : `${spec.endpoints.length} endpoints` }),
        ),
      );
    }
  }
  if (agentsOn() && inScope) {
    chips.push(h('button', { class: 'chip k-ai', title: 'Ask Claude Code what to try next here. You see what is shared first.', onclick: () => askClaude({ kind: 'request', id: ex.id }, { question: IDEAS_QUESTION }) }, h('span', { text: '✦ Ideas for this endpoint' })));
  }
  if (!slot.isConnected) return;
  slot.hidden = !chips.length;
  clear(slot, chips.length ? [h('span', { class: 'chipslbl', text: 'Suggested' }), chips] : null);
}

function showSpecInMap(host) {
  M.sel = host;
  M.specOpen = host;
  leaveTo('map');
}

/* ---------- Claude writes the finding ---------- */

function findingQuestion(ids, note) {
  return [
    'Write this up as a security finding for a report.',
    note ? 'What Plonix noticed: ' + note : '',
    ids.length > 1 ? `More evidence: requests ${ids.slice(1).map((i) => '#' + i).join(', ')}. Read them with get_request.` : '',
    'Use simple, plain words a developer who is new to security can follow. Answer with only a JSON object and no other text:',
    '{"title": "a short title, under 90 characters", "severity": "info, low, medium, high or critical", "severity_reason": "one or two sentences on why this severity", "what_happens": "two or three sentences", "why_it_matters": "one or two sentences on the impact", "steps": ["each step to reproduce it, in order"]}',
    'If the evidence does not show a real issue, say so in what_happens and use info.',
  ]
    .filter(Boolean)
    .join('\n');
}

/** Pulls the JSON object out of Claude's answer. */
function parseFindingAnswer(text) {
  const a = text.indexOf('{');
  const b = text.lastIndexOf('}');
  if (a < 0 || b <= a) return null;
  try {
    const o = JSON.parse(text.slice(a, b + 1));
    return o && typeof o.title === 'string' ? o : null;
  } catch (_) {
    return null;
  }
}

function findingDescription(o, curl) {
  const out = [];
  if (o.what_happens) out.push('What happens\n' + o.what_happens);
  if (o.why_it_matters) out.push('Why it matters\n' + o.why_it_matters);
  if (o.severity_reason) out.push('Why this severity\n' + o.severity_reason);
  const steps = Array.isArray(o.steps) ? o.steps.filter((x) => typeof x === 'string' && x.trim()) : [];
  if (steps.length) out.push('How to reproduce\n' + steps.map((x, i) => `${i + 1}. ${x.trim()}`).join('\n'));
  if (curl) out.push('The request, as curl\n' + curl);
  return out.join('\n\n');
}

/**
 * Asks Claude Code to write a finding from its evidence requests. Uses the
 * same context bundle and in-app conversation as Ask Claude, then fills the
 * form; nothing is saved until the user presses Save.
 */
async function writeFindingWithClaude(ids, note, onProgress, signal) {
  let bundle = await api('/api/agents/ask', { method: 'POST', body: { kind: 'request', id: ids[0], question: findingQuestion(ids, note) } });
  if (bundle.over_budget) bundle = await api('/api/agents/ask', { method: 'POST', body: { kind: 'request', id: ids[0], question: findingQuestion(ids, note), max_body_chars: 2000 } });
  if (bundle.over_budget) throw new Error('This request is too large to send as is. Use Ask Claude to choose what to share.');
  const { id } = await api('/api/agents/run', { method: 'POST', body: { prompt: bundle.prompt } });
  signal.run = id;
  let since = 0;
  let text = '';
  for (;;) {
    if (signal.stop) return null;
    const snap = await api(`/api/agents/run/${id}?since=${since}`);
    for (const ev of snap.events) {
      since = ev.seq + 1;
      if (ev.type === 'text') text += ev.text + '\n';
      else if (ev.type === 'error') throw new Error(ev.text);
    }
    if (snap.status !== 'running') break;
    const p = snap.progress;
    if (p) onProgress(`${p.step}… ${claudeStats(p)}` + (p.idle_ms > CLAUDE_QUIET_MS ? ' · waiting on Claude Code' : ''));
    await new Promise((r) => setTimeout(r, 600));
  }
  const o = parseFindingAnswer(text);
  if (!o) throw new Error('Claude did not answer in the expected shape. Try again, or use Ask Claude.');
  let curl = '';
  try {
    curl = curlForExchange(await getExchange(ids[0]));
  } catch (_) {}
  return { title: o.title.trim().slice(0, 200), severity: SEVERITIES.includes(String(o.severity).toLowerCase()) ? String(o.severity).toLowerCase() : null, description: findingDescription(o, curl) };
}

/* ---------- copy as curl ---------- */

const shq = (s) => "'" + String(s).replace(/'/g, "'\\''") + "'";

/**
 * A curl command that sends this request again. Headers curl works out by
 * itself (Content-Length, HTTP/2 pseudo-headers, a Host matching the URL)
 * are left out; a binary body is noted rather than pasted.
 */
function curlFor(method, url, headers, body, binary) {
  const host = hostOf(url);
  const parts = ['curl'];
  const m = (method || 'GET').toUpperCase();
  if (m !== 'GET' || (body && m !== 'POST')) parts.push('-X ' + shq(m));
  parts.push(shq(url));
  let compressed = false;
  for (const [k, v] of headers || []) {
    const name = k.toLowerCase();
    if (name.startsWith(':') || name === 'content-length' || name === 'connection') continue;
    if (name === 'host' && v.split(':')[0].toLowerCase() === host) continue;
    if (name === 'accept-encoding' && /gzip|br|deflate/.test(v)) compressed = true;
    parts.push('-H ' + shq(`${k}: ${v}`));
  }
  if (compressed) parts.push('--compressed');
  if (body) parts.push('--data-raw ' + shq(body));
  let cmd = parts.join(' \\\n  ');
  if (binary) cmd += '\n# The body is binary and is not included.';
  return cmd;
}

function curlForExchange(ex) {
  const binary = ex.req_text == null && b64len(ex.req_body) > 0;
  return curlFor(ex.method, ex.url, ex.req_headers, binary ? '' : ex.req_text || '', binary);
}

async function copyCurl(id) {
  try {
    await copyText(curlForExchange(await getExchange(id)));
  } catch (e) {
    toast(e.message, 'err');
  }
}

async function copyText(text) {
  try {
    await navigator.clipboard.writeText(text);
    toast('Copied', 'ok');
  } catch (_) {
    toast('Could not copy to the clipboard', 'err');
  }
}

/** Highlights every occurrence of `needle` in a rendered request or response. */
function markIn(pre, needle) {
  if (!pre || !needle || needle.length < 3) return;
  const walker = document.createTreeWalker(pre, NodeFilter.SHOW_TEXT);
  const nodes = [];
  while (walker.nextNode()) nodes.push(walker.currentNode);
  let first = null;
  for (const node of nodes) {
    const text = node.nodeValue;
    let at = text.indexOf(needle);
    if (at < 0) continue;
    const frag = document.createDocumentFragment();
    let from = 0;
    while (at >= 0) {
      frag.append(text.slice(from, at));
      const m = h('mark', { class: 'hit', text: needle });
      first = first || m;
      frag.append(m);
      from = at + needle.length;
      at = text.indexOf(needle, from);
    }
    frag.append(text.slice(from));
    node.replaceWith(frag);
  }
  // Scroll only the pane, never the window around it.
  if (first) pre.scrollTo({ top: Math.max(0, first.offsetTop - pre.clientHeight / 3), behavior: 'smooth' });
}

function unmark(pre) {
  if (!pre) return;
  for (const m of pre.querySelectorAll('mark.hit')) m.replaceWith(m.textContent);
  pre.normalize();
}

/**
 * Request and response side by side, with a divider that drags to resize
 * them. The split is remembered per place (`lens`, `bench`); double-click
 * the divider to even it out again.
 */
function sideBySide(cls, key, left, right) {
  const wrap = h('div', { class: cls + ' sbs' });
  const apply = (pct) => {
    wrap.style.setProperty('--lw', pct + 'fr');
    wrap.style.setProperty('--rw', 100 - pct + 'fr');
  };
  apply(store('plonix.split.' + key) || 50);
  const bar = h('div', {
    class: 'vsplit',
    title: 'Drag to resize · double-click to reset',
    ondblclick: () => {
      apply(50);
      store('plonix.split.' + key, null);
    },
    onmousedown: (e) => {
      e.preventDefault();
      const box = wrap.getBoundingClientRect();
      document.body.classList.add('colresize');
      const move = (ev) => {
        const pct = Math.round(Math.max(15, Math.min(85, ((ev.clientX - box.left) / box.width) * 100)));
        apply(pct);
        store('plonix.split.' + key, pct);
      };
      const up = () => {
        document.body.classList.remove('colresize');
        window.removeEventListener('mousemove', move);
        window.removeEventListener('mouseup', up);
      };
      window.addEventListener('mousemove', move);
      window.addEventListener('mouseup', up);
    },
  });
  return append(wrap, [left, bar, right]);
}

/** A handle on the right edge of a side list that drags its width. The width
 *  goes to `wrap`'s `cssVar` and is remembered under `key`; a double-click
 *  goes back to the stylesheet's width. */
function widthGrip(wrap, key, cssVar, { min = 160, max = 0.6 } = {}) {
  const apply = (px) => (px ? wrap.style.setProperty(cssVar, px + 'px') : wrap.style.removeProperty(cssVar));
  apply(store(key));
  return h('div', {
    class: 'wgrip',
    title: 'Drag to resize · double-click to reset',
    ondblclick: () => {
      store(key, null);
      apply(null);
    },
    onmousedown: (e) => {
      e.preventDefault();
      const box = wrap.getBoundingClientRect();
      document.body.classList.add('colresize');
      const move = (ev) => {
        const px = Math.round(Math.max(min, Math.min(box.width * max, ev.clientX - box.left)));
        apply(px);
        store(key, px);
      };
      const up = () => {
        document.body.classList.remove('colresize');
        window.removeEventListener('mousemove', move);
        window.removeEventListener('mouseup', up);
      };
      window.addEventListener('mousemove', move);
      window.addEventListener('mouseup', up);
    },
  });
}

/** A handle that drags `box`'s height: moving it up makes `box` taller.
 *  `get`/`set` read and apply the height so the caller decides what it
 *  means (a fixed height or a cap); the last size is remembered under `key`
 *  and a double-click forgets it so the box fits its content again. */
function heightGrip(key, { get, set, fit, min = 80, max }) {
  return h('div', {
    class: 'hgrip',
    title: 'Drag to resize · double-click to fit',
    ondblclick: () => {
      store(key, null);
      fit();
    },
    onmousedown: (e) => {
      e.preventDefault();
      const startY = e.clientY;
      const startH = get();
      document.body.classList.add('rowresize');
      const move = (ev) => {
        const px = Math.round(Math.max(min, Math.min(max(), startH - (ev.clientY - startY))));
        set(px);
        store(key, px);
      };
      const up = () => {
        document.body.classList.remove('rowresize');
        window.removeEventListener('mousemove', move);
        window.removeEventListener('mouseup', up);
      };
      window.addEventListener('mousemove', move);
      window.addEventListener('mouseup', up);
    },
  });
}

function startResize(e, insp) {
  e.preventDefault();
  const startY = e.clientY;
  const startH = insp.getBoundingClientRect().height;
  const move = (ev) => {
    T.inspH = Math.max(140, Math.min(window.innerHeight - 220, startH - (ev.clientY - startY)));
    insp.style.height = T.inspH + 'px';
    store('plonix.inspH', T.inspH);
  };
  const up = () => {
    window.removeEventListener('mousemove', move);
    window.removeEventListener('mouseup', up);
  };
  window.addEventListener('mousemove', move);
  window.addEventListener('mouseup', up);
}

/** A window of its own showing one request: `#/lens/<id>` on this project's
 *  address. It signs in with the token this window already has. */
let LENS_WINDOW = (() => {
  const m = location.hash.match(/^#\/lens\/(\d+)$/);
  return m ? Number(m[1]) : 0;
})();

function leaveLensWindow() {
  LENS_WINDOW = 0;
  document.documentElement.classList.remove('lenswin');
  document.title = S.status ? 'Plonix · ' + S.status.project : 'Plonix';
}

function openLensWindow(id) {
  const w = window.open(location.origin + '/#/lens/' + id, 'plonix-lens-' + id, 'popup,width=1100,height=760');
  if (w) w.focus();
}

function closeInspector() {
  if (LENS_WINDOW) return window.close();
  T.sel = null;
  const slot = $('#inspslot');
  if (slot) clear(slot);
  for (const tr of document.querySelectorAll('#rows tr.sel, tr[data-ex].sel')) tr.classList.remove('sel');
}

const scopeTag = (d) => ({ accepted: 'in', rejected: 'rej', unknown: 'out' })[d];
const scopeLabel = (d) => ({ accepted: 'in scope', rejected: 'rejected', unknown: 'not in scope' })[d];

/* ---- adaptive scope banner on the traffic screen ---- */

const EV = {
  shares_session: 'Shares a session',
  shares_certificate: 'Shares a certificate',
  redirected_from: 'Redirected from',
  requested_from: 'Called from',
  linked_from: 'Linked from',
  discovered: 'Found by lookup',
};

/**
 * Pending scope decisions, as one slim bar above Traffic: how many are
 * waiting, the strongest one with its choices, and Accept all / Reject all.
 * Nothing here needs an answer: Skip moves to the next one, Hide tucks the
 * bar away until a new domain is suggested. The count stays in the sidebar.
 */
function renderBanner() {
  const slot = $('#bannerslot');
  if (!slot) return;
  const all = stillPending(S.scope.suggestions);
  const fresh = all.filter((s) => !(T.hidden || []).includes(s.domain));
  if (!all.length || !fresh.length) return clear(slot);
  const queue = all.filter((s) => !(T.skipped || []).includes(s.domain));
  const s = queue[0] || all[0];
  const ev = s.evidence[0];
  const label = (ev && EV[ev.kind]) || '';
  clear(
    slot,
    h(
      'div',
      { class: 'scopequeue' },
      h('button', { class: 'qcount', title: 'Review every suggestion on the Scope screen', onclick: () => go('scope') }, h('b', { text: all.length }), all.length === 1 ? ' scope decision' : ' scope decisions'),
      h('span', { class: 'qdom', text: s.domain, title: s.domain }),
      ev ? h('span', { class: 'qev', title: s.evidence.map((e) => e.summary).join('\n') }, label, ' ', ev.via) : null,
      h('span', { class: 'qacts' }, scopeButtons(s, 'sm'), all.length > 1 ? h('button', { class: 'btn sm ghost', text: 'Skip', title: 'Decide later; show the next one', onclick: () => ((T.skipped = queue.length > 1 ? [...(T.skipped || []), s.domain] : []), renderBanner()) }) : null),
      h(
        'span',
        { class: 'qall' },
        all.length > 1 ? [h('button', { class: 'btn sm', text: 'Accept all', onclick: () => decideAll('accept') }), h('button', { class: 'btn sm', text: 'Reject all', onclick: () => decideAll('reject') })] : null,
        h('button', { class: 'iconbtn', text: '✕', title: 'Hide until a new domain is suggested (they stay on the Scope screen)', onclick: () => ((T.hidden = all.map((x) => x.domain)), renderBanner()) }),
      ),
    ),
  );
}

/** The host a suggestion is about: `*.example.com` is about example.com. */
const suggestionBase = (domain) => domain.replace(/^\*\./, '');

/**
 * Suggestions still waiting on a decision. The engine already drops a
 * suggestion once a rule covers it, but a rule added elsewhere can land a
 * moment before the next scope refresh, so we also hide anything the current
 * rules already decide. A domain that is part of an existing rule never
 * prompts again.
 */
const stillPending = (sugg) => (sugg || []).filter((s) => decide(suggestionBase(s.domain)) === 'unknown');

/** The three choices for one suggestion: this host only, with subdomains, or reject. */
function scopeButtons(s, size, after) {
  const base = suggestionBase(s.domain);
  const cls = (extra) => 'btn ' + (size || '') + ' ' + (extra || '');
  const act = (action, domain, subs) => async () => (await decideDomain(action, domain, subs)) && after && after();
  return [
    h('button', { class: cls('danger'), text: 'Reject', title: `Keep ${s.domain} out of scope`, onclick: act('reject', s.domain, false) }),
    h('button', { class: cls(), text: '+ subdomains', title: `Accept ${base} and every subdomain (*.${base})`, onclick: act('accept', base, true) }),
    h('button', { class: cls('primary'), text: 'Only ' + base, title: `Accept ${base} only, not its subdomains`, onclick: act('accept', base, false) }),
  ];
}

/**
 * Accepts (each host only) or rejects every pending suggestion, after asking.
 * Every domain in the sheet has a checkbox, so any of them can be left out.
 */
function decideAll(action) {
  const list = stillPending(S.scope.suggestions);
  if (!list.length) return;
  const accept = action === 'accept';
  const shown = (s) => (accept ? suggestionBase(s.domain) : s.domain);
  const picked = new Set(list.map((s) => s.domain));
  const run = async () => {
    const chosen = list.filter((s) => picked.has(s.domain));
    if (!chosen.length) return;
    closeModal();
    let done = 0;
    for (const s of chosen) {
      try {
        await api('/api/scope/' + action, { method: 'POST', body: { domain: shown(s), include_subdomains: false } });
        done++;
      } catch (e) {
        toast(e.message, 'err');
      }
    }
    toast(accept ? `✓ ${done} domain${done === 1 ? '' : 's'} accepted into scope` : `✗ ${done} domain${done === 1 ? '' : 's'} kept out of scope`, accept ? 'ok' : '');
    await loadScope();
    if (S.view === 'scope') renderScopeBody();
  };
  const goBtn = h('button', { class: 'btn ' + (accept ? 'primary' : 'danger'), onclick: run });
  const toggleAll = h('button', { class: 'link', onclick: () => (picked.size === list.length ? picked.clear() : list.forEach((s) => picked.add(s.domain)), boxes.forEach((b) => (b.checked = picked.has(b.value))), sync()) });
  const boxes = list.map((s) => h('input', { type: 'checkbox', value: s.domain, checked: true, onchange: (e) => (e.target.checked ? picked.add(s.domain) : picked.delete(s.domain), sync()) }));
  const sync = () => {
    const n = picked.size;
    const all = n === list.length;
    $('.modal .mcard h3').textContent = (accept ? 'Accept ' : 'Reject ') + (all ? `all ${n} suggested domains?` : `${n} of ${list.length} suggested domains?`);
    goBtn.textContent = n === 0 ? 'Nothing picked' : all ? (accept ? 'Accept all' : 'Reject all') : `${accept ? 'Accept' : 'Reject'} ${n}`;
    goBtn.disabled = n === 0;
    toggleAll.textContent = all ? 'Uncheck all' : 'Check all';
  };
  modal(
    '',
    [
      h('p', { class: 'muted', text: (accept ? 'Each host is accepted on its own, without its subdomains.' : 'They stay captured, but Bench sends to them are refused.') + ' Uncheck any domain to leave it for later.' }),
      h('div', { class: 'allbar' }, toggleAll),
      h('div', { class: 'alllist' }, list.map((s, i) => h('label', { class: 'allrow' }, boxes[i], h('span', { class: 'mono', text: shown(s), title: shown(s) })))),
    ],
    [h('button', { class: 'btn', text: 'Cancel', onclick: closeModal }), goBtn],
  );
  sync();
}

function evidenceList(evidence) {
  return h(
    'div',
    { class: 'evlist' },
    evidence.map((e) => {
      return h(
        'div',
        { class: 'ev' },
        h('span', { class: 'k', text: EV[e.kind] || e.kind }),
        h('span', { class: 'd' }, e.summary, e.detail ? ' · ' + e.detail : ''),
        // A lookup (subdomain discovery) has no request behind it.
        h('span', { class: 'w' }, e.count > 1 ? '×' + e.count + ' ' : '', e.exchange_id ? h('button', { class: 'link', text: '#' + e.exchange_id, title: 'Show the request this came from', onclick: () => showExchange(e.exchange_id) }) : null),
      );
    }),
  );
}

/** Opens a request in the Lens. Screens with a Lens of their own (Traffic,
 * Map, Findings) show it in place, so the user never loses their spot. */
function showExchange(id) {
  if ($('#inspslot')) return openInspector(id);
  T.sel = id;
  leaveTo('traffic');
}
