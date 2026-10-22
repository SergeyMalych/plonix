// Plonix window: Feedback, at the foot of the sidebar: report a bug or suggest a feature as a GitHub issue.
// Part of the project window's scripts (see index.html for the load order).
'use strict';

/* The engine builds the links (src/feedback.rs): a new issue form with only
 * the Plonix version and the system filled in. Nothing from the project goes
 * with it, and nothing is sent until the user presses Submit on GitHub. */

const FEEDBACK_ISSUES = 'https://github.com/SergeyMalych/plonix/issues';

function feedbackLink(kind) {
  const f = (S.status && S.status.feedback) || {};
  return f[kind] || (kind === 'issues' ? FEEDBACK_ISSUES : `${FEEDBACK_ISSUES}/new/choose`);
}

function openFeedbackLink(kind) {
  closePopover();
  window.open(feedbackLink(kind), '_blank', 'noopener');
}

/** The Feedback menu, opened above its sidebar button. */
function openFeedback() {
  const btn = $('#fbbtn');
  if ($('.fbmenu')) return closePopover();
  closePopover();
  hideRailTip();
  const item = (kind, title, sub) =>
    h('button', { role: 'menuitem', class: 'fbitem', onclick: () => openFeedbackLink(kind) }, h('b', { text: title }), sub ? h('span', { text: sub }) : null);
  const menu = h(
    'div',
    { class: 'ctxmenu fbmenu', role: 'menu', 'aria-label': 'Feedback' },
    item('bug', 'Report a bug', 'Something doesn’t work the way it should.'),
    item('feature', 'Suggest a feature', 'An idea, or a step that could be easier.'),
    h('div', { class: 'msep' }),
    item('issues', 'See what others reported'),
    h('div', { class: 'mnote', text: `Opens GitHub with your Plonix version and system filled in, nothing else. Please leave out tokens, cookies and target names.` }),
  );
  const r = btn ? btn.getBoundingClientRect() : { left: 8, right: 8, top: innerHeight - 8, bottom: innerHeight - 8 };
  showPopover(menu, r);
  // Above the button, or beside it when the sidebar is folded to icons.
  const folded = !!btn?.closest('.rail.collapsed');
  if (folded) menu.style.left = Math.round(r.right + 8) + 'px';
  menu.style.top = Math.max(8, Math.round((folded ? r.bottom : r.top - 6) - menu.offsetHeight)) + 'px';
}
