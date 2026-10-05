// Copy link, Copy ref and Cite: the toolbar of a block. No context menu is touched.
(function () {
  "use strict";
  var COPIED_MS = 1500;
  var CITE_MAX = 300;

  function copyText(text, done) {
    function fallback() {
      try {
        var t = document.createElement("textarea");
        t.value = text;
        t.setAttribute("readonly", "");
        t.style.cssText = "position:fixed;top:0;left:0;opacity:0";
        document.body.appendChild(t);
        t.select();
        var ok = document.execCommand("copy");
        document.body.removeChild(t);
        done(ok);
      } catch (e) {
        done(false);
      }
    }
    if (navigator.clipboard && navigator.clipboard.writeText) {
      navigator.clipboard.writeText(text).then(function () { done(true); }, fallback);
    } else {
      fallback();
    }
  }

  // the button says Copied for a moment, then goes back to what it said
  function flash(btn, ok) {
    if (btn._label === undefined) btn._label = btn.textContent;
    btn.textContent = ok ? "Copied" : "Not copied";
    clearTimeout(btn._timer);
    btn._timer = setTimeout(function () {
      btn.textContent = btn._label;
      btn._label = undefined;
    }, COPIED_MS);
  }

  function elementOf(node) {
    return node && node.nodeType === 1 ? node : node && node.parentElement;
  }

  function blockOf(el) {
    el = elementOf(el);
    if (!el) return null;
    var own = el.closest("[data-rdy-block]");
    if (own) return own;
    // a toolbar that sits just before its block
    var bar = el.closest(".tools");
    var next = bar && bar.nextElementSibling;
    return next && next.hasAttribute("data-rdy-block") ? next : null;
  }

  function toolbarOf(block) {
    var inside = block.querySelector(".tools");
    if (inside) return inside;
    var before = block.previousElementSibling;
    return before && before.classList.contains("tools") ? before : null;
  }

  // whitespace collapsed, straight quotes to ', cut at a word boundary with an ellipsis
  function quote(text) {
    var flat = text.replace(/\s+/g, " ").trim().replace(/"/g, "'");
    if (flat.length <= CITE_MAX) return flat;
    var cut = flat.charAt(CITE_MAX) === " " ? CITE_MAX : flat.lastIndexOf(" ", CITE_MAX - 1);
    if (cut <= 0) cut = CITE_MAX;
    return flat.slice(0, cut).replace(/\s+$/, "") + "…";
  }

  function removeCite() {
    var old = document.querySelectorAll(".tool.cite");
    for (var i = 0; i < old.length; i++) {
      if (old[i]._held) continue;
      old[i].parentNode.removeChild(old[i]);
    }
  }

  function showCite() {
    var sel = window.getSelection ? window.getSelection() : null;
    if (!sel || sel.rangeCount === 0 || sel.isCollapsed || !sel.toString().trim()) {
      removeCite();
      return;
    }
    var a = blockOf(sel.anchorNode);
    var b = blockOf(sel.focusNode);
    var bar = a && a === b ? toolbarOf(a) : null;
    if (!bar || !bar.querySelector(".copy-ref")) {
      removeCite();
      return;
    }
    var have = bar.querySelector(".cite");
    removeCite();
    if (have && have.parentNode) {
      have._text = sel.toString();
      return;
    }
    var btn = document.createElement("button");
    btn.type = "button";
    btn.className = "tool cite";
    btn.textContent = "Cite";
    btn.setAttribute("aria-label", "Copy the selected words with a reference for an agent chat");
    btn._text = sel.toString();
    btn._block = a;
    btn.addEventListener("pointerdown", function () { btn._held = true; });
    btn.addEventListener("mousedown", function () { btn._held = true; });
    bar.appendChild(btn);
  }

  document.addEventListener("selectionchange", function () {
    var active = document.activeElement;
    if (active && active.classList && active.classList.contains("cite")) return;
    showCite();
  });

  document.addEventListener("click", function (ev) {
    var t = elementOf(ev.target);
    var btn = t && t.closest ? t.closest(".tool") : null;
    if (!btn) return;
    if (btn.classList.contains("copy-link")) {
      var base = location.href.split("#")[0];
      var url;
      try {
        url = new URL(btn.getAttribute("data-href") || "", base).href;
      } catch (e) {
        url = base + (btn.getAttribute("data-href") || "");
      }
      copyText(url, function (ok) { flash(btn, ok); });
    } else if (btn.classList.contains("copy-ref")) {
      var block = blockOf(btn);
      var ref = block && block.getAttribute("data-ref");
      if (ref) copyText(ref, function (ok) { flash(btn, ok); });
    } else if (btn.classList.contains("cite")) {
      var holder = btn._block || blockOf(btn);
      var head = holder && holder.getAttribute("data-ref-head");
      if (head) {
        copyText(head + ': "' + quote(btn._text || "") + '"', function (ok) {
          btn._held = false;
          flash(btn, ok);
        });
      }
    }
  });
})();
