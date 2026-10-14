// rdy live site: reload this page when it changes; update the version label in place.
// Only a page in view keeps its stream open: a browser opens few connections to one site, and a
// stream holds one for its whole life. A page that comes back into view opens its stream again
// with the revisions it was loaded at, and the server sends reload at once if it changed.
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
