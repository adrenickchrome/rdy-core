(function () {
  'use strict';
  // The scroll-spy of the "On this page" index: the link of the heading the reader is in is
  // marked. Without this script the index is a plain list of links.
  function start() {
    if (!('IntersectionObserver' in window)) return;
    var index = document.querySelector('nav[aria-label="On this page"]');
    var content = document.querySelector('[data-rdy="content"]');
    if (!index || !content) return;

    var links = {};
    var anchors = index.querySelectorAll('a[href^="#"]');
    for (var i = 0; i < anchors.length; i++) {
      links[decodeURIComponent(anchors[i].getAttribute('href').slice(1))] = anchors[i];
    }

    var headings = [];
    var all = content.querySelectorAll('h1[id], h2[id], h3[id], h4[id], h5[id], h6[id]');
    for (var j = 0; j < all.length; j++) {
      if (links[all[j].id]) headings.push(all[j]);
    }
    if (!headings.length) return;

    var seen = {};
    var active = null;

    function mark(id) {
      if (id === active) return;
      if (active && links[active]) links[active].classList.remove('is-active');
      active = id;
      if (links[id]) links[id].classList.add('is-active');
    }

    var observer = new IntersectionObserver(
      function (entries) {
        for (var k = 0; k < entries.length; k++) {
          seen[entries[k].target.id] = entries[k].isIntersecting;
        }
        // the first heading in the band wins; between bands the last one stays marked
        for (var h = 0; h < headings.length; h++) {
          if (seen[headings[h].id]) {
            mark(headings[h].id);
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
