// rdy live site: reload when the page changes; patch the label and header in place.
// Only a page in view holds a stream. A refused one reopens in 1 s, doubling to 30 s.
(function () {
  var s = document.currentScript;
  var key = "rdy-live-y:" + location.pathname;
  try {
    var y = sessionStorage.getItem(key);
    if (y !== null) {
      sessionStorage.removeItem(key);
      addEventListener("load", function () { scrollTo(0, +y); });
    }
  } catch (e) {}
  if (!s || !window.EventSource) return;
  var url = s.getAttribute("data-events");
  var es = null, t, w = 0;
  function set(q, k, v) {
    var n = document.querySelectorAll(q);
    for (var i = 0; i < n.length; i++) n[i][k] = v;
  }
  function close() {
    clearTimeout(t);
    if (es) { es.close(); es = null; }
  }
  function open() {
    if (es || document.visibilityState === "hidden") return;
    var me = es = new EventSource(url);
    me.onopen = function () { w = 0; };
    // closed (2): refused, and the browser gave up
    me.onerror = function () {
      if (es !== me || me.readyState < 2) return;
      close();
      t = setTimeout(open, w = Math.min(w * 2 || 1e3, 3e4));
    };
    me.addEventListener("reload", function () {
      try { sessionStorage.setItem(key, String(scrollY)); } catch (e) {}
      close();
      location.reload();
    });
    me.addEventListener("label", function (e) {
      var d = JSON.parse(e.data);
      set("[data-rdy-label]", "textContent", d.text);
      set("[data-rdy-version]", "innerHTML", d.html);
    });
  }
  document.addEventListener("visibilitychange", function () {
    if (document.visibilityState === "hidden") close(); else open();
  });
  addEventListener("pagehide", close);
  addEventListener("pageshow", open);
  open();
})();
