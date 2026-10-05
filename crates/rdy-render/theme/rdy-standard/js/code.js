(function () {
  'use strict';
  // The copy-code control: a click on a `.copy-code` button copies the text of the `<code>`
  // beside it and says `Copied` for a moment.
  function codeOf(button) {
    for (var el = button.parentElement; el; el = el.parentElement) {
      var code = el.querySelector('code');
      if (code) return code;
    }
    return null;
  }
  function select(code) {
    var range = document.createRange();
    range.selectNodeContents(code);
    var sel = window.getSelection();
    sel.removeAllRanges();
    sel.addRange(range);
  }
  document.addEventListener('click', function (event) {
    var button = event.target.closest && event.target.closest('.copy-code');
    if (!button) return;
    var code = codeOf(button);
    if (!code) return;
    var label = button.getAttribute('data-label') || button.textContent;
    button.setAttribute('data-label', label);
    function done() {
      button.textContent = 'Copied';
      clearTimeout(button._copyTimer);
      button._copyTimer = setTimeout(function () {
        button.textContent = label;
      }, 1500);
    }
    if (navigator.clipboard && navigator.clipboard.writeText) {
      navigator.clipboard.writeText(code.textContent).then(done, function () {
        select(code);
      });
    } else {
      select(code);
      try {
        if (document.execCommand('copy')) done();
      } catch (e) {}
    }
  });
})();
