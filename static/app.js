// Live countdown to the 12:42 Europe/Vienna deadline. The server sends the
// remaining seconds, so a wrong clock on the visitor's machine doesn't matter.
(function () {
  var el = document.querySelector('.countdown');
  if (!el) return;

  var left = parseInt(el.dataset.seconds, 10);
  if (!isFinite(left)) return;

  // Same flat-24-hour share as stats::progress_bar in stats.rs.
  var bar = document.querySelector('.meter-fill');
  var pct = document.querySelector('.meter-pct');
  var DAY = 24 * 60 * 60;

  function pad(n) { return n < 10 ? '0' + n : String(n); }

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
    var share = Math.floor(done * 100 / DAY);
    if (bar) bar.style.width = share + '%';
    if (pct) pct.textContent = share + '% of the day gone';
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
  function tick() { el.textContent = fmt.format(new Date()) + ' Vienna'; }
  tick();
  setInterval(tick, 10 * 1000);
})();

// Loading feedback: a small spinner and a message in the header while the
// next page loads, and "Sending…" on the button that sent a form. Also stops
// a double click from sending the same form twice.
(function () {
  // The spinner (.spin) is hidden from screen readers; the message goes once
  // to .busy-text (role=status), so it is announced once.
  var glyph = document.querySelector('.busy .spin');
  var status = document.querySelector('.busy .busy-text');
  var busyButton = null;
  var busyLabel = '';

  function spin(text) {
    stop();
    if (status) status.textContent = text;
    if (glyph) glyph.classList.add('spinning');
    if (busyButton) busyButton.textContent = 'Sending…';
  }

  function stop() {
    if (glyph) glyph.classList.remove('spinning');
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
    spin(url.pathname === '/login' ? 'Connecting to 42…' : 'Loading…');
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
      // Screen readers hear "sending", not a new braille frame every 80ms.
      busyButton.setAttribute('aria-busy', 'true');
      busyButton.setAttribute('aria-label', 'sending');
    }
    spin(form.method.toLowerCase() === 'get' ? 'Loading…' : 'Sending…');
  });

  // Coming back with the Back button can restore this page from memory,
  // mid-load; put it back to rest.
  window.addEventListener('pageshow', function (e) {
    if (!e.persisted) return;
    stop();
    if (busyButton) {
      busyButton.textContent = busyLabel;
      busyButton.classList.remove('is-busy');
      busyButton.removeAttribute('aria-busy');
      busyButton.removeAttribute('aria-label');
      busyButton = null;
    }
    document.querySelectorAll('form[data-sent]').forEach(function (f) { delete f.dataset.sent; });
  });
})();
