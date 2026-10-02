/*!
 * kosync-rs progressive enhancements.
 *
 * Every feature here is optional: pages work as plain HTML forms and links
 * without JavaScript. Modules are wired by data attributes so templates stay
 * free of inline handlers.
 */
(function () {
  'use strict';

  var root = document.documentElement;

  function $(selector, scope) {
    return (scope || document).querySelector(selector);
  }

  function $$(selector, scope) {
    return Array.prototype.slice.call((scope || document).querySelectorAll(selector));
  }

  function storageGet(key) {
    try { return window.localStorage.getItem(key); } catch (e) { return null; }
  }

  function storageSet(key, value) {
    try { window.localStorage.setItem(key, value); } catch (e) { /* ignore */ }
  }

  /* ------------------------------------------------------------------ */
  /* Theme                                                               */
  /* ------------------------------------------------------------------ */

  var THEMES = ['auto', 'light', 'dark'];
  var THEME_LABELS = { auto: 'Theme: match system', light: 'Theme: light', dark: 'Theme: dark' };

  function currentTheme() {
    var pref = root.getAttribute('data-theme-pref');
    return THEMES.indexOf(pref) >= 0 ? pref : 'auto';
  }

  function applyTheme(pref) {
    if (pref === 'light' || pref === 'dark') {
      root.setAttribute('data-theme', pref);
    } else {
      pref = 'auto';
      root.removeAttribute('data-theme');
    }
    root.setAttribute('data-theme-pref', pref);
    storageSet('theme', pref);
    $$('[data-theme-toggle]').forEach(function (button) {
      button.setAttribute('aria-label', THEME_LABELS[pref]);
      button.setAttribute('title', THEME_LABELS[pref]);
    });
    $$('input[name="theme-choice"]').forEach(function (input) {
      input.checked = input.value === pref;
    });
  }

  function initTheme() {
    applyTheme(currentTheme());
    document.addEventListener('click', function (event) {
      var toggle = event.target.closest('[data-theme-toggle]');
      if (!toggle) return;
      var next = THEMES[(THEMES.indexOf(currentTheme()) + 1) % THEMES.length];
      applyTheme(next);
    });
    document.addEventListener('change', function (event) {
      if (event.target.name === 'theme-choice') applyTheme(event.target.value);
    });
  }

  /* ------------------------------------------------------------------ */
  /* Toasts                                                              */
  /* ------------------------------------------------------------------ */

  function showToast(message) {
    var toast = $('#app-toast');
    if (!toast) return;
    toast.textContent = message;
    toast.hidden = false;
    window.clearTimeout(showToast.timer);
    showToast.timer = window.setTimeout(function () { toast.hidden = true; }, 6000);
  }

  function initToasts() {
    document.body.addEventListener('htmx:responseError', function (event) {
      var status = event.detail && event.detail.xhr ? event.detail.xhr.status : '';
      showToast('Something went wrong' + (status ? ' (error ' + status + ')' : '') + '. Please try again.');
    });
    document.body.addEventListener('htmx:sendError', function () {
      showToast('Network error. Please check your connection and try again.');
    });

    // Dismissible flash messages; successes fade out on their own.
    document.addEventListener('click', function (event) {
      var close = event.target.closest('[data-dismiss]');
      if (close) close.closest('.flash').remove();
    });
    $$('.flash.success').forEach(function (flash) {
      window.setTimeout(function () { flash.remove(); }, 6000);
    });
  }

  /* ------------------------------------------------------------------ */
  /* Menus (<details class="menu">)                                      */
  /* ------------------------------------------------------------------ */

  function closeMenus(except) {
    $$('details.menu[open], details.filter-panel[open]').forEach(function (menu) {
      if (menu !== except && !menu.contains(except)) menu.open = false;
    });
  }

  function initMenus() {
    document.addEventListener('click', function (event) {
      var inside = event.target.closest('details.menu, details.filter-panel');
      closeMenus(inside);
    });
    document.addEventListener('keydown', function (event) {
      if (event.key !== 'Escape') return;
      var open = $('details.menu[open], details.filter-panel[open]');
      if (open) {
        open.open = false;
        var summary = $('summary', open);
        if (summary) summary.focus();
      }
    });
  }

  /* ------------------------------------------------------------------ */
  /* Dialogs and confirmations                                           */
  /* ------------------------------------------------------------------ */

  function initDialogs() {
    document.addEventListener('click', function (event) {
      var opener = event.target.closest('[data-dialog-open]');
      if (opener) {
        var dialog = document.getElementById(opener.getAttribute('data-dialog-open'));
        if (dialog && dialog.showModal) {
          event.preventDefault();
          closeMenus(null);
          dialog.showModal();
        }
        return;
      }
      var closer = event.target.closest('[data-dialog-close]');
      if (closer) {
        var parent = closer.closest('dialog');
        if (parent) parent.close();
        return;
      }
      // Clicking the backdrop closes a dialog.
      if (event.target.tagName === 'DIALOG') event.target.close();
    });

    var confirmDialog = $('#confirm-dialog');
    var pendingForm = null;
    document.addEventListener('submit', function (event) {
      var form = event.target;
      if (!form.hasAttribute('data-confirm') || !confirmDialog || !confirmDialog.showModal) return;
      event.preventDefault();
      pendingForm = form;
      $('#confirm-title', confirmDialog).textContent = form.getAttribute('data-confirm-title') || 'Are you sure?';
      $('#confirm-message', confirmDialog).textContent = form.getAttribute('data-confirm');
      $('#confirm-accept', confirmDialog).textContent = form.getAttribute('data-confirm-label') || 'Confirm';
      closeMenus(null);
      confirmDialog.showModal();
    });
    var accept = $('#confirm-accept');
    if (accept) {
      accept.addEventListener('click', function () {
        if (pendingForm) {
          pendingForm.removeAttribute('data-confirm');
          HTMLFormElement.prototype.submit.call(pendingForm);
        }
        confirmDialog.close();
      });
    }
  }

  /* ------------------------------------------------------------------ */
  /* Command palette                                                     */
  /* ------------------------------------------------------------------ */

  function initPalette() {
    var palette = $('#palette');
    var input = $('#palette-input');
    var results = $('#palette-results');
    if (!palette || !input || !results || !palette.showModal) return;

    function items() {
      return $$('.palette-item', results);
    }

    function select(index) {
      var list = items();
      list.forEach(function (item, i) {
        item.setAttribute('aria-selected', i === index ? 'true' : 'false');
        if (!item.id) item.id = 'palette-item-' + i;
      });
      if (list[index]) {
        input.setAttribute('aria-activedescendant', list[index].id);
        list[index].scrollIntoView({ block: 'nearest' });
      } else {
        input.removeAttribute('aria-activedescendant');
      }
    }

    function selectedIndex() {
      return items().findIndex(function (item) {
        return item.getAttribute('aria-selected') === 'true';
      });
    }

    function open() {
      if (palette.open) return;
      closeMenus(null);
      palette.showModal();
      input.select();
      if (!results.children.length && window.htmx) {
        window.htmx.trigger(input, 'paletteopen');
      }
    }

    document.addEventListener('click', function (event) {
      var trigger = event.target.closest('[data-palette-open]');
      if (!trigger) return;
      event.preventDefault();
      open();
    });

    document.addEventListener('keydown', function (event) {
      var typing = /^(INPUT|TEXTAREA|SELECT)$/.test(event.target.tagName) || event.target.isContentEditable;
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === 'k') {
        event.preventDefault();
        if (palette.open) palette.close(); else open();
      } else if (event.key === '/' && !typing && !palette.open) {
        event.preventDefault();
        open();
      }
    });

    input.addEventListener('keydown', function (event) {
      var list = items();
      var index = selectedIndex();
      if (event.key === 'ArrowDown') {
        event.preventDefault();
        select(Math.min(index + 1, list.length - 1));
      } else if (event.key === 'ArrowUp') {
        event.preventDefault();
        select(Math.max(index - 1, 0));
      } else if (event.key === 'Enter') {
        if (list[index]) {
          event.preventDefault();
          window.location.href = list[index].href;
        }
      }
    });

    results.addEventListener('mousemove', function (event) {
      var item = event.target.closest('.palette-item');
      if (item) select(items().indexOf(item));
    });

    document.body.addEventListener('htmx:afterSwap', function (event) {
      if (event.detail.target === results) select(0);
    });
  }

  /* ------------------------------------------------------------------ */
  /* Clipboard, origin placeholders, password helpers                    */
  /* ------------------------------------------------------------------ */

  function initCopy() {
    $$('[data-origin]').forEach(function (el) {
      el.textContent = window.location.origin + el.getAttribute('data-origin');
    });

    document.addEventListener('click', function (event) {
      var button = event.target.closest('[data-copy]');
      if (!button) return;
      var source = document.getElementById(button.getAttribute('data-copy'));
      if (!source || !navigator.clipboard) return;
      navigator.clipboard.writeText(source.textContent.trim()).then(function () {
        var label = $('.copy-label', button);
        if (!label) return;
        var original = label.textContent;
        label.textContent = 'Copied';
        window.setTimeout(function () { label.textContent = original; }, 1600);
      });
    });
  }

  function scorePassword(value) {
    if (!value) return 0;
    var score = 0;
    if (value.length >= 8) score++;
    if (value.length >= 12) score++;
    if (/[a-z]/.test(value) && /[A-Z]/.test(value)) score++;
    if (/\d/.test(value) && /[^A-Za-z0-9]/.test(value)) score++;
    return Math.max(1, Math.min(4, score));
  }

  function initPasswords() {
    document.addEventListener('click', function (event) {
      var toggle = event.target.closest('[data-toggle-password]');
      if (!toggle) return;
      var field = document.getElementById(toggle.getAttribute('data-toggle-password'));
      if (!field) return;
      var show = field.type === 'password';
      field.type = show ? 'text' : 'password';
      toggle.setAttribute('aria-pressed', show ? 'true' : 'false');
      toggle.setAttribute('aria-label', show ? 'Hide password' : 'Show password');
    });

    $$('[data-strength]').forEach(function (meter) {
      var field = document.getElementById(meter.getAttribute('data-strength'));
      if (!field) return;
      meter.hidden = false;
      field.addEventListener('input', function () {
        meter.setAttribute('data-score', String(scorePassword(field.value)));
      });
    });
  }

  /* ------------------------------------------------------------------ */
  /* Upload dropzone                                                     */
  /* ------------------------------------------------------------------ */

  function formatBytes(bytes) {
    var units = ['B', 'kB', 'MB', 'GB'];
    var value = bytes;
    var unit = 0;
    while (value >= 1000 && unit < units.length - 1) {
      value /= 1000;
      unit++;
    }
    return (unit === 0 ? value : value.toFixed(1)) + ' ' + units[unit];
  }

  function initUpload() {
    var zone = $('#dropzone');
    var input = $('#upload-files');
    var list = $('#file-list');
    var submit = $('#upload-submit');
    if (!zone || !input || !list) return;

    function render() {
      list.innerHTML = '';
      Array.prototype.forEach.call(input.files, function (file, index) {
        var item = document.createElement('li');
        var ext = (file.name.split('.').pop() || '').toUpperCase();
        var badge = document.createElement('span');
        badge.className = 'badge accent';
        badge.textContent = ext;
        var name = document.createElement('span');
        name.className = 'file-name';
        name.textContent = file.name;
        var size = document.createElement('span');
        size.className = 'file-size';
        size.textContent = formatBytes(file.size);
        var remove = document.createElement('button');
        remove.type = 'button';
        remove.className = 'icon-button';
        remove.setAttribute('aria-label', 'Remove ' + file.name);
        remove.innerHTML = '<svg class="icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true"><path d="M18 6 6 18M6 6l12 12"/></svg>';
        remove.addEventListener('click', function () {
          if (typeof DataTransfer === 'undefined') return;
          var transfer = new DataTransfer();
          Array.prototype.forEach.call(input.files, function (f, i) {
            if (i !== index) transfer.items.add(f);
          });
          input.files = transfer.files;
          render();
        });
        item.append(badge, name, size, remove);
        list.appendChild(item);
      });
      if (submit) {
        var count = input.files.length;
        submit.textContent = count > 1 ? 'Upload ' + count + ' files' : 'Upload';
      }
    }

    input.addEventListener('change', render);

    ['dragenter', 'dragover'].forEach(function (name) {
      zone.addEventListener(name, function () { zone.classList.add('dragover'); });
    });
    ['dragleave', 'drop'].forEach(function (name) {
      zone.addEventListener(name, function () { zone.classList.remove('dragover'); });
    });

    document.body.addEventListener('htmx:afterRequest', function (event) {
      if (event.detail.elt && event.detail.elt.id === 'upload-form' && event.detail.successful) {
        input.value = '';
        render();
      }
    });
  }

  /* ------------------------------------------------------------------ */
  /* Filter panel badge                                                  */
  /* ------------------------------------------------------------------ */

  function initFilterPanel() {
    var panel = $('details.filter-panel');
    if (!panel) return;
    var badge = $('.filter-count', panel);
    function update() {
      var count = $$('select', panel).filter(function (select) { return select.value; }).length;
      if (badge) {
        badge.textContent = String(count);
        badge.hidden = count === 0;
      }
    }
    panel.addEventListener('change', update);
    update();
  }

  function init() {
    initTheme();
    initToasts();
    initMenus();
    initDialogs();
    initPalette();
    initCopy();
    initPasswords();
    initUpload();
    initFilterPanel();
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', init);
  } else {
    init();
  }
})();
