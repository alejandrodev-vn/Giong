// giong.run: three small behaviours. The page reads fine without any of them.
(function () {
  "use strict";

  // 1. Copy buttons.
  document.querySelectorAll("[data-copy]").forEach(function (btn) {
    var label = btn.textContent;
    btn.addEventListener("click", function () {
      var el = document.getElementById(btn.getAttribute("data-copy"));
      if (!el || !navigator.clipboard) return;
      navigator.clipboard.writeText(el.textContent.trim()).then(function () {
        btn.textContent = btn.getAttribute("data-done") || label;
        btn.classList.add("done");
        setTimeout(function () {
          btn.textContent = label;
          btn.classList.remove("done");
        }, 1600);
      }).catch(function () {});
    });
  });

  // 2. Code tabs. Without JavaScript every example is shown, one under another.
  var tabs = Array.prototype.slice.call(document.querySelectorAll('[role="tab"]'));
  function select(tab, focus) {
    tabs.forEach(function (t) {
      var on = t === tab;
      t.setAttribute("aria-selected", on ? "true" : "false");
      t.tabIndex = on ? 0 : -1;
      var panel = document.getElementById(t.getAttribute("aria-controls"));
      if (panel) panel.hidden = !on;
    });
    if (focus) tab.focus();
  }
  tabs.forEach(function (tab, i) {
    tab.addEventListener("click", function () { select(tab, false); });
    tab.addEventListener("keydown", function (e) {
      var step = { ArrowRight: 1, ArrowDown: 1, ArrowLeft: -1, ArrowUp: -1 }[e.key];
      if (!step) return;
      e.preventDefault();
      select(tabs[(i + step + tabs.length) % tabs.length], true);
    });
  });
  if (tabs.length) select(tabs[0], false);

  // 3. The tick. Operations queue on the ring; one io_uring_enter submits them
  //    all and reaps their completions. The second row counts what the same
  //    work costs when every operation is its own system call.
  var tick = document.getElementById("tick");
  var still = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  if (!tick || still) return;

  var segs = Array.prototype.slice.call(tick.querySelectorAll(".seg"));
  var epoll = Array.prototype.slice.call(tick.querySelectorAll("[data-marks-epoll] i"));
  var giong = tick.querySelector("[data-marks-giong] i");
  var ops = tick.querySelector("[data-ops]");
  var sys = tick.querySelector("[data-sys]");
  var unit = tick.querySelector("[data-unit]");
  var nGiong = tick.querySelector("[data-n-giong]");
  var nEpoll = tick.querySelector("[data-n-epoll]");
  var N = segs.length;
  var head = 14;          // the page ships with segments 0..13 lit
  var timer = null;

  function show(queued, calls) {
    ops.textContent = queued;
    sys.textContent = calls;
    unit.textContent = tick.getAttribute(calls === 1 ? "data-unit1" : "data-unitn");
    nGiong.textContent = calls;
    nEpoll.textContent = queued;
    giong.classList.toggle("on", calls > 0);
    epoll.forEach(function (m, i) { m.classList.toggle("on", i < queued); });
  }
  function later(fn, ms) { timer = setTimeout(fn, ms); }

  function fill(start, want, done) {
    // Tasks run and queue their operations: nothing has entered the kernel.
    var k = 0;
    (function next() {
      if (k === want) return later(function () { enter(start, want); }, 420);
      var seg = segs[(start + k) % N];
      seg.className.baseVal = "seg on";
      k += 1;
      show(k, 0);
      later(next, 70 + Math.random() * 130);
    })();
  }
  function enter(start, count) {
    // One system call: everything is submitted, every completion comes back.
    tick.classList.add("enter");
    show(count, 1);
    for (var k = 0; k < count; k++) segs[(start + k) % N].className.baseVal = "seg done";
    later(function () { drain(start, count); }, 900);
  }
  function drain(start, count) {
    var k = 0;
    (function next() {
      if (k === count) {
        tick.classList.remove("enter");
        head = (start + count) % N;
        return later(run, 500);
      }
      segs[(start + k) % N].className.baseVal = "seg";
      k += 1;
      later(next, 28);
    })();
  }
  function run() {
    show(0, 0);
    fill(head, 8 + Math.floor(Math.random() * 13));   // 8 to 20 operations a tick
  }

  // Start from the state the page ships with: hold it, then flush it.
  var io = "IntersectionObserver" in window ? new IntersectionObserver(function (entries) {
    var seen = entries[0].isIntersecting;
    if (seen && timer === null) later(function () { enter(0, 14); }, 1800);
    if (!seen && timer !== null && !tick.classList.contains("enter")) {
      // Off screen: stop spending CPU. An idle runtime should cost nothing.
      clearTimeout(timer);
      timer = null;
      segs.forEach(function (s, i) { s.className.baseVal = i < 14 ? "seg on" + (i < 3 ? " a" + (i + 1) : "") : "seg"; });
      head = 14;
      show(14, 1);
    }
  }, { threshold: 0.2 }) : null;
  if (io) io.observe(tick); else later(function () { enter(0, 14); }, 1800);
})();
