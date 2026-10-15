// Review mode: the buttons in the review bar. Show changes hides and shows the Edited, New and Removed marks. Show diff shows each edited block's old and new text, word by word, in place of its text. Show diff depends on Show changes: while the changes are hidden it is unpressed and disabled (aria-disabled, so it stays in the tab order), shows no diff and ignores a click; its own choice comes back with the changes. Without a button (a canon or tag render) nothing happens. Each choice lasts the browser session; where storage is blocked the choice holds for this page only.
(function () {
  "use strict";
  var root = document.documentElement;

  function stored(key) {
    try {
      return sessionStorage.getItem(key);
    } catch (e) {
      return null;
    }
  }

  // remembers `on` under `key`; the default (`initial`) is stored as no entry
  function remember(key, on, initial, word) {
    try {
      if (on === initial) sessionStorage.removeItem(key);
      else sessionStorage.setItem(key, word);
    } catch (e) {
      // blocked storage: the choice holds for this page only
    }
  }

  var changes = document.getElementById("show-changes");
  var diff = document.getElementById("show-diff");
  var diffs = [];
  if (diff) {
    // the root carries data-diff while the diffs are shown, and is not one of them
    var found = document.querySelectorAll("[data-diff]");
    for (var i = 0; i < found.length; i++) if (found[i] !== root) diffs.push(found[i]);
  }
  // Show changes: on by default; "hidden" is what is stored
  var changesOn = stored("rdy-changes") !== "hidden";
  // Show diff's own choice: off by default; "shown" is what is stored. The button waits for a
  // page that holds a diff.
  var diffOn = stored("rdy-diff") === "shown";
  var hasDiff = !!(diff && diffs.length);

  function apply() {
    if (changes) {
      changes.setAttribute("aria-pressed", changesOn ? "true" : "false");
      if (changesOn) root.removeAttribute("data-changes");
      else root.setAttribute("data-changes", "hidden");
    }
    if (!diff) return;
    // without Show changes, Show diff has nothing to show: unpressed and disabled
    var blocked = !!changes && !changesOn;
    var shown = hasDiff && diffOn && !blocked;
    diff.setAttribute("aria-pressed", shown ? "true" : "false");
    if (blocked) diff.setAttribute("aria-disabled", "true");
    else diff.removeAttribute("aria-disabled");
    if (!hasDiff) return;
    if (shown) root.setAttribute("data-diff", "shown");
    else root.removeAttribute("data-diff");
    for (var j = 0; j < diffs.length; j++) diffs[j].hidden = !shown;
  }

  apply();
  if (changes) {
    changes.hidden = false;
    changes.addEventListener("click", function () {
      changesOn = !changesOn;
      apply();
      remember("rdy-changes", changesOn, true, "hidden");
    });
  }
  if (hasDiff) {
    diff.hidden = false;
    diff.addEventListener("click", function () {
      if (changes && !changesOn) return;
      diffOn = !diffOn;
      apply();
      remember("rdy-diff", diffOn, false, "shown");
    });
  }
})();
