// Applies the saved theme before the first paint. Loaded without `defer` from
// <head>, so it is an external file to satisfy the Content-Security-Policy.
(function () {
  var pref = 'auto';
  try { pref = localStorage.getItem('theme') || 'auto'; } catch (e) {}
  if (pref === 'light' || pref === 'dark') {
    document.documentElement.setAttribute('data-theme', pref);
  } else {
    pref = 'auto';
  }
  document.documentElement.setAttribute('data-theme-pref', pref);
})();
