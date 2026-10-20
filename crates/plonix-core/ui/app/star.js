/* Plonix — the one-time "Star Plonix on GitHub" card.
 *
 * The engine decides when (src/star.rs): from the third different day of use,
 * right after something is finished. Never in the demo, never over a dialog or
 * the walkthrough. Counted on this computer only. */

const STAR_URL = 'https://github.com/SergeyMalych/plonix';

function starNote(event) {
  return api('/api/ui/star', { method: 'POST', body: { event } }).catch(() => ({ ask: false }));
}

/** A project window opened: counts the day. */
function starOpen() {
  if (!LENS_WINDOW) starNote('open');
}

/** Something was finished ("finished"), or a Bench send came back ("sent"). */
async function starMoment(kind) {
  if (S.status?.demo || LENS_WINDOW || $('.starcard')) return;
  const r = await starNote(kind);
  if (r && r.ask) setTimeout(showStarCard, 1500);
}

function showStarCard() {
  // Wait for a quiet moment: no dialog, no walkthrough, no menu.
  if ($('.modal, .tourcard, .popover, .ctxmenu')) return setTimeout(showStarCard, 2000);
  if ($('.starcard')) return;
  const close = (event) => {
    card.remove();
    starNote(event);
  };
  const card = h(
    'div',
    { class: 'starcard', role: 'dialog', 'aria-label': 'Star Plonix on GitHub' },
    h('span', { class: 'starmark', 'aria-hidden': 'true', text: '★' }),
    h(
      'div',
      { class: 'starbody' },
      h('b', { text: 'Finding Plonix useful?' }),
      h('p', { text: 'A star on GitHub helps other researchers find it.' }),
      h(
        'div',
        { class: 'staracts' },
        h('a', { class: 'btn sm primary', href: STAR_URL, target: '_blank', rel: 'noopener', text: 'Star on GitHub', onclick: () => setTimeout(() => close('starred'), 0) }),
        h('button', { class: 'btn sm ghost', text: 'Maybe later', onclick: () => close('later') }),
        h('button', { class: 'linkbtn', text: 'No thanks', onclick: () => close('never') }),
      ),
    ),
    h('button', { class: 'iconbtn starx', title: 'Maybe later', 'aria-label': 'Maybe later', text: '✕', onclick: () => close('later') }),
  );
  let box = $('.toasts');
  if (!box) document.body.append((box = h('div', { class: 'toasts' })));
  box.prepend(card);
}
