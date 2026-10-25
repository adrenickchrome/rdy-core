// The sidebar's folders: each folder is a native disclosure (details and summary) that works without this script. The script only keeps their open state for the session and shows the current entry. The state is in sessionStorage, under a key made from the address of the site's home page, so file sites, origins, live scope prefixes and profiles keep their own. It holds the folder pages that are open, as absolute URLs, and nothing else. No cookie, no request, no timer. When storage is denied, the folders keep their native state.
(function () {
  "use strict";
  var PREFIX = "rdy:sidebar:1:";
  // the bounds of the stored text: its length, the number of its entries, the length of an entry
  var MAX_RAW = 65536;
  var MAX_OPEN = 256;
  var MAX_ENTRY = 2048;
  var started = false;
  var key = null;
  // the folders of this page: { details, link (the folder-page link beside it), url, active }
  var folders = [];

  // the session's storage, or none when the browser denies it
  function store() {
    try {
      return window.sessionStorage || null;
    } catch (e) {
      return null;
    }
  }

  // the absolute address of `href` as the browser resolves it on this page; none if it does not
  function absolute(href) {
    if (!href || !window.URL) return null;
    try {
      return new window.URL(href, window.location.href).href;
    } catch (e) {
      return null;
    }
  }

  // the stored open paths, or none when nothing valid is stored: text over the limit, text that
  // is not JSON, an object without a list `open`, a list over the limit, or an entry that is
  // not printable ASCII (an absolute URL always is) or is over its limit
  function read() {
    var s = store();
    if (!s || !key) return null;
    var raw;
    try {
      raw = s.getItem(key);
    } catch (e) {
      return null;
    }
    if (typeof raw !== "string" || raw.length > MAX_RAW) return null;
    var v;
    try {
      v = JSON.parse(raw);
    } catch (e) {
      return null;
    }
    if (!v || typeof v !== "object" || !Array.isArray(v.open) || v.open.length > MAX_OPEN) return null;
    for (var i = 0; i < v.open.length; i++) {
      var u = v.open[i];
      if (typeof u !== "string" || u.length === 0 || u.length > MAX_ENTRY || !/^[\x21-\x7e]+$/.test(u)) return null;
    }
    return v.open;
  }

  function byteOrder(a, b) {
    return a < b ? -1 : a > b ? 1 : 0;
  }

  // keeps what this page shows as it is now, and what it does not show as it was stored
  function save() {
    var s = store();
    if (!s || !key) return;
    var open = Object.create(null);
    var active = Object.create(null);
    var shown = Object.create(null);
    folders.forEach(function (f) {
      shown[f.url] = true;
      if (f.details.open && f.url.length <= MAX_ENTRY) open[f.url] = true;
      if (f.active) active[f.url] = true;
    });
    (read() || []).forEach(function (u) {
      if (!shown[u]) open[u] = true;
    });
    var urls = Object.keys(open);
    // the active ancestors first, then the lowest of the rest, within the limits of the text
    var order = urls.filter(function (u) { return active[u]; }).sort(byteOrder)
      .concat(urls.filter(function (u) { return !active[u]; }).sort(byteOrder));
    var kept = [];
    var size = 11; // {"open":[]}
    for (var i = 0; i < order.length && kept.length < MAX_OPEN; i++) {
      if (size + order[i].length + 3 > MAX_RAW) break;
      size += order[i].length + 3;
      kept.push(order[i]);
    }
    kept.sort(byteOrder);
    try {
      s.setItem(key, JSON.stringify({ open: kept }));
    } catch (e) {
      // denied or full: the folders keep their state on this page
    }
  }

  function start() {
    if (started) return;
    started = true;
    var nav = document.querySelector('nav[aria-label="Pages"]');
    if (!nav) return;
    var home = nav.querySelector("a");
    var homeUrl = home ? absolute(home.getAttribute("href")) : null;
    key = homeUrl ? PREFIX + homeUrl : null;
    var current = nav.querySelector("[aria-current]");
    nav.querySelectorAll("details.nav-fold").forEach(function (d) {
      var link = d.previousElementSibling;
      var url = link && link.tagName === "A" ? absolute(link.getAttribute("href")) : null;
      if (!url) return;
      folders.push({ details: d, link: link, url: url, active: false });
    });
    var stored = read();
    if (stored) {
      var set = Object.create(null);
      stored.forEach(function (u) { set[u] = true; });
      folders.forEach(function (f) {
        f.details.open = !!set[f.url];
      });
    }
    // the path to the current page is always open
    folders.forEach(function (f) {
      f.active = !!current && (f.details.contains(current) || f.link === current);
      if (f.active) f.details.open = true;
      f.details.addEventListener("toggle", save);
    });
    window.addEventListener("pagehide", save);
    if (current && current.scrollIntoView) {
      try {
        current.scrollIntoView({ block: "nearest" });
      } catch (e) {
        // the entry stays where it is
      }
    }
  }

  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", start);
  else start();
})();
