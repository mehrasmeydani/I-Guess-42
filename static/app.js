// The countdown to the round's close. The server sends the deadline and its
// own clock; the gap between that clock and this machine's is measured once
// and again whenever the tab comes back into view, so a visitor's wrong
// clock, or a laptop that slept, never moves the deadline.
(function () {
  var el = document.querySelector('.countdown[data-closes-at]');
  if (!el) return;

  var closesAt = Date.parse(el.dataset.closesAt);
  var offset = Date.parse(el.dataset.serverNow) - Date.now();
  if (!isFinite(closesAt) || !isFinite(offset)) return;

  var DAY = 24 * 60 * 60 * 1000;
  var bar = document.querySelector('.progress');
  var cells = document.querySelectorAll('.progress .cells i');
  var pctLabel = document.querySelector('.progress-pct');
  var mirrors = document.querySelectorAll('.countdown-mirror');
  var stateEl = document.querySelector('.roundstatus .state');
  var closesIn = document.querySelector('.closes-in');
  var lastMinute = null;
  var timer = null;

  function pad(n) { return n < 10 ? '0' + n : String(n); }
  function remaining() { return Math.max(0, closesAt - (Date.now() + offset)); }

  // Same shapes as round::format_duration and round::spoken_duration.
  function short(ms) {
    var t = Math.floor(ms / 1000);
    var h = Math.floor(t / 3600), m = Math.floor((t % 3600) / 60), s = t % 60;
    if (h > 0) return h + 'h ' + pad(m) + 'm';
    if (m > 0) return m + 'm ' + pad(s) + 's';
    return s + 's';
  }
  function spoken(ms) {
    var t = Math.floor(ms / 1000);
    var h = Math.floor(t / 3600), m = Math.floor((t % 3600) / 60);
    function unit(n, w) { return n + ' ' + w + (n === 1 ? '' : 's'); }
    if (!h && !m) return 'less than a minute';
    if (!h) return unit(m, 'minute');
    if (!m) return unit(h, 'hour');
    return unit(h, 'hour') + ' ' + unit(m, 'minute');
  }

  function render() {
    var ms = remaining();
    var text = short(ms);
    el.textContent = text;
    for (var i = 0; i < mirrors.length; i++) mirrors[i].textContent = text;
    // What is read out and the bar only change once a minute.
    var minute = Math.floor(ms / 60000);
    if (minute !== lastMinute) {
      lastMinute = minute;
      el.setAttribute('aria-label', 'closes in ' + spoken(ms));
      if (bar && cells.length) {
        var pct = Math.min(100, Math.max(0, Math.floor((DAY - ms) * 100 / DAY)));
        bar.setAttribute('aria-valuenow', pct);
        bar.setAttribute('aria-valuetext', pct + '% of the round gone');
        // As templates::progress_cells: floor(pct * cells / 100) filled.
        var filled = Math.floor(pct * cells.length / 100);
        for (var c = 0; c < cells.length; c++) cells[c].className = c < filled ? 'on' : '';
        if (pctLabel) pctLabel.textContent = pct + '%';
      }
    }
    if (ms <= 0) closed();
  }

  // The deadline passed with the page open: say so, and wait for the server
  // to have moved on to the next round before going to the results.
  function closed() {
    clearInterval(timer);
    el.textContent = 'revealing…';
    el.setAttribute('aria-label', 'round closed, revealing the numbers');
    if (stateEl) stateEl.textContent = '[closed]';
    if (closesIn) closesIn.textContent = 'round closed · revealing…';
    var closedRound = (document.querySelector('input[name=round]') || {}).value
      || el.dataset.closesAt.slice(0, 10);
    var base = location.pathname.indexOf('/g') === 0 ? '/g' : '';
    (function poll(tries) {
      fetch('/api/round', { cache: 'no-store' })
        .then(function (r) { return r.json(); })
        .then(function (now) {
          if (now.round !== closedRound) {
            location.assign(base + '/day/' + closedRound);
          } else if (tries < 30) {
            setTimeout(function () { poll(tries + 1); }, 2000);
          }
        })
        .catch(function () { if (tries < 30) setTimeout(function () { poll(tries + 1); }, 2000); });
    })(0);
  }

  // Back from a hidden tab or a sleeping laptop: ask the server for the time
  // again rather than trust a clock that may have drifted while away.
  document.addEventListener('visibilitychange', function () {
    if (document.visibilityState !== 'visible') return;
    fetch('/api/round', { cache: 'no-store' })
      .then(function (r) { return r.json(); })
      .then(function (now) {
        var server = Date.parse(now.server_now);
        if (isFinite(server)) offset = server - Date.now();
        lastMinute = null;
        render();
      })
      .catch(function () { render(); });
  });

  render();
  timer = setInterval(render, 1000);
})();

// Vienna wall-clock time in the header - the clock the game runs on, which
// is the server's: on a test instance an admin may have moved it, and the
// header must agree with the round it shows. The test banner says so.
(function () {
  var el = document.querySelector('.clock');
  if (!el || !window.Intl) return;
  var fmt = new Intl.DateTimeFormat('en-GB', {
    timeZone: 'Europe/Vienna', hour: '2-digit', minute: '2-digit'
  });
  var offset = 0;
  function tick() { el.textContent = fmt.format(new Date(Date.now() + offset)) + ' vienna'; }
  tick();
  setInterval(tick, 10 * 1000);
  if (!window.fetch) return;
  fetch('/api/round', { cache: 'no-store' })
    .then(function (r) { return r.json(); })
    .then(function (now) {
      var server = Date.parse(now.server_now);
      if (isFinite(server)) offset = server - Date.now();
      tick();
      var shifted = document.querySelector('.testbanner .shifted');
      if (shifted && now.clock_offset) {
        var s = Math.abs(now.clock_offset), d = Math.floor(s / 86400), h = Math.floor((s % 86400) / 3600);
        shifted.textContent = ' · game clock moved ' + (now.clock_offset > 0 ? '+' : '-')
          + (d ? d + 'd ' : '') + h + 'h';
      }
    })
    .catch(function () {});
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
    spin(url.pathname.indexOf('/login/') === 0 ? 'connecting…' : 'loading…');
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
      // A button without a name sends nothing of its own, so it can be truly
      // disabled; one with a name (the admin clock) cannot, or its value
      // would be dropped. Disabled after this event, so the form still goes.
      if (!busyButton.name) {
        var b = busyButton;
        setTimeout(function () { b.disabled = true; }, 0);
      }
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
      busyButton.disabled = false;
      busyButton.removeAttribute('aria-busy');
      busyButton.removeAttribute('aria-label');
      busyButton = null;
    }
    document.querySelectorAll('form[data-sent]').forEach(function (f) { delete f.dataset.sent; });
  });
})();

// "share result" on a day page: the text is built on the server and never
// holds the player's own number. The share sheet where there is one, the
// clipboard otherwise, and a word to say it worked.
(function () {
  var button = document.querySelector('button.share[data-share]');
  if (!button) return;
  var done = document.querySelector('.share-done');
  function say(text) { if (done) done.textContent = text; }
  button.addEventListener('click', function () {
    var text = button.dataset.share;
    if (navigator.share) {
      navigator.share({ text: text }).catch(function () {});
      return;
    }
    if (navigator.clipboard && navigator.clipboard.writeText) {
      navigator.clipboard.writeText(text).then(
        function () { say('copied - paste it anywhere'); },
        function () { say('could not copy; select the text below'); }
      );
    } else {
      say(text);
    }
  });
})();

// A form that came back with errors: move focus to the summary, so it is
// read out and the first fix is one Tab away.
(function () {
  var summary = document.querySelector('.error-summary');
  if (summary) summary.focus();
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
