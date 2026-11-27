// plonix.io page counter. No cookies, no storage, no ids, nothing about the visitor's device. Each page view
// sends the page's path and where the visit came from (the referring site's name only, never its full address)
// to /api/hit, which adds one to an hourly total and keeps nothing else. Download button clicks are counted the
// same way. Browsers that ask sites not to track them (Global Privacy Control or Do Not Track) send nothing.
(() => {
  try {
    if (navigator.globalPrivacyControl || navigator.doNotTrack === '1' || window.doNotTrack === '1') return;
    if (location.hostname !== 'plonix.io') return;
    let ref = '';
    try { ref = document.referrer ? new URL(document.referrer).hostname : ''; } catch (e) {}
    const q = new URLSearchParams(location.search);
    const src = (q.get('ref') || q.get('utm_source') || '').slice(0, 40);
    const send = (body) => {
      const data = JSON.stringify(body);
      if (navigator.sendBeacon && navigator.sendBeacon('/api/hit', new Blob([data], { type: 'application/json' }))) return;
      fetch('/api/hit', { method: 'POST', body: data, headers: { 'Content-Type': 'application/json' }, keepalive: true }).catch(() => {});
    };
    send({ kind: 'view', path: location.pathname, ref, src });
    document.addEventListener('click', (e) => {
      const a = e.target.closest && e.target.closest('a[href]');
      if (!a) return;
      const href = a.getAttribute('href');
      const os = /\.dmg$/.test(href) ? 'macos' : /\.exe$/.test(href) ? 'windows' : '';
      if (os) send({ kind: 'download_' + os, path: location.pathname, ref, src });
    }, true);
  } catch (e) {}
})();
