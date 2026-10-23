// Interactive transcript demo: click words to cut them, like in TextCut.
(() => {
  const demo = document.getElementById('demo');
  const stat = document.getElementById('demo-stat');
  const words = [...demo.querySelectorAll('.w')];
  const WORD_SECONDS = 0.26;
  const length = (w) => (w.classList.contains('pause') ? parseFloat(w.textContent.replace('⏸', '')) : WORD_SECONDS);
  const total = words.reduce((s, w) => s + length(w), 0);

  const update = () => {
    const kept = words.filter((w) => !w.classList.contains('cut')).reduce((s, w) => s + length(w), 0);
    const saved = total - kept;
    stat.textContent = `Length ${kept.toFixed(1)} s` + (saved > 0.05 ? ` · −${saved.toFixed(1)} s cut` : '');
  };

  words.forEach((w) => {
    w.tabIndex = 0;
    w.setAttribute('role', 'button');
    const toggle = () => { w.classList.toggle('cut'); update(); };
    w.addEventListener('click', toggle);
    w.addEventListener('keydown', (e) => {
      if (e.key === 'Enter' || e.key === ' ' || e.key === 'Delete' || e.key === 'Backspace') { e.preventDefault(); toggle(); }
    });
  });
  const cutAll = (sel) => { demo.querySelectorAll(sel).forEach((w) => w.classList.add('cut')); update(); };
  document.getElementById('demo-silence').addEventListener('click', () => cutAll('.pause'));
  document.getElementById('demo-fillers').addEventListener('click', () => cutAll('.filler'));
  document.getElementById('demo-reset').addEventListener('click', () => { words.forEach((w) => w.classList.remove('cut')); update(); });
  update();
})();

// Install tabs.
(() => {
  const tabs = [...document.querySelectorAll('[role="tab"]')];
  const select = (tab) => {
    tabs.forEach((t) => {
      const on = t === tab;
      t.setAttribute('aria-selected', on);
      t.tabIndex = on ? 0 : -1;
      document.getElementById(t.getAttribute('aria-controls')).hidden = !on;
    });
    tab.focus();
  };
  tabs.forEach((t, i) => {
    t.addEventListener('click', () => select(t));
    t.addEventListener('keydown', (e) => {
      if (e.key === 'ArrowRight') select(tabs[(i + 1) % tabs.length]);
      if (e.key === 'ArrowLeft') select(tabs[(i - 1 + tabs.length) % tabs.length]);
    });
  });
  // Preselect the visitor's platform.
  const p = (navigator.userAgentData?.platform || navigator.platform || '').toLowerCase();
  if (p.includes('win')) select(document.getElementById('t-win'));
  else if (p.includes('linux')) select(document.getElementById('t-linux'));
  document.activeElement?.blur();
})();

// Screenshot lightbox.
(() => {
  const box = document.getElementById('lightbox');
  const img = box.querySelector('img');
  document.querySelectorAll('.zoom').forEach((z) => {
    z.addEventListener('click', () => {
      img.src = z.dataset.full;
      img.alt = z.querySelector('img')?.alt || '';
      box.showModal();
    });
  });
  box.addEventListener('click', () => box.close());
})();
