// Live countdown to the 12:42 Europe/Vienna deadline. The server sends the
// remaining seconds, so a wrong clock on the visitor's machine doesn't matter.
(function () {
  var el = document.querySelector('.countdown');
  if (!el) return;

  var left = parseInt(el.dataset.seconds, 10);
  if (!isFinite(left)) return;

  // Same flat-24-hour bar as stats::progress_bar in stats.rs.
  var bar = document.querySelector('.meter-bar');
  var pct = document.querySelector('.meter-pct');
  var DAY = 24 * 60 * 60;
  var CELLS = 30;

  function pad(n) { return n < 10 ? '0' + n : String(n); }

  function repeat(ch, n) { return new Array(n + 1).join(ch); }

  function label(total) {
    if (total <= 0) return 'closing…';
    var h = Math.floor(total / 3600);
    var m = Math.floor((total % 3600) / 60);
    var s = total % 60;
    return h > 0 ? h + ':' + pad(m) + ':' + pad(s) : m + ':' + pad(s);
  }

  function render() {
    el.textContent = label(left);
    var done = Math.min(Math.max(DAY - left, 0), DAY);
    var filled = Math.floor(done * CELLS / DAY);
    if (bar) bar.textContent = '[' + repeat('#', filled) + repeat('-', CELLS - filled) + ']';
    if (pct) pct.textContent = Math.floor(done * 100 / DAY) + '%';
  }

  render();

  var timer = setInterval(function () {
    left -= 1;
    render();
    if (left <= 0) {
      clearInterval(timer);
      // The round just rolled over; pull the fresh one.
      setTimeout(function () { window.location.assign('/'); }, 1500);
    }
  }, 1000);
})();

// Vienna wall-clock time in the header - the clock the game runs on.
(function () {
  var el = document.querySelector('.clock');
  if (!el || !window.Intl) return;
  var fmt = new Intl.DateTimeFormat('en-GB', {
    timeZone: 'Europe/Vienna', hour: '2-digit', minute: '2-digit'
  });
  function tick() { el.textContent = fmt.format(new Date()) + ' vienna'; }
  tick();
  setInterval(tick, 10 * 1000);
})();

// Loading feedback, the way a CLI does it: a braille spinner in the header
// while the next page loads, and in the button that sent a form. Also
// stops a double click from sending the same form twice.
(function () {
  var FRAMES = '⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏';
  var status = document.querySelector('.busy');
  var timer = null;
  var busyButton = null;
  var busyLabel = '';

  function spin(text) {
    var i = 0;
    stop();
    function frame() {
      var f = FRAMES.charAt(i++ % FRAMES.length);
      if (status) status.textContent = f + ' ' + text;
      if (busyButton) busyButton.textContent = f + ' sending';
    }
    frame();
    timer = setInterval(frame, 80);
  }

  function stop() {
    if (timer) clearInterval(timer);
    timer = null;
    if (status) status.textContent = '';
  }

  document.addEventListener('click', function (e) {
    if (e.defaultPrevented || e.button !== 0) return;
    if (e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return; // new tab/window
    var a = e.target.closest && e.target.closest('a[href]');
    if (!a || a.target || a.hasAttribute('download')) return;
    var url = new URL(a.href, location.href);
    if (url.origin !== location.origin) return;
    if (url.pathname === location.pathname && url.search === location.search && url.hash) return;
    spin(url.pathname === '/login' ? 'connecting to intra' : 'loading ' + url.pathname);
  });

  document.addEventListener('submit', function (e) {
    var form = e.target;
    if (form.dataset.sent) {
      e.preventDefault();
      return;
    }
    form.dataset.sent = '1';
    // Not `disabled`: a disabled button drops its name=value from the form,
    // and the admin clock buttons rely on theirs. Changing its text is fine;
    // the submitted value comes from the value attribute.
    busyButton = e.submitter || form.querySelector('button[type=submit]');
    if (busyButton) {
      busyLabel = busyButton.textContent;
      busyButton.classList.add('is-busy');
    }
    spin('sending ' + (form.getAttribute('action') || location.pathname));
  });

  // Coming back with the Back button can restore this page from memory,
  // mid-load; put it back to rest.
  window.addEventListener('pageshow', function (e) {
    if (!e.persisted) return;
    stop();
    if (busyButton) {
      busyButton.textContent = busyLabel;
      busyButton.classList.remove('is-busy');
      busyButton = null;
    }
    document.querySelectorAll('form[data-sent]').forEach(function (f) { delete f.dataset.sent; });
  });
})();
