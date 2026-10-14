// rdy live site: reload when the page changes; update the version label in place.
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
      var text = JSON.parse(e.data).text;
      var nodes = document.querySelectorAll("[data-rdy-label]");
      for (var i = 0; i < nodes.length; i++) nodes[i].textContent = text;
    });
  }
  document.addEventListener("visibilitychange", function () {
    if (document.visibilityState === "hidden") close(); else open();
  });
  addEventListener("pagehide", close);
  addEventListener("pageshow", open);
  open();
})();
