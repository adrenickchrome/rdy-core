(function () {
  'use strict';
  // Draws every Mermaid diagram from its source, in the colours of the page. Without this
  // script, or without the library, the source and the caption stay on the page as they are.
  var counter = 0;
  var entries = [];
  var queue = Promise.resolve();

  function token(style, name) {
    return style.getPropertyValue(name).trim();
  }

  function configure() {
    var s = getComputedStyle(document.documentElement);
    window.mermaid.initialize({
      startOnLoad: false,
      securityLevel: "strict",
      theme: "base",
      themeVariables: {
        background: token(s, '--bg'),
        primaryColor: token(s, '--surface'),
        primaryTextColor: token(s, '--ink'),
        primaryBorderColor: token(s, '--line-strong'),
        lineColor: token(s, '--ink-soft'),
        secondaryColor: token(s, '--accent-soft'),
        tertiaryColor: token(s, '--surface'),
        fontFamily: token(s, '--font-text'),
        fontSize: "14px"
      }
    });
  }

  function caption(figure) {
    var c = figure.querySelector('figcaption');
    return c ? c.textContent.trim() : '';
  }

  function draw(entry) {
    var id = 'rdy-dg-' + counter++;
    return window.mermaid.render(id, entry.pre.dataset.source).then(
      function (result) {
        var host = document.createElement('div');
        host.className = 'diagram-svg';
        host.innerHTML = result.svg;
        var svg = host.querySelector('svg');
        if (svg) {
          svg.setAttribute('role', 'img');
          var label = caption(entry.figure);
          if (label) svg.setAttribute('aria-label', label);
        }
        if (entry.node.parentNode) entry.node.parentNode.replaceChild(host, entry.node);
        entry.node = host;
        entry.figure.classList.remove('diagram-error');
      },
      function () {
        // leave the source on the page, and say that the drawing failed
        var stale = document.getElementById('d' + id);
        if (stale && stale.parentNode) stale.parentNode.removeChild(stale);
        if (entry.node !== entry.pre && entry.node.parentNode) {
          entry.node.parentNode.replaceChild(entry.pre, entry.node);
        }
        entry.node = entry.pre;
        entry.figure.classList.add('diagram-error');
      }
    );
  }

  // Mermaid sizes each label box from the text as it measures it while it draws. The text face
  // is `font-display: swap` and may still be loading: measured in the fallback face, the boxes
  // come out narrower than the text and cut its last letters once the face swaps in. So wait
  // until the face has loaded (or failed: then the fallback is what shows, and is measured).
  function fontsReady() {
    var fonts = document.fonts;
    if (!fonts || !fonts.load) return Promise.resolve();
    var family = token(getComputedStyle(document.documentElement), '--font-text');
    return fonts.load('14px ' + family).then(
      function () { return fonts.ready; },
      function () {}
    );
  }

  function renderAll() {
    queue = queue.then(fontsReady).then(function () {
      configure();
      return entries.reduce(function (chain, entry) {
        return chain.then(function () {
          return draw(entry);
        });
      }, Promise.resolve());
    });
  }

  function start() {
    if (!window.mermaid) return;
    var pres = document.querySelectorAll('figure.diagram[data-lang="mermaid"] pre.mermaid');
    for (var i = 0; i < pres.length; i++) {
      var pre = pres[i];
      if (!pre.dataset.source) pre.dataset.source = pre.textContent;
      entries.push({ figure: pre.closest('figure'), pre: pre, node: pre });
    }
    if (!entries.length) return;
    renderAll();
    document.addEventListener('rdy:theme', renderAll);
    if (window.matchMedia) {
      var dark = window.matchMedia('(prefers-color-scheme: dark)');
      var follow = function () {
        if (!document.documentElement.hasAttribute('data-theme')) renderAll();
      };
      if (dark.addEventListener) dark.addEventListener('change', follow);
      else if (dark.addListener) dark.addListener(follow);
    }
  }

  // Deferred scripts run in document order while readyState is already 'interactive', and
  // mermaid.min.js is deferred after this script: wait for DOMContentLoaded, which fires after
  // every deferred script. Only a page that has fully loaded can start at once.
  if (document.readyState === 'complete') start();
  else document.addEventListener('DOMContentLoaded', start);
})();
