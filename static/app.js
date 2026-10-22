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
  var bar = document.querySelector('.meter');
  var barText = document.querySelector('.meter-bar');
  var pctLabel = document.querySelector('.meter-pct');
  var stateEl = document.querySelector('#round-h .state');
  // The server paints 30 cells, which fit the narrowest phone; fit() makes
  // it as many as the row holds. The bar is monospace text, so one cell is
  // one character: the row's width, less room for "100%", over a
  // character's width.
  var CELLS = 30;
  function fit() {
    if (!barText) return;
    var text = barText.textContent || '';
    var ch = barText.offsetWidth / text.length;
    if (!(ch > 0)) return;
    var cells = Math.floor((bar.clientWidth - 5 * ch) / ch) - 2; // less the brackets
    if (isFinite(cells)) CELLS = Math.max(10, Math.min(300, cells));
  }
  function repeat(c, n) { return new Array(n + 1).join(c); }
  var lastMinute = null;
  var timer = null;

  function pad(n) { return n < 10 ? '0' + n : String(n); }
  function remaining() { return Math.max(0, closesAt - (Date.now() + offset)); }

  // Same shapes as round::clock and round::spoken_duration.
  function short(ms) {
    var t = Math.floor(ms / 1000);
    var h = Math.floor(t / 3600), m = Math.floor((t % 3600) / 60), s = t % 60;
    return h > 0 ? h + ':' + pad(m) + ':' + pad(s) : m + ':' + pad(s);
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

  // As templates::meter: floor(pct * cells / 100) filled.
  function paint(ms) {
    if (!bar || !barText) return;
    var pct = Math.min(100, Math.max(0, Math.floor((DAY - ms) * 100 / DAY)));
    bar.setAttribute('aria-valuenow', pct);
    bar.setAttribute('aria-valuetext', pct + '% of the round gone');
    var filled = Math.floor(pct * CELLS / 100);
    barText.textContent = '[' + repeat('#', filled) + repeat('-', CELLS - filled) + ']';
    if (pctLabel) pctLabel.textContent = pct + '%';
  }

  function render() {
    var ms = remaining();
    var text = short(ms);
    el.textContent = text;
    // What is read out and the bar only change once a minute.
    var minute = Math.floor(ms / 60000);
    if (minute !== lastMinute) {
      lastMinute = minute;
      el.setAttribute('aria-label', 'closes in ' + spoken(ms));
      paint(ms);
    }
    if (ms <= 0) closed();
  }

  // The deadline passed with the page open: say so, and wait for the server
  // to have moved on to the next round before going to the results.
  function closed() {
    clearInterval(timer);
    el.textContent = 'revealing…';
    el.setAttribute('aria-label', 'round closed, revealing the numbers');
    if (stateEl) stateEl.textContent = 'closed';
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

  fit();
  render();
  timer = setInterval(render, 1000);
  // A rotated phone or a dragged window holds a different number of cells.
  var refit = null;
  window.addEventListener('resize', function () {
    clearTimeout(refit);
    refit = setTimeout(function () { fit(); paint(remaining()); }, 120);
  });
})();

// Vienna wall-clock time in the header - the clock the game runs on, which
// is the server's: on a test instance an admin may have moved it, and the
// header must agree with the round it shows. The test banner says so.
(function () {
  var el = document.querySelector('.clock');
  var fmt = window.Intl && new Intl.DateTimeFormat('en-GB', {
    timeZone: 'Europe/Vienna', hour: '2-digit', minute: '2-digit'
  });
  var offset = 0;
  function tick() {
    if (el && fmt) el.textContent = fmt.format(new Date(Date.now() + offset)) + ' vienna';
  }
  tick();
  if (el) setInterval(tick, 10 * 1000);
  // Only a moved test clock needs the server's time here.
  if (!window.fetch || !document.querySelector('.testbanner .shifted')) return;
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

// The round switch keeps you on the same kind of page: results to results,
// trends to trends. Bets exist only in the global round, so from there the
// 42 side goes to its round page.
(function () {
  var nav = document.querySelector('nav.pools[data-here]');
  if (!nav) return;
  var here = nav.dataset.here || '/';
  var links = nav.querySelectorAll('a[data-pool]');
  for (var i = 0; i < links.length; i++) {
    var base = links[i].dataset.pool;
    var path = (here === '/bets' && base === '') ? '/' : base + here;
    links[i].setAttribute('href', path === '/g/' || path === '/g' ? '/g/' : path);
  }
})();

// Bets: the quote follows the two boxes as you type, without the button.
// Each change asks the server for just the quote box (odds move with every
// bet, so they are never worked out here); an older answer arriving late
// is dropped. Without the script the button still does it as a page load.
(function () {
  var form = document.querySelector('form.pick[data-quote]');
  var box = document.getElementById('quote');
  if (!form || !box || !window.fetch) return;
  form.classList.add('live');
  var lo = form.elements.lo, hi = form.elements.hi;
  var timer = null, asked = 0;

  function update() {
    var on = form.querySelector('input[name=on]:checked');
    var q = 'on=' + (on ? on.value : 'win') + '&lo=' + encodeURIComponent(lo.value.trim()) + '&hi=' + encodeURIComponent(hi.value.trim());
    var n = ++asked;
    // Reloading or sharing the page keeps the range.
    try { history.replaceState(null, '', lo.value.trim() || hi.value.trim() ? '/g/bets?' + q : '/g/bets'); } catch (e) {}
    fetch(form.dataset.quote + '?' + q, { credentials: 'same-origin' })
      .then(function (r) { return r.ok ? r.text() : null; })
      .then(function (html) { if (html !== null && n === asked) box.innerHTML = html; })
      .catch(function () { /* offline: the button still works */ });
    var none = form.querySelector('.pick-none.on');
    if (none) none.classList.remove('on');
  }

  function soon() {
    clearTimeout(timer);
    timer = setTimeout(update, 150);
  }
  lo.addEventListener('input', soon);
  hi.addEventListener('input', soon);
  // Winner or most picked: the same range, priced on the other book.
  var kinds = form.querySelectorAll('input[name=on]');
  for (var k = 0; k < kinds.length; k++) kinds[k].addEventListener('change', update);
  form.addEventListener('submit', function (e) { e.preventDefault(); clearTimeout(timer); update(); });

  // What the stake typed pays if the bet wins, stake included, as
  // odds::payout works it out: floor(stake * odds / 100). On the box, not
  // the input, since the quote inside is swapped out as the range changes.
  box.addEventListener('input', function (e) {
    if (e.target.name !== 'stake') return;
    var pays = box.querySelector('.pays');
    if (!pays) return;
    var stake = parseInt(e.target.value.replace(/[^0-9]/g, ''), 10);
    // No more than the stake that reaches the cap: past it a win pays no
    // more, so the box stops there and the reason under it lights up.
    var most = parseInt(e.target.dataset.max, 10);
    var why = box.querySelector('.cap-why');
    if (most > 0 && stake > most) {
      stake = most;
      e.target.value = String(most);
      if (why) why.classList.add('hit');
    } else if (why) {
      why.classList.remove('hit');
    }
    var odds = parseInt(pays.dataset.odds, 10);
    var cap = parseInt(pays.dataset.cap, 10);
    if (!(stake > 0) || !(odds > 0)) { pays.textContent = '\u2013'; return; }
    // As odds::payout: the profit is capped by the size of the game.
    var back = Math.floor(stake * odds / 100);
    var capped = cap >= 0 && back - stake > cap;
    if (capped) back = stake + cap;
    pays.innerHTML = back + ' <small>+' + (back - stake) + (capped ? ' \u00b7 capped' : '') + '</small>';
  });
})();

// The robot check on the sign-up form (human.rs): find a nonce whose
// SHA-256 of "salt:nonce" starts with the asked-for number of zero bits.
// Worked in small slices so the page never freezes; started when the box
// is ticked, and on submit if it has not finished yet.
(function () {
  var box = document.querySelector('.captcha[data-bits]');
  if (!box) return;
  var form = box.closest('form');
  var check = box.querySelector('.captcha-check');
  var text = box.querySelector('.captcha-text');
  var token = box.querySelector('input[name=captcha]').value;
  var nonceField = box.querySelector('input[name=nonce]');
  var bits = parseInt(box.dataset.bits, 10) || 0;
  var salt = token.split('.')[0];
  var K = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2
  ];
  var w = new Int32Array(64);
  function rotr(x, n) { return (x >>> n) | (x << (32 - n)); }

  // The first 32 bits of SHA-256(msg), for an ASCII msg under 56 bytes
  // (one block): all the check needs.
  function head(msg) {
    var i, n = msg.length;
    for (i = 0; i < 16; i++) w[i] = 0;
    for (i = 0; i < n; i++) w[i >> 2] |= msg.charCodeAt(i) << (24 - (i % 4) * 8);
    w[n >> 2] |= 0x80 << (24 - (n % 4) * 8);
    w[15] = n * 8;
    for (i = 16; i < 64; i++) {
      var a = w[i - 15], b = w[i - 2];
      var s0 = rotr(a, 7) ^ rotr(a, 18) ^ (a >>> 3);
      var s1 = rotr(b, 17) ^ rotr(b, 19) ^ (b >>> 10);
      w[i] = (w[i - 16] + s0 + w[i - 7] + s1) | 0;
    }
    var A = 0x6a09e667, B = 0xbb67ae85, C = 0x3c6ef372, D = 0xa54ff53a,
        E = 0x510e527f, F = 0x9b05688c, G = 0x1f83d9ab, H = 0x5be0cd19;
    for (i = 0; i < 64; i++) {
      var S1 = rotr(E, 6) ^ rotr(E, 11) ^ rotr(E, 25);
      var t1 = (H + S1 + ((E & F) ^ (~E & G)) + K[i] + w[i]) | 0;
      var S0 = rotr(A, 2) ^ rotr(A, 13) ^ rotr(A, 22);
      var t2 = (S0 + ((A & B) ^ (A & C) ^ (B & C))) | 0;
      H = G; G = F; F = E; E = (D + t1) | 0; D = C; C = B; B = A; A = (t1 + t2) | 0;
    }
    return (A + 0x6a09e667) | 0;
  }
  window.__iglHead = head; // for checking against a reference in tests

  var state = 'idle', waiting = false, n = 0;
  function done() {
    state = 'done';
    check.checked = true;
    box.classList.remove('busy');
    box.classList.add('ok');
    text.textContent = 'not a robot ✓';
    if (waiting) form.submit();
  }
  function work() {
    var end = n + 20000;
    for (; n < end; n++) {
      if (Math.clz32(head(salt + ':' + n)) >= bits) { nonceField.value = String(n); return done(); }
    }
    setTimeout(work, 0);
  }
  function start() {
    if (state !== 'idle') return;
    state = 'busy';
    box.classList.add('busy');
    text.textContent = 'checking…';
    work();
  }
  check.addEventListener('change', function () {
    if (state === 'done') { check.checked = true; return; }
    check.checked = false; // ticked by the check, not by hand
    start();
  });
  form.addEventListener('submit', function (e) {
    if (state === 'done') return;
    e.preventDefault();
    waiting = true;
    start();
  });
})();
