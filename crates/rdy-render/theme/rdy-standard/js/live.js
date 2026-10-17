// rdy live site: reload or relabel a page in view. Reopen a refused stream in 1 s, doubling to 30 s.
(function () {
  var d = document, s = d.currentScript, key = "rdy-live-y:" + location.pathname;
  try {
    var y = sessionStorage.getItem(key);
    sessionStorage.removeItem(key);
    if (y) addEventListener("load", function () { scrollTo(0, +y); });
  } catch (e) {}
  if (!s || !window.EventSource) return;
  var url = s.getAttribute("data-events"), es, t, w = 0;
  function set(q, k, v) { [].forEach.call(d.querySelectorAll(q), function (n) { n[k] = v; }); }
  function close() { clearTimeout(t); if (es) es.close(); es = null; }
  function open() {
    if (es || d.visibilityState === "hidden") return;
    var me = es = new EventSource(url);
    me.onopen = function () { w = 0; };
    me.onerror = function () {
      if (es !== me || me.readyState < 2) return;
      close();
      t = setTimeout(open, w = Math.min(w * 2 || 1e3, 3e4));
    };
    me.addEventListener("reload", function () {
      try { sessionStorage.setItem(key, scrollY); } catch (e) {}
      close();
      location.reload();
    });
    me.addEventListener("label", function (e) {
      var j = JSON.parse(e.data);
      set("[data-rdy-label]", "textContent", j.text);
      set("[data-rdy-version]", "innerHTML", j.html);
    });
  }
  d.addEventListener("visibilitychange", function () { close(); open(); });
  addEventListener("pagehide", close);
  addEventListener("pageshow", open);
  open();
})();
