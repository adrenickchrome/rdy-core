(function () {
  'use strict';
  // The theme toggle, the folds and the phone menu. Everything here is progressive: without
  // this script every page shows its navigation and its index in the flow of the page.
  var root = document.documentElement;
  var KEY = 'rdy-theme';

  function stored() {
    try {
      var t = localStorage.getItem(KEY);
      return t === 'light' || t === 'dark' ? t : 'system';
    } catch (e) {
      return 'system';
    }
  }

  function effective(choice) {
    if (choice === 'light' || choice === 'dark') return choice;
    var dark = window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches;
    return dark ? 'dark' : 'light';
  }

  function mark(choice) {
    var buttons = document.querySelectorAll('[data-theme-choice]');
    for (var i = 0; i < buttons.length; i++) {
      var on = buttons[i].getAttribute('data-theme-choice') === choice;
      buttons[i].setAttribute('aria-checked', on ? 'true' : 'false');
    }
  }

  function announce(choice) {
    document.dispatchEvent(
      new CustomEvent('rdy:theme', { detail: { theme: effective(choice) } })
    );
  }

  function choose(choice) {
    if (choice === 'light' || choice === 'dark') {
      root.setAttribute('data-theme', choice);
    } else {
      root.removeAttribute('data-theme');
    }
    try {
      if (choice === 'light' || choice === 'dark') localStorage.setItem(KEY, choice);
      else localStorage.removeItem(KEY);
    } catch (e) {}
    mark(choice);
    announce(choice);
  }

  var group = document.querySelector('.theme');
  if (group) {
    mark(stored());
    group.addEventListener('click', function (event) {
      var button = event.target.closest && event.target.closest('[data-theme-choice]');
      if (button) choose(button.getAttribute('data-theme-choice'));
    });
    group.addEventListener('keydown', function (event) {
      var keys = { ArrowRight: 1, ArrowDown: 1, ArrowLeft: -1, ArrowUp: -1 };
      if (!(event.key in keys)) return;
      var buttons = Array.prototype.slice.call(group.querySelectorAll('[data-theme-choice]'));
      var at = buttons.indexOf(document.activeElement);
      if (at < 0) return;
      event.preventDefault();
      var next = buttons[(at + keys[event.key] + buttons.length) % buttons.length];
      next.focus();
      choose(next.getAttribute('data-theme-choice'));
    });
    // In the automatic mode the diagrams follow the system theme as it changes.
    if (window.matchMedia) {
      var query = window.matchMedia('(prefers-color-scheme: dark)');
      var follow = function () {
        if (stored() === 'system') announce('system');
      };
      if (query.addEventListener) query.addEventListener('change', follow);
      else if (query.addListener) query.addListener(follow);
    }
  }

  // The folds: the navigation and the index are open in the markup, so a page without
  // scripting reaches both by scrolling. Here they start closed where the screen is narrow.
  var app = document.querySelector('.app');
  var menu = document.getElementById('menu');
  var scrim = document.getElementById('scrim');
  var nav = document.querySelector('details.sidenav-fold');
  var toc = document.querySelector('details.toc-fold');

  function fold(details, narrow) {
    if (!details) return;
    if (narrow) details.removeAttribute('open');
    else details.setAttribute('open', '');
  }

  function drawer(open) {
    if (!app) return;
    app.classList.toggle('nav-open', open);
    if (menu) menu.setAttribute('aria-expanded', open ? 'true' : 'false');
    // The drawer shows the pages, so its fold is open while it is; a closed drawer goes
    // back to the compact page.
    if (nav) fold(nav, !open && window.matchMedia('(max-width: 900px)').matches);
  }

  if (window.matchMedia) {
    var phone = window.matchMedia('(max-width: 900px)');
    var tablet = window.matchMedia('(max-width: 1200px)');
    var apply = function () {
      var opened = app && app.classList.contains('nav-open');
      fold(nav, phone.matches && !opened);
      fold(toc, tablet.matches);
      if (!phone.matches && opened) drawer(false);
    };
    apply();
    [phone, tablet].forEach(function (q) {
      if (q.addEventListener) q.addEventListener('change', apply);
      else if (q.addListener) q.addListener(apply);
    });
  }

  if (menu) {
    menu.hidden = false;
    menu.addEventListener('click', function () {
      drawer(!(app && app.classList.contains('nav-open')));
    });
  }
  if (scrim) scrim.addEventListener('click', function () { drawer(false); });
  document.addEventListener('keydown', function (event) {
    if (event.key === 'Escape' && app && app.classList.contains('nav-open')) {
      drawer(false);
      if (menu) menu.focus();
    }
  });
})();
