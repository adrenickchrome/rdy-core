// Copy link, Copy ref, Copy and Cite: the toolbar of a block, and the tap that shows it on a touch screen. No context menu is touched.
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

  function inEyebrow(node) {
    var el = elementOf(node);
    return !!(el && el.closest(".eyebrow"));
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

  // Copy: the Markdown of a range of the page's payload, the one template the page carries. The
  // template's parsed text is in its content; each link slot is resolved against the page's own
  // address and written as the export writes a link target. Null when the range or a slot does
  // not hold: nothing is copied, and nothing is ever requested.
  var ESCAPED = { " ": "%20", "(": "%28", ")": "%29", "<": "%3C", ">": "%3E" };

  function linkTarget(url) {
    return url
      .replace(/[\r\n\t]/g, "")
      .replace(/[ ()<>]/g, function (c) { return ESCAPED[c]; })
      .replace(/&(?=#?[A-Za-z0-9]+;)/g, "&amp;");
  }

  function index(btn, name) {
    var v = btn.getAttribute(name);
    return v !== null && /^(0|[1-9][0-9]{0,8})$/.test(v) ? Number(v) : -1;
  }

  function markdownOf(btn) {
    try {
      var tpl = document.querySelector("template[data-copy-page]");
      if (!tpl || !tpl.content) return null;
      var blocks = JSON.parse(tpl.content.textContent).blocks;
      var start = index(btn, "data-copy-start");
      var end = index(btn, "data-copy-end");
      if (!Array.isArray(blocks) || start < 0 || end <= start || end > blocks.length) return null;
      var out = [];
      for (var i = start; i < end; i++) {
        var parts = blocks[i];
        if (!Array.isArray(parts)) return null;
        var md = "";
        for (var j = 0; j < parts.length; j++) {
          var p = parts[j];
          if (!p || typeof p.value !== "string") return null;
          if (p.kind === "text") md += p.value;
          else if (p.kind === "url") md += linkTarget(new URL(p.value, location.href).href);
          else return null;
        }
        out.push(md);
      }
      return out.join("\n");
    } catch (e) {
      return null;
    }
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
    // the eyebrow above a page's title names its folder, not the page: no words of it are cited
    if (inEyebrow(sel.anchorNode) || inEyebrow(sel.focusNode)) {
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

  // a tap (touch or pen; a mouse has hover) on a block shows its toolbar and hides every other;
  // a second tap hides it. A tap on a link, a control or the toolbar itself does not toggle.
  document.addEventListener("pointerup", function (ev) {
    if (ev.pointerType !== "touch" && ev.pointerType !== "pen") return;
    var t = elementOf(ev.target);
    if (t && t.closest && t.closest(".tools, a, button, summary, input, textarea, select, label")) return;
    var sel = window.getSelection ? window.getSelection() : null;
    if (sel && sel.rangeCount > 0 && !sel.isCollapsed) return;
    var block = blockOf(t);
    var open = document.querySelectorAll(".tools-open");
    for (var i = 0; i < open.length; i++) {
      if (open[i] !== block) open[i].classList.remove("tools-open");
    }
    if (block) block.classList.toggle("tools-open");
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
    } else if (btn.classList.contains("copy-text")) {
      var md = markdownOf(btn);
      if (md === null) flash(btn, false);
      else copyText(md, function (ok) { flash(btn, ok); });
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
