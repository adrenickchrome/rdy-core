(function () {
  'use strict';
  // The scroll-spy of the "On this page" index: the link of the heading the reader is in is
  // marked. Without this script the index is a plain list of links.
  function start() {
    if (!('IntersectionObserver' in window)) return;
    var index = document.querySelector('nav[aria-label="On this page"]');
    if (!index) return;

    // Each toc link points at a section anchor. The anchor is on the section's wrapper, not on
    // its heading, so the element to watch is the heading that belongs to that anchor: the
    // target itself when it is a heading, otherwise the first heading directly inside it. The
    // wrapper is never watched: a section holding sub-sections would stay in the band for all
    // of them and always win.
    function headingOf(target) {
      if (/^H[1-6]$/.test(target.tagName)) return target;
      for (var c = 0; c < target.children.length; c++) {
        if (/^H[1-6]$/.test(target.children[c].tagName)) return target.children[c];
      }
      return null;
    }

    var headings = []; // in index (document) order
    var linkOf = [];
    var anchors = index.querySelectorAll('a[href^="#"]');
    for (var i = 0; i < anchors.length; i++) {
      var target = document.getElementById(decodeURIComponent(anchors[i].getAttribute('href').slice(1)));
      var heading = target && headingOf(target);
      if (heading && headings.indexOf(heading) < 0) {
        headings.push(heading);
        linkOf.push(anchors[i]);
      }
    }
    if (!headings.length) return;

    var seen = [];
    var active = -1;

    function mark(i) {
      if (i === active) return;
      if (active >= 0) linkOf[active].classList.remove('is-active');
      active = i;
      linkOf[i].classList.add('is-active');
    }

    var observer = new IntersectionObserver(
      function (entries) {
        for (var k = 0; k < entries.length; k++) {
          var at = headings.indexOf(entries[k].target);
          if (at >= 0) seen[at] = entries[k].isIntersecting;
        }
        // the first heading in the band wins; between bands the last one stays marked
        for (var h = 0; h < headings.length; h++) {
          if (seen[h]) {
            mark(h);
            return;
          }
        }
      },
      { rootMargin: '-10% 0px -75% 0px' }
    );
    for (var n = 0; n < headings.length; n++) observer.observe(headings[n]);
  }

  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', start);
  else start();
})();
