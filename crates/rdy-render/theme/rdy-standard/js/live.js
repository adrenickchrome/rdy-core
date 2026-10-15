// rdy live site: reload when the page changes; patch the label and header in place.
// Only a page in view holds a stream; it reopens with its revisions when it returns.
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
  var es = null;
  function set(q, k, v) {
    var n = document.querySelectorAll(q);
    for (var i = 0; i < n.length; i++) n[i][k] = v;
  }
  function close() {
    if (es) { es.close(); es = null; }
  }
  function open() {
    if (es || document.visibilityState === "hidden") return;
    es = new EventSource(url);
    es.addEventListener("reload", function () {
      try { sessionStorage.setItem(key, String(scrollY)); } catch (e) {}
      close();
      location.reload();
    });
    es.addEventListener("label", function (e) {
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
