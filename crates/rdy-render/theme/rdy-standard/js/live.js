// rdy live site: reload this page when it changes; update the version label in place.
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
  var es = new EventSource(s.getAttribute("data-events"));
  es.addEventListener("reload", function () {
    try { sessionStorage.setItem(key, String(scrollY)); } catch (e) {}
    es.close();
    location.reload();
  });
  es.addEventListener("label", function (e) {
    var text = JSON.parse(e.data).text;
    var nodes = document.querySelectorAll("[data-rdy-label]");
    for (var i = 0; i < nodes.length; i++) nodes[i].textContent = text;
  });
})();
