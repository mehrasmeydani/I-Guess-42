// Live countdown to the 12:42 Europe/Vienna deadline. The server sends the
// remaining seconds, so a wrong clock on the visitor's machine doesn't matter.
(function () {
  var el = document.querySelector('.countdown');
  if (!el) return;

  var left = parseInt(el.dataset.seconds, 10);
  if (!isFinite(left)) return;

  function pad(n) { return n < 10 ? '0' + n : String(n); }

  function label(total) {
    if (total <= 0) return 'closing…';
    var h = Math.floor(total / 3600);
    var m = Math.floor((total % 3600) / 60);
    var s = total % 60;
    return h > 0 ? h + ':' + pad(m) + ':' + pad(s) : m + ':' + pad(s);
  }

  el.textContent = label(left);

  var timer = setInterval(function () {
    left -= 1;
    el.textContent = label(left);
    if (left <= 0) {
      clearInterval(timer);
      // The round just rolled over; pull the fresh one.
      setTimeout(function () { window.location.assign('/'); }, 1500);
    }
  }, 1000);
})();
