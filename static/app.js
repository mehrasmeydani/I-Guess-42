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
  // How many cells fit is a question about the screen, and the server cannot
  // see the screen: it paints a short bar that fits the narrowest phone, and
  // fit() widens it here. Without this file the short one is what stays.
  var CELLS = 30;

  // The bar is monospace text, so one cell is one character: measure the row
  // it lives on, take off what the percentage beside it needs, and the rest
  // divided by a character's width is the cell count.
  function fit() {
    if (!bar) return;
    var row = bar.parentNode;
    var text = bar.textContent || '';
    if (!row || !text.length) return;
    var ch = bar.offsetWidth / text.length;
    if (!(ch > 0)) return;
    // Reserve the percentage's widest reading, "100%", plus a character of
    // gap -- measuring the label as it stands now would fit the bar around
    // "0%" in the morning and push it off the edge by the afternoon.
    var room = row.clientWidth - 5 * ch;
    // The two brackets are not cells.
    var cells = Math.floor(room / ch) - 2;
    if (!isFinite(cells)) return;
    CELLS = Math.max(10, Math.min(300, cells));
  }

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

  fit();
  render();

  // A rotated phone or a dragged window is a different number of cells.
  var refit = null;
  window.addEventListener('resize', function () {
    if (refit) clearTimeout(refit);
    refit = setTimeout(function () { fit(); render(); }, 120);
  });

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
  // Frames go to .spin, which is hidden from screen readers; the message goes
  // once to .busy-text (role=status), so it is announced once, not 12x a second.
  var glyph = document.querySelector('.busy .spin');
  var status = document.querySelector('.busy .busy-text');
  var timer = null;
  var busyButton = null;
  var busyLabel = '';

  function spin(text) {
    var i = 0;
    stop();
    if (status) status.textContent = text;
    function frame() {
      var f = FRAMES.charAt(i++ % FRAMES.length);
      if (glyph) glyph.textContent = f;
      if (busyButton) busyButton.textContent = f + ' sending';
    }
    frame();
    timer = setInterval(frame, 80);
  }

  function stop() {
    if (timer) clearInterval(timer);
    timer = null;
    if (glyph) glyph.textContent = '';
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
      // Screen readers hear "sending", not a new braille frame every 80ms.
      busyButton.setAttribute('aria-busy', 'true');
      busyButton.setAttribute('aria-label', 'sending');
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
      busyButton.removeAttribute('aria-busy');
      busyButton.removeAttribute('aria-label');
      busyButton = null;
    }
    document.querySelectorAll('form[data-sent]').forEach(function (f) { delete f.dataset.sent; });
  });
})();

// Charts that start below the fold hold their bars until they are scrolled
// to, so the columns grow while someone is watching instead of before they
// get there. The page works the same without this: no IntersectionObserver,
// or reduced motion asked for, and nothing is ever held.
(function () {
  var charts = document.querySelectorAll('.chart');
  if (!charts.length || !window.IntersectionObserver) return;
  if (window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches) return;

  var held = [];
  for (var i = 0; i < charts.length; i++) {
    // A chart already on screen animates now; holding it would only delay
    // the one the visitor is looking at.
    if (charts[i].getBoundingClientRect().top < window.innerHeight) continue;
    charts[i].classList.add('hold');
    held.push(charts[i]);
  }
  if (!held.length) return;

  // A tenth of the viewport of overlap, so the bars start once the chart is
  // properly in view rather than at the first pixel of it.
  var io = new IntersectionObserver(function (entries) {
    entries.forEach(function (e) {
      if (!e.isIntersecting) return;
      e.target.classList.add('seen');
      io.unobserve(e.target);
    });
  }, { rootMargin: '0px 0px -10% 0px' });

  held.forEach(function (c) { io.observe(c); });
})();

// The round that just closed, announced for half an hour. The panel and its
// dismiss control are both server-rendered and work on their own; this only
// makes a dismissal stick across a reload, and takes the panel down when the
// half hour is up instead of leaving it there until the next page load.
(function () {
  var box = document.querySelector('.announce-x');
  if (!box) return;
  var wrap = box.parentNode;
  var round = box.dataset.round || '';
  var KEY = 'ig42-announce-seen';

  // Private windows and blocked site data throw on the first touch, not on
  // use, so ask once and carry on without it.
  var store = null;
  try { store = window.localStorage; } catch (e) { store = null; }

  // Only this round's dismissal counts: tomorrow's result is a new one.
  try {
    if (store && store.getItem(KEY) === round) box.checked = true;
  } catch (e) { /* unreadable: show it, which is the safe way round */ }

  box.addEventListener('change', function () {
    if (!store) return;
    try {
      if (box.checked) store.setItem(KEY, round);
      else store.removeItem(KEY);
    } catch (e) { /* full or refused: dismissing still works, it just forgets */ }
  });

  var left = parseInt(wrap.dataset.left, 10);
  if (isFinite(left) && left > 0) {
    setTimeout(function () { wrap.parentNode.removeChild(wrap); }, left * 1000);
  }
})();
