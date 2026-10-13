'use strict';
(() => {
  const button = document.getElementById('theme');
  const system = matchMedia('(prefers-color-scheme: dark)');
  let preference = null;
  try { preference = localStorage.getItem('theme'); } catch {}
  function apply(dark) {
    document.body.classList.toggle('dark', dark);
    if (button) button.setAttribute('aria-pressed', String(dark));
    window.dispatchEvent(new Event('themechange'));
  }
  function preferred() { return preference === 'dark' || preference === 'true' || preference === null && system.matches; }
  apply(preferred());
  if (button) button.addEventListener('click', () => {
    const dark = !document.body.classList.contains('dark');
    preference = JSON.stringify(dark);
    apply(dark);
    try { localStorage.setItem('theme', preference); } catch {}
  });
  system.addEventListener('change', () => { if (preference === null) apply(system.matches); });
  window.addEventListener('storage', event => { if (event.key === 'theme') { preference = event.newValue; apply(preferred()); } });
})();
