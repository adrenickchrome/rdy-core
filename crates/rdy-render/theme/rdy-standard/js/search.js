(function () {
  'use strict';
  // Search: loads the prebuilt index on first use and matches it in the page. Every piece of
  // text it shows is put in the page with textContent, never as markup. Without this script the
  // box does nothing and the rest of the page is unchanged.
  var input = document.getElementById('search');
  var results = document.getElementById('results');
  if (!input || !results) return;

  var MAX = 8;
  var RANK = { term: 0, section: 1, doc: 2, folder: 3, question: 4 };
  var NOT_AVAILABLE = 'Search is not available on this copy of the site.';
  var href = input.getAttribute('data-index') || 'search-index.json';
  // the index sits at the site root, and so does the prefix that leads there
  var root = href.replace(/search-index\.json$/, '');
  var loading = null;
  var entries = [];
  var failed = false;
  var shown = [];
  var selected = -1;

  results.setAttribute('aria-live', 'polite');

  function load() {
    if (!loading) {
      loading = fetch(href)
        .then(function (res) {
          if (!res.ok) throw new Error('missing');
          return res.json();
        })
        .then(function (data) {
          if (!data || !Array.isArray(data.entries)) throw new Error('unreadable');
          entries = data.entries.map(function (e) {
            e.lowerTitle = String(e.title || '').toLowerCase();
            e.lowerText = String(e.text || '').toLowerCase();
            return e;
          });
        })
        .catch(function () {
          failed = true;
        });
    }
    return loading;
  }

  function clear() {
    while (results.firstChild) results.removeChild(results.firstChild);
    shown = [];
    selected = -1;
    input.removeAttribute('aria-activedescendant');
  }

  function open(on) {
    results.hidden = !on;
    input.setAttribute('aria-expanded', on ? 'true' : 'false');
  }

  function note(text) {
    clear();
    var p = document.createElement('p');
    p.className = 'results-empty';
    p.setAttribute('role', 'option');
    p.setAttribute('aria-disabled', 'true');
    p.textContent = text;
    results.appendChild(p);
    open(true);
  }

  // The hits for q: a title match before a text match, then term, section, doc, folder, question.
  function find(q) {
    var hits = [];
    for (var i = 0; i < entries.length; i++) {
      var e = entries[i];
      var inTitle = e.lowerTitle.indexOf(q) >= 0;
      if (!inTitle && e.lowerText.indexOf(q) < 0) continue;
      var kind = RANK[e.kind];
      hits.push({ e: e, score: (inTitle ? 0 : 10) + (kind === undefined ? 9 : kind), at: i });
    }
    hits.sort(function (a, b) {
      return a.score - b.score || a.at - b.at;
    });
    return hits.slice(0, MAX).map(function (h) {
      return h.e;
    });
  }

  // The text around the first match, with the match in a <mark>.
  function snippet(e, q) {
    var span = document.createElement('span');
    span.className = 'r-snip';
    var text = String(e.text || '');
    var i = e.lowerText.indexOf(q);
    if (i < 0) {
      span.textContent = text.length > 90 ? text.slice(0, 90) + '…' : text;
      return span;
    }
    var a = Math.max(0, i - 40);
    var b = Math.min(text.length, i + q.length + 60);
    span.appendChild(document.createTextNode((a > 0 ? '…' : '') + text.slice(a, i)));
    var mark = document.createElement('mark');
    mark.textContent = text.slice(i, i + q.length);
    span.appendChild(mark);
    span.appendChild(document.createTextNode(text.slice(i + q.length, b) + (b < text.length ? '…' : '')));
    return span;
  }

  function option(e, q, n) {
    var a = document.createElement('a');
    a.setAttribute('role', 'option');
    a.setAttribute('aria-selected', 'false');
    a.id = 'result-' + n;
    a.href = root + e.page + (e.anchor ? '#' + e.anchor : '');
    var where = document.createElement('span');
    where.className = 'r-where';
    where.textContent = e.doc;
    var title = document.createElement('span');
    title.className = 'r-title';
    title.textContent = e.title;
    a.appendChild(where);
    a.appendChild(title);
    a.appendChild(snippet(e, q));
    return a;
  }

  function render() {
    var raw = input.value.trim();
    if (!raw) {
      clear();
      open(false);
      return;
    }
    if (failed) {
      note(NOT_AVAILABLE);
      return;
    }
    var q = raw.toLowerCase();
    var hits = find(q);
    if (!hits.length) {
      note('Nothing matches “' + raw + '”.');
      return;
    }
    clear();
    for (var i = 0; i < hits.length; i++) {
      var a = option(hits[i], q, i);
      results.appendChild(a);
      shown.push(a);
    }
    open(true);
  }

  function run() {
    load().then(render);
  }

  function select(n) {
    if (!shown.length) return;
    if (selected >= 0) shown[selected].setAttribute('aria-selected', 'false');
    selected = (n + shown.length) % shown.length;
    var a = shown[selected];
    a.setAttribute('aria-selected', 'true');
    input.setAttribute('aria-activedescendant', a.id);
    if (a.scrollIntoView) a.scrollIntoView({ block: 'nearest' });
  }

  function reset() {
    input.value = '';
    clear();
    open(false);
  }

  input.addEventListener('focus', function () {
    load().then(function () {
      if (input.value.trim()) render();
    });
  });
  input.addEventListener('input', run);
  input.addEventListener('keydown', function (event) {
    var key = event.key;
    if (key === 'Enter') {
      var target = shown[selected >= 0 ? selected : 0];
      if (target) {
        event.preventDefault();
        window.location.href = target.href;
      }
    } else if (key === 'Escape') {
      reset();
      input.blur();
    } else if (key === 'ArrowDown') {
      event.preventDefault();
      select(selected + 1);
    } else if (key === 'ArrowUp') {
      event.preventDefault();
      select(selected < 0 ? -1 : selected - 1);
    }
  });
  results.addEventListener('click', function (event) {
    if (event.target.closest && event.target.closest('a')) reset();
  });
  document.addEventListener('click', function (event) {
    if (!(event.target.closest && event.target.closest('.search-wrap'))) open(false);
  });
  // `/` jumps to the box unless the reader is typing somewhere
  document.addEventListener('keydown', function (event) {
    if (event.key === "/" && !event.ctrlKey && !event.metaKey && !event.altKey) {
      var at = document.activeElement;
      var tag = at && at.tagName;
      var typing = tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT' || (at && at.isContentEditable);
      if (typing) return;
      event.preventDefault();
      input.focus();
    }
  });
})();
