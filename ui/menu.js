// Menu contextuel Ferrox (injecté dans chaque page, cadre principal uniquement).
// - Isolé du site dans un Shadow DOM fermé : le site ne peut ni le lire ni le styler.
// - Le menu natif reste disponible : champs de saisie (correcteur, coller), sites qui ont
//   leur propre menu, et Maj + clic droit.
// - `__fxSend` et `__ACCENT__` sont fournis par Ferrox.
(() => {
  if (window.top !== window) return;
  const ACCENT = '__ACCENT__';
  let host = null, root = null, menu = null;

  const ICONS = {
    back: 'M19 12H5M11 6l-6 6 6 6', fwd: 'M5 12h14M13 6l6 6-6 6',
    reload: 'M20 12a8 8 0 1 1-2.6-5.9L20 8.5M20 3.5v5h-5', print: 'M7 9V3h10v6M7 17H5a2 2 0 0 1-2-2v-4a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2v4a2 2 0 0 1-2 2h-2M7 14h10v7H7z',
    tab: 'M14 4h6v6M20 4l-9 9M18 14v5a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V7a1 1 0 0 1 1-1h5',
    priv: 'M2 12s4-7 10-7 10 7 10 7-4 7-10 7S2 12 2 12zM3 3l18 18', copy: 'M9 9h11v11H9zM5 15H4V4h11v1',
    link: 'M10 14a5 5 0 0 0 7 0l3-3a5 5 0 0 0-7-7l-1 1M14 10a5 5 0 0 0-7 0l-3 3a5 5 0 0 0 7 7l1-1',
    search: 'M11 4.5a6.5 6.5 0 1 0 0 13 6.5 6.5 0 0 0 0-13zM20 20l-4.2-4.2', image: 'M3 5h18v14H3zM3 16l5-5 4 4 3-3 6 6M15 9h.01',
  };

  const CSS = `
    :host { all: initial; }
    .m { position: fixed; z-index: 2147483647; min-width: 228px; max-width: 320px; padding: 6px; border-radius: 12px;
      background: #18181cf5; border: 1px solid #ffffff14; color: #ececf1; font: 13px/1.2 "Segoe UI Variable Text", "Segoe UI", system-ui, sans-serif;
      box-shadow: 0 18px 48px -12px #000d, 0 0 0 1px #0008, 0 0 24px -10px ${ACCENT}66;
      transform-origin: var(--ox) var(--oy); animation: in .16s cubic-bezier(.22,1,.36,1); user-select: none; -webkit-user-select: none; }
    @keyframes in { from { opacity: 0; transform: scale(.94); } }
    .i { display: flex; align-items: center; gap: 11px; height: 32px; padding: 0 10px; border-radius: 7px; cursor: default; white-space: nowrap; }
    .i:hover { background: ${ACCENT}26; }
    .i:hover svg { color: ${ACCENT}; }
    .i.off { opacity: .38; pointer-events: none; }
    .i span { flex: 1; overflow: hidden; text-overflow: ellipsis; }
    .i kbd { font: 11px system-ui, sans-serif; color: #8d8d97; }
    svg { width: 16px; height: 16px; flex: none; stroke: currentColor; fill: none; stroke-width: 1.8; stroke-linecap: round; stroke-linejoin: round; color: #a7a7b0; transition: color .12s; }
    .s { height: 1px; margin: 5px 8px; background: #ffffff12; }
    .h { padding: 6px 10px 4px; font-size: 10.5px; letter-spacing: .8px; text-transform: uppercase; color: #8d8d97; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  `;

  function close() {
    if (menu) { menu.remove(); menu = null; }
    removeEventListener('scroll', close, true);
    removeEventListener('blur', close);
    removeEventListener('resize', close);
  }

  function ensureHost() {
    if (host && host.isConnected) return;
    host = document.createElement('ferrox-menu');
    root = host.attachShadow({ mode: 'closed' });
    const st = document.createElement('style'); st.textContent = CSS; root.append(st);
    (document.body || document.documentElement).append(host);
  }

  const svg = (d) => { const s = document.createElementNS('http://www.w3.org/2000/svg', 'svg'); s.setAttribute('viewBox', '0 0 24 24'); const p = document.createElementNS('http://www.w3.org/2000/svg', 'path'); p.setAttribute('d', d); s.append(p); return s; };

  async function copyText(t) {
    try { await navigator.clipboard.writeText(t); return; } catch (_) {}
    const ta = document.createElement('textarea'); ta.value = t; ta.style.cssText = 'position:fixed;opacity:0';
    root.append(ta); ta.select(); try { document.execCommand('copy'); } catch (_) {} ta.remove();
  }

  function show(x, y, items) {
    close(); ensureHost();
    menu = document.createElement('div'); menu.className = 'm';
    for (const it of items) {
      if (it === '-') { menu.append(Object.assign(document.createElement('div'), { className: 's' })); continue; }
      if (it.head) { menu.append(Object.assign(document.createElement('div'), { className: 'h', textContent: it.head })); continue; }
      const el = document.createElement('div'); el.className = 'i' + (it.off ? ' off' : '');
      const label = document.createElement('span'); label.textContent = it.label;
      el.append(svg(ICONS[it.icon] || ''), label);
      if (it.kbd) el.append(Object.assign(document.createElement('kbd'), { textContent: it.kbd }));
      el.addEventListener('click', (e) => { if (!e.isTrusted) return; close(); it.run(); });
      menu.append(el);
    }
    root.append(menu);
    // reste dans la fenêtre
    const r = menu.getBoundingClientRect();
    const left = Math.max(6, Math.min(x, innerWidth - r.width - 6));
    const top = Math.max(6, y + r.height > innerHeight - 6 ? y - r.height : y);
    menu.style.left = left + 'px'; menu.style.top = Math.max(6, top) + 'px';
    menu.style.setProperty('--ox', (x - left) + 'px'); menu.style.setProperty('--oy', (y - top) + 'px');
    addEventListener('scroll', close, true); addEventListener('blur', close); addEventListener('resize', close);
  }

  addEventListener('mousedown', (e) => { if (menu && !e.composedPath().includes(host)) close(); }, true);
  addEventListener('keydown', (e) => { if (menu && e.key === 'Escape') close(); }, true);

  // Phase de « bouillonnement » : si le site a déjà son propre menu (preventDefault), on ne fait rien.
  addEventListener('contextmenu', (e) => {
    if (!e.isTrusted || e.defaultPrevented || e.shiftKey) return;
    const t = e.target instanceof Element ? e.target : null;
    if (t && t.closest('input, textarea, select, [contenteditable=""], [contenteditable="true"]')) return; // menu natif
    const link = t && t.closest('a[href]');
    const img = t && t.closest('img');
    const sel = String(getSelection() || '').trim();
    const http = (u) => /^https?:\/\//i.test(u);
    const items = [];
    if (link && http(link.href)) {
      const u = link.href;
      items.push({ head: (link.textContent || u).trim().slice(0, 60) || u });
      items.push({ icon: 'tab', label: 'Ouvrir dans un nouvel onglet', run: () => __fxSend('o:' + u) });
      items.push({ icon: 'priv', label: 'Ouvrir dans un onglet privé', run: () => __fxSend('op:' + u) });
      items.push({ icon: 'link', label: 'Copier l’adresse du lien', run: () => copyText(u) });
      items.push('-');
    }
    if (img && img.currentSrc && http(img.currentSrc)) {
      const u = img.currentSrc;
      items.push({ icon: 'image', label: 'Ouvrir l’image dans un nouvel onglet', run: () => __fxSend('o:' + u) });
      items.push({ icon: 'link', label: 'Copier l’adresse de l’image', run: () => copyText(u) });
      items.push('-');
    }
    if (sel) {
      const short = sel.length > 24 ? sel.slice(0, 24) + '…' : sel;
      items.push({ icon: 'copy', label: 'Copier', kbd: 'Ctrl+C', run: () => { try { document.execCommand('copy'); } catch (_) { copyText(sel); } } });
      items.push({ icon: 'search', label: 'Rechercher « ' + short + ' »', run: () => __fxSend('s:' + sel.slice(0, 400)) });
      items.push('-');
    }
    items.push({ icon: 'back', label: 'Précédent', kbd: 'Alt+←', off: history.length < 2, run: () => history.back() });
    items.push({ icon: 'fwd', label: 'Suivant', kbd: 'Alt+→', run: () => history.forward() });
    items.push({ icon: 'reload', label: 'Actualiser', kbd: 'Ctrl+R', run: () => location.reload() });
    items.push('-');
    items.push({ icon: 'link', label: 'Copier l’adresse de la page', run: () => copyText(location.href) });
    items.push({ icon: 'print', label: 'Imprimer…', kbd: 'Ctrl+P', run: () => window.print() });
    e.preventDefault();
    show(e.clientX, e.clientY, items);
  });
})();
