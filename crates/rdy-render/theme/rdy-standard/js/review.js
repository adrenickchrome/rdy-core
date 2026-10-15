// Review mode: the buttons in the review bar. Show changes hides and shows the Edited, New and Removed marks. Show diff shows each edited block's old and new text, word by word, in place of its text. Without a button (a canon or tag render) nothing happens. Each choice lasts the browser session; where storage is blocked the choice holds for this page only.
(function () {
  "use strict";
  var root = document.documentElement;

  // one toggle: the button, the session key, and what the state does to the page
  function toggle(button, key, initial, remembered, apply) {
    var on = remembered();
    apply(on);
    button.hidden = false;
    button.addEventListener("click", function () {
      on = !on;
      apply(on);
      try {
        if (on === initial) sessionStorage.removeItem(key);
        else sessionStorage.setItem(key, on ? "shown" : "hidden");
      } catch (e) {
        // blocked storage: the choice holds for this page only
      }
    });
  }

  function stored(key) {
    try {
      return sessionStorage.getItem(key);
    } catch (e) {
      return null;
    }
  }

  // Show changes: on by default; "hidden" is what is stored
  var changes = document.getElementById("show-changes");
  if (changes) {
    toggle(changes, "rdy-changes", true, function () {
      return stored("rdy-changes") !== "hidden";
    }, function (shown) {
      changes.setAttribute("aria-pressed", shown ? "true" : "false");
      if (shown) root.removeAttribute("data-changes");
      else root.setAttribute("data-changes", "hidden");
    });
  }

  // Show diff: off by default; "shown" is what is stored. The button waits for a page that holds
  // a diff.
  var diff = document.getElementById("show-diff");
  var diffs = [];
  if (diff) {
    // the root carries data-diff while the diffs are shown, and is not one of them
    var found = document.querySelectorAll("[data-diff]");
    for (var i = 0; i < found.length; i++) if (found[i] !== root) diffs.push(found[i]);
  }
  if (diff && diffs.length) {
    toggle(diff, "rdy-diff", false, function () {
      return stored("rdy-diff") === "shown";
    }, function (shown) {
      diff.setAttribute("aria-pressed", shown ? "true" : "false");
      if (shown) root.setAttribute("data-diff", "shown");
      else root.removeAttribute("data-diff");
      for (var j = 0; j < diffs.length; j++) diffs[j].hidden = !shown;
    });
  }
})();
