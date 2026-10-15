// The version menu, on the live site only: a click on the pill shows the versions, the open proposals and the open views in a popover under it, and keeps it current while it is open. A pill without data-menu (a static render) stays a plain link. When the menu cannot load, the click follows the link to the full page.
(function () {
  "use strict";
  var PAGE = "/~versions/index.html";
  var LABEL = "Versions, proposals and views";
  if (!window.fetch || !window.DOMParser) return;
  // the open menu: its pill, its popover once loaded, its stream and the stream's URL
  var open = null;

  function pill() {
    return document.querySelector(".pill[data-menu]");
  }

  // the menu and its stream's URL, from the versions page; no markup from anywhere else
  function load() {
    return window.fetch(PAGE, { credentials: "same-origin" }).then(function (r) {
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

  // loads the menu again in place; a failure keeps what it shows
  function refresh() {
    var o = open;
    stopStream();
    load().then(function (got) {
      if (open !== o || !o.pop) return;
      o.pop.replaceChild(got.nav, o.pop.firstChild);
      o.events = got.events;
      stream();
    }, function () {});
  }

  function show(p) {
    var o = { pill: p, pop: null, es: null, events: null };
    open = o;
    p.setAttribute("aria-expanded", "true");
    load().then(function (got) {
      if (open !== o) return;
      var pop = document.createElement("div");
      pop.className = "vmenu-pop";
      pop.setAttribute("role", "dialog");
      pop.setAttribute("aria-label", LABEL);
      pop.appendChild(got.nav);
      // beside the header's version slot, which a label event rewrites
      var at = p.closest("[data-rdy-version]") || p;
      at.parentNode.insertBefore(pop, at.nextSibling);
      o.pop = pop;
      o.events = got.events;
      var first = pop.querySelector("a");
      if (first) first.focus();
      stream();
    }, function () {
      if (open !== o) return;
      open = null;
      p.setAttribute("aria-expanded", "false");
      window.location.href = p.getAttribute("href");
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
      e.preventDefault();
      if (open) hide();
      else show(p);
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
