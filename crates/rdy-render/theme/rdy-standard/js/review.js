// Show changes: the button on a draft render hides and shows the Edited, New and Removed marks. Without the button (a canon or tag render) nothing happens. The choice lasts the browser session; where storage is blocked the marks stay shown and nothing is remembered.
(function () {
  "use strict";
  var KEY = "rdy-changes";
  var button = document.getElementById("show-changes");
  if (!button) return;
  var root = document.documentElement;

  function remembered() {
    try {
      return sessionStorage.getItem(KEY) !== "hidden";
    } catch (e) {
      return true;
    }
  }

  function remember(shown) {
    try {
      if (shown) sessionStorage.removeItem(KEY);
      else sessionStorage.setItem(KEY, "hidden");
    } catch (e) {
      // blocked storage: the choice holds for this page only
    }
  }

  function apply(shown) {
    button.setAttribute("aria-pressed", shown ? "true" : "false");
    if (shown) root.removeAttribute("data-changes");
    else root.setAttribute("data-changes", "hidden");
  }

  var shown = remembered();
  apply(shown);
  button.hidden = false;
  button.addEventListener("click", function () {
    shown = !shown;
    apply(shown);
    remember(shown);
  });
})();
