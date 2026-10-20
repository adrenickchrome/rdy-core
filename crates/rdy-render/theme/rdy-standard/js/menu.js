// The version menu, on the live site only: a click on the pill shows the versions, the open proposals, the open views and the profiles in a popover under it, and keeps it current while it is open. The pill's own href is the menu's URL, with the page's scope and profile in it. A pill without data-menu (a static render), or with an href that resolves to another origin, stays a plain link. When the menu cannot load, the click follows the link to the full page.
(function () {
  "use strict";
  var LABEL = "Versions, proposals, views and profiles";
  if (!window.fetch || !window.DOMParser) return;
  // the open menu: its pill, its URL (the pill's href when it opened), its popover once loaded,
  // its stream and the stream's URL, and the number of its latest load (a load that a newer one
  // replaced changes nothing)
  var open = null;

  // the pill in the header now: a label event (live.js) rewrites the version slot and the pill in it
  function pill() {
    return document.querySelector(".pill[data-menu]");
  }

  // the URL a pill asks for: its href as the browser resolves it against the page, when that stays
  // on this origin (http or https), as a path and query; else none. The browser's own parser
  // decides, so a tab, a newline or a "\\" inside "//" cannot hide another host.
  function menuUrl(p) {
    var h = p.getAttribute("href");
    if (!h || !window.URL) return null;
    var u;
    try {
      u = new window.URL(h, window.location.href);
    } catch (e) {
      return null;
    }
    if ((u.protocol !== "http:" && u.protocol !== "https:") || u.origin !== window.location.origin) return null;
    return u.pathname + u.search;
  }

  // the menu and its stream's URL, from the versions page at `url`; no markup from anywhere else
  function load(url) {
    return window.fetch(url, { credentials: "same-origin", mode: "same-origin" }).then(function (r) {
      if (!r.ok) throw new Error("the menu did not load");
      return r.text();
    }).then(function (text) {
      var page = new window.DOMParser().parseFromString(text, "text/html");
      var nav = page.querySelector("nav.vmenu");
      if (!nav) throw new Error("the page has no menu");
      var script = page.querySelector("script[data-events]");
      return {
        nav: document.importNode(nav, true),
        events: script ? script.getAttribute("data-events") : null
      };
    });
  }

  function stopStream() {
    if (open && open.es) {
      open.es.close();
      open.es = null;
    }
  }

  function stream() {
    if (!open || open.es || !open.events || !window.EventSource) return;
    if (document.visibilityState === "hidden") return;
    var es = new window.EventSource(open.events);
    open.es = es;
    es.addEventListener("reload", function () {
      if (open && open.es === es) refresh();
    });
  }

  // the menu o, if it is still open and `n` is its latest load
  function current(o, n) {
    return open === o && o.n === n;
  }

  // marks the pill now in the header as the open menu's; false when there is none
  function claim(o) {
    var p = pill();
    if (!p) return false;
    o.pill = p;
    p.setAttribute("aria-expanded", "true");
    return true;
  }

  // closes menu o and follows its pill's link to the full page: the menu did not load, or has
  // no pill to show under
  function follow(o) {
    var p = pill() || o.pill;
    stopStream();
    open = null;
    if (o.pop && o.pop.parentNode) o.pop.parentNode.removeChild(o.pop);
    o.pill.setAttribute("aria-expanded", "false");
    p.setAttribute("aria-expanded", "false");
    window.location.href = p.getAttribute("href");
  }

  // loads the menu again in place, with the stream of what it shows; a failure follows the link
  function refresh() {
    var o = open;
    if (!o || !o.pop) return;
    var n = ++o.n;
    stopStream();
    load(o.url).then(function (got) {
      if (!current(o, n)) return;
      // the popover stays where it is; a new pill says the menu is open
      claim(o);
      o.pop.replaceChild(got.nav, o.pop.firstChild);
      o.events = got.events;
      stream();
    }, function () {
      if (current(o, n)) follow(o);
    });
  }

  function show(p, url) {
    var o = { pill: p, url: url, pop: null, es: null, events: null, n: 0 };
    open = o;
    p.setAttribute("aria-expanded", "true");
    load(url).then(function (got) {
      if (!current(o, 0)) return;
      if (!claim(o)) {
        follow(o);
        return;
      }
      var pop = document.createElement("div");
      pop.className = "vmenu-pop";
      pop.setAttribute("role", "dialog");
      pop.setAttribute("aria-label", LABEL);
      pop.appendChild(got.nav);
      // beside the header's version slot, which a label event rewrites
      var at = o.pill.closest("[data-rdy-version]") || o.pill;
      at.parentNode.insertBefore(pop, at.nextSibling);
      o.pop = pop;
      o.events = got.events;
      var first = pop.querySelector("a");
      if (first) first.focus();
      stream();
    }, function () {
      if (current(o, 0)) follow(o);
    });
  }

  function hide() {
    var o = open;
    if (!o) return;
    stopStream();
    open = null;
    if (o.pop && o.pop.parentNode) o.pop.parentNode.removeChild(o.pop);
    o.pill.setAttribute("aria-expanded", "false");
    // the pill a label event put in place of the one that opened the menu
    var p = pill() || o.pill;
    p.setAttribute("aria-expanded", "false");
    p.focus();
  }

  document.addEventListener("click", function (e) {
    var t = e.target;
    var p = t && t.closest ? t.closest(".pill[data-menu]") : null;
    if (p) {
      // a new tab or window follows the link
      if (e.button || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
      var url = open ? null : menuUrl(p);
      // a link that leaves the site is the browser's
      if (!open && !url) return;
      e.preventDefault();
      if (open) hide();
      else show(p, url);
      return;
    }
    if (open && !(open.pop && open.pop.contains(t))) hide();
  });
  document.addEventListener("keydown", function (e) {
    if (open && e.key === "Escape") hide();
  });
  document.addEventListener("visibilitychange", function () {
    if (document.visibilityState === "hidden") stopStream();
    else if (open && open.pop) refresh();
  });
  window.addEventListener("pagehide", stopStream);
})();
