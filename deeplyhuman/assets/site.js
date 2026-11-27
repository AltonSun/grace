/* Deeply Human — small vanilla script: language, menu, accordions, slideshows, lightbox, lamp. */
var ICON = {
  play: '<svg viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><path d="M7 4.5v15l12.5-7.5z"/></svg>',
  pause: '<svg viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><rect x="6" y="4" width="4" height="16" rx="1"/><rect x="14" y="4" width="4" height="16" rx="1"/></svg>'
};
(function () {
  var KEY = 'dh-lang';
  var root = document.documentElement;

  function setLang(l) {
    root.setAttribute('data-lang', l);
    root.lang = l;
    try { localStorage.setItem(KEY, l); } catch (e) {}
    document.querySelectorAll('.lang-toggle').forEach(function (b) {
      var next = l === 'en' ? 'lt' : 'en';
      b.textContent = next.toUpperCase();
      b.setAttribute('aria-label', next === 'lt' ? 'Perjungti į lietuvių kalbą' : 'Switch to English');
    });
    document.querySelectorAll('[data-title-' + l + ']').forEach(function (el) {
      el.setAttribute('alt', el.getAttribute('data-title-' + l));
    });
  }
  setLang(root.getAttribute('data-lang') || 'en');
  document.addEventListener('click', function (e) {
    if (e.target.closest('.lang-toggle')) setLang(root.getAttribute('data-lang') === 'en' ? 'lt' : 'en');
  });

  // Home menu
  var menu = document.querySelector('.menu');
  if (menu) {
    menu.querySelector('.menu-open').addEventListener('click', function () { menu.classList.add('is-open'); menu.querySelector('.menu-list a').focus(); });
    menu.querySelector('.menu-close').addEventListener('click', function () { menu.classList.remove('is-open'); menu.querySelector('.menu-open').focus(); });
  }

  // Accordions — only one open per group, like the original
  document.querySelectorAll('.acc-btn').forEach(function (btn) {
    btn.addEventListener('click', function () {
      var open = btn.getAttribute('aria-expanded') === 'true';
      var group = btn.closest('[data-acc-group]');
      if (group && !open) group.querySelectorAll('.acc-btn[aria-expanded="true"]').forEach(function (o) {
        o.setAttribute('aria-expanded', 'false'); document.getElementById(o.getAttribute('aria-controls')).hidden = true;
      });
      btn.setAttribute('aria-expanded', String(!open));
      document.getElementById(btn.getAttribute('aria-controls')).hidden = open;
    });
  });
  // In-page "get in touch" links open the contact accordion
  document.querySelectorAll('a[href="#contact"]').forEach(function (a) {
    a.addEventListener('click', function () {
      var b = document.querySelector('#contact .acc-btn');
      if (b && b.getAttribute('aria-expanded') !== 'true') b.click();
    });
  });

  var reduce = window.matchMedia('(prefers-reduced-motion: reduce)').matches;

  // Slideshows
  document.querySelectorAll('.show').forEach(function (show) {
    var imgs = show.querySelectorAll('.show-stage img');
    var count = show.querySelector('.count');
    var playBtn = show.querySelector('.play');
    var i = 0, timer = null, n = imgs.length;
    function load(k) { var im = imgs[(k + n) % n]; if (im.dataset.src) { im.src = im.dataset.src; im.removeAttribute('data-src'); } }
    function go(k) {
      imgs[i].classList.remove('on');
      i = (k + n) % n; load(i); load(i + 1);
      imgs[i].classList.add('on');
      count.textContent = (i + 1) + ' / ' + n;
    }
    function setPlaying(p) {
      clearInterval(timer); timer = p ? setInterval(function () { go(i + 1); }, 5000) : null;
      playBtn.innerHTML = p ? ICON.pause : ICON.play;
      playBtn.setAttribute('aria-label', p ? 'Pause slideshow' : 'Play slideshow');
    }
    show.querySelector('.prev').addEventListener('click', function () { go(i - 1); });
    show.querySelector('.next').addEventListener('click', function () { go(i + 1); });
    playBtn.addEventListener('click', function () { setPlaying(!timer); });
    go(0);
    playBtn.innerHTML = ICON.play;
    // Autoplay only while visible, and never with reduced motion
    if (!reduce && 'IntersectionObserver' in window) {
      new IntersectionObserver(function (es) { es.forEach(function (e) { if (e.isIntersecting && !show.dataset.started) { show.dataset.started = 1; setPlaying(true); } }); }, { threshold: .4 }).observe(show);
    } else { setPlaying(false); }
  });

  // Lightbox for book photos
  var lb = document.querySelector('.lb');
  if (lb) {
    var thumbs = Array.prototype.slice.call(document.querySelectorAll('.thumbs button'));
    var big = lb.querySelector('img'), cnt = lb.querySelector('.count'), cur = 0, opener = null;
    function show(k) {
      cur = (k + thumbs.length) % thumbs.length;
      var t = thumbs[cur]; big.src = t.dataset.full; big.alt = t.querySelector('img').alt;
      cnt.textContent = (cur + 1) + ' / ' + thumbs.length;
    }
    function close() { lb.classList.remove('open'); document.body.style.overflow = ''; if (opener) opener.focus(); }
    thumbs.forEach(function (t, k) { t.addEventListener('click', function () { opener = t; show(k); lb.classList.add('open'); document.body.style.overflow = 'hidden'; lb.querySelector('.close').focus(); }); });
    lb.querySelector('.close').addEventListener('click', close);
    lb.querySelector('.prev').addEventListener('click', function () { show(cur - 1); });
    lb.querySelector('.next').addEventListener('click', function () { show(cur + 1); });
    lb.addEventListener('click', function (e) { if (e.target === lb) close(); });
    document.addEventListener('keydown', function (e) {
      if (!lb.classList.contains('open')) return;
      if (e.key === 'Escape') close();
      if (e.key === 'ArrowLeft') show(cur - 1);
      if (e.key === 'ArrowRight') show(cur + 1);
    });
  }

  // Lamp: a soft pool of light follows the cursor (pointer devices only)
  var lamp = document.querySelector('.bg-lamp');
  if (lamp && !reduce && window.matchMedia('(hover: hover)').matches) {
    var raf = 0;
    window.addEventListener('mousemove', function (e) {
      cancelAnimationFrame(raf);
      raf = requestAnimationFrame(function () {
        lamp.style.setProperty('--mx', e.clientX + 'px'); lamp.style.setProperty('--my', e.clientY + 'px'); lamp.style.opacity = 1;
      });
    });
  }
})();
