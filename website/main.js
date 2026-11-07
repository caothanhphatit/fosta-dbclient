// Fosta Studio website: progressive enhancement only. The page works without this file.
(function () {
  "use strict";

  var root = document.documentElement;
  var KEY = "fosta-studio-site-theme";
  var media = window.matchMedia ? window.matchMedia("(prefers-color-scheme: dark)") : null;

  function stored() {
    try {
      var value = window.localStorage.getItem(KEY);
      return value === "light" || value === "dark" ? value : null;
    } catch (e) {
      return null;
    }
  }

  function isDark() {
    var theme = root.getAttribute("data-theme");
    if (theme) return theme === "dark";
    return !!(media && media.matches);
  }

  function sync() {
    root.classList.toggle("is-dark", isDark());
    var button = document.querySelector("[data-theme-toggle]");
    if (!button) return;
    // The labels are translated at build time and carried on the button.
    var label = button.getAttribute(isDark() ? "data-label-light" : "data-label-dark");
    if (label) button.setAttribute("aria-label", label);
  }

  // Apply a saved choice before first paint (this script is loaded in <head> without defer).
  var saved = stored();
  if (saved) root.setAttribute("data-theme", saved);
  sync();
  if (media && media.addEventListener) media.addEventListener("change", sync);
  else if (media && media.addListener) media.addListener(sync); // Safari before 14

  function init() {
    var button = document.querySelector("[data-theme-toggle]");
    if (button) {
      button.hidden = false;
      sync();
      button.addEventListener("click", function () {
        var next = isDark() ? "light" : "dark";
        root.setAttribute("data-theme", next);
        try {
          window.localStorage.setItem(KEY, next);
        } catch (e) {
          /* storage unavailable: the choice lasts for this page view */
        }
        sync();
      });
    }

    initLanguages();
    initCarousel();
    initEffects();

    // Screenshot slots: each <figure data-shot="..."> shows its HTML mock until its PNG is listed in
    // assets/screenshots/shots.json (a JSON array of file names). Listing them avoids 404s for missing files.
    // The JSON sits next to the PNGs, so its URL follows data-shot (which is ../assets/... on translated pages).
    var shots = document.querySelectorAll("figure[data-shot]");
    if (!shots.length || !window.fetch) return;
    fetch(shots[0].getAttribute("data-shot").replace(/[^/]*$/, "") + "shots.json", { cache: "no-cache" })
      .then(function (response) {
        return response.ok ? response.json() : [];
      })
      .then(function (available) {
        if (!Array.isArray(available)) return;
        Array.prototype.forEach.call(shots, function (figure) {
          var src = figure.getAttribute("data-shot");
          var name = src.split("/").pop();
          if (available.indexOf(name) === -1) return;
          var img = new Image();
          img.className = "shot-img";
          img.alt = figure.getAttribute("data-shot-alt") || "";
          img.decoding = "async";
          img.onload = function () {
            if (!img.naturalWidth) return;
            figure.insertBefore(img, figure.firstChild);
            figure.classList.add("has-shot");
          };
          img.src = src;
        });
      })
      .catch(function () {
        /* keep the mocks */
      });
  }

  // Languages: remember an explicit choice, close the switcher menu on Escape or an outside click, and on the
  // English page offer (never force) the visitor's preferred language.
  var LANG_KEY = "fosta-studio-site-lang";
  var OFFERED = ["vi", "zh", "ja"];

  function storedLang() {
    try {
      return window.localStorage.getItem(LANG_KEY);
    } catch (e) {
      return null;
    }
  }

  function storeLang(code) {
    try {
      window.localStorage.setItem(LANG_KEY, code);
    } catch (e) {
      /* storage unavailable: nothing is remembered */
    }
  }

  // First supported language in the browser's preference list; English (or nothing) ends the search.
  function preferredLang() {
    var list = navigator.languages && navigator.languages.length ? navigator.languages : [navigator.language || ""];
    for (var i = 0; i < list.length; i++) {
      var tag = String(list[i]).toLowerCase();
      var base = tag.split("-")[0];
      if (base === "en") return null;
      // The Chinese page is Simplified; do not offer it to Traditional Chinese readers.
      if (base === "zh" && /-(hant|tw|hk|mo)(-|$)/.test(tag)) continue;
      if (OFFERED.indexOf(base) !== -1) return base;
    }
    return null;
  }

  function initLanguages() {
    Array.prototype.forEach.call(document.querySelectorAll("[data-lang-choice]"), function (link) {
      link.addEventListener("click", function () {
        storeLang(link.getAttribute("data-lang-choice"));
      });
    });

    var menu = document.querySelector("[data-lang-switch]");
    if (menu) {
      document.addEventListener("click", function (event) {
        if (menu.open && !menu.contains(event.target)) menu.open = false;
      });
      document.addEventListener("keydown", function (event) {
        if (event.key === "Escape" && menu.open) {
          menu.open = false;
          menu.querySelector("summary").focus();
        }
      });
    }

    var banner = document.querySelector("[data-lang-offer]");
    if (!banner) return;
    var choice = storedLang();
    var target = choice ? (OFFERED.indexOf(choice) !== -1 ? choice : null) : preferredLang();
    var offer = target && banner.querySelector('[data-offer="' + target + '"]');
    if (!offer) return;
    var close = banner.querySelector("[data-lang-offer-close]");
    offer.hidden = false;
    if (close) {
      close.setAttribute("aria-label", offer.getAttribute("data-dismiss") || "Close");
      close.setAttribute("lang", offer.getAttribute("lang"));
      close.addEventListener("click", function () {
        storeLang("en");
        banner.hidden = true;
      });
    }
    banner.hidden = false;
  }

  // Hero carousel. The markup is a scroll-snapping strip that works on its own (swipe or scroll); this adds arrows,
  // dots, a pause button and auto-advance every 6 s. Auto-advance pauses on hover, on keyboard focus inside the
  // carousel and in hidden tabs, and is off for prefers-reduced-motion (which also switches slides instantly).
  // Slide changes are announced (politely) only when the visitor caused them.
  function initCarousel() {
    var root = document.querySelector("[data-carousel]");
    if (!root) return;
    var viewport = root.querySelector("[data-carousel-viewport]");
    var slides = root.querySelectorAll("[data-carousel-slide]");
    var controls = root.querySelector("[data-carousel-controls]");
    if (!viewport || !controls || slides.length < 2) return;
    var dots = root.querySelectorAll("[data-carousel-dot]");
    var toggle = root.querySelector("[data-carousel-toggle]");
    var status = root.querySelector("[data-carousel-status]");
    var reduced = !!(window.matchMedia && window.matchMedia("(prefers-reduced-motion: reduce)").matches);
    var index = 0;
    var userPaused = false;
    var hovered = false;
    var focused = false;
    var timer = null;
    var settle = null;
    var programmatic = false;
    var target = 0;

    function setActive(i, announce) {
      index = i;
      Array.prototype.forEach.call(slides, function (slide, j) {
        if (j === i) slide.removeAttribute("aria-hidden");
        else slide.setAttribute("aria-hidden", "true");
      });
      Array.prototype.forEach.call(dots, function (dot, j) {
        if (j === i) dot.setAttribute("aria-current", "true");
        else dot.removeAttribute("aria-current");
      });
      if (announce && status) {
        status.textContent = (status.getAttribute("data-template") || "{n} / {total}")
          .replace("{n}", String(i + 1))
          .replace("{total}", String(slides.length))
          .replace("{name}", slides[i].getAttribute("aria-label") || "");
      }
    }

    // Element.scrollTo with options is missing in older Safari; fall back to setting scrollLeft (no animation).
    function scrollToSlide(i, smooth) {
      var left = i * viewport.clientWidth;
      if (typeof viewport.scrollTo === "function") {
        try {
          viewport.scrollTo({ left: left, behavior: smooth && !reduced ? "smooth" : "auto" });
          return;
        } catch (e) {
          /* fall through */
        }
      }
      viewport.scrollLeft = left;
    }

    function go(i, announce) {
      i = (i + slides.length) % slides.length;
      setActive(i, announce);
      programmatic = true;
      target = i;
      scrollToSlide(i, true);
    }

    function running() {
      return !reduced && !userPaused && !hovered && !focused && !document.hidden;
    }

    function schedule() {
      window.clearTimeout(timer);
      if (running()) {
        timer = window.setTimeout(function () {
          go(index + 1, false);
          schedule();
        }, 6000);
      }
    }

    // Swipes and scrolls by the visitor: pick up the slide the strip settled on.
    viewport.addEventListener(
      "scroll",
      function () {
        window.clearTimeout(settle);
        settle = window.setTimeout(function () {
          var i = Math.round(viewport.scrollLeft / Math.max(1, viewport.clientWidth));
          if (programmatic && i !== target) return; // our own smooth scroll is still on its way
          if (i !== index && i >= 0 && i < slides.length) {
            setActive(i, !programmatic);
            schedule();
          }
          programmatic = false;
        }, 120);
      },
      { passive: true },
    );

    // A swipe, wheel or drag by the visitor takes over from a scroll we started.
    ["pointerdown", "wheel", "touchstart"].forEach(function (type) {
      viewport.addEventListener(
        type,
        function () {
          programmatic = false;
        },
        { passive: true },
      );
    });

    root.querySelector("[data-carousel-prev]").addEventListener("click", function () {
      go(index - 1, true);
      schedule();
    });
    root.querySelector("[data-carousel-next]").addEventListener("click", function () {
      go(index + 1, true);
      schedule();
    });
    Array.prototype.forEach.call(dots, function (dot, j) {
      dot.addEventListener("click", function () {
        go(j, true);
        schedule();
      });
    });
    controls.addEventListener("keydown", function (event) {
      if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
      event.preventDefault();
      go(index + (event.key === "ArrowRight" ? 1 : -1), true);
    });

    if (toggle) {
      if (reduced) {
        toggle.hidden = true;
      } else {
        toggle.addEventListener("click", function () {
          userPaused = !userPaused;
          toggle.classList.toggle("is-paused", userPaused);
          var label = toggle.getAttribute(userPaused ? "data-label-play" : "data-label-pause");
          if (label) toggle.setAttribute("aria-label", label);
          schedule();
        });
      }
    }

    root.addEventListener("mouseenter", function () {
      hovered = true;
      schedule();
    });
    root.addEventListener("mouseleave", function () {
      hovered = false;
      schedule();
    });
    root.addEventListener("focusin", function () {
      focused = true;
      schedule();
    });
    root.addEventListener("focusout", function (event) {
      if (event.relatedTarget && root.contains(event.relatedTarget)) return;
      focused = false;
      schedule();
    });
    document.addEventListener("visibilitychange", schedule);
    window.addEventListener("resize", function () {
      scrollToSlide(index, false);
    });

    root.classList.add("is-enhanced");
    controls.hidden = false;

    // "#slide=2" in the URL opens that view (handy for links and screenshots); it pauses auto-advance.
    var start = /(?:^#|&)slide=(\d+)/.exec(window.location.hash || "");
    var first = start ? Math.min(Math.max(parseInt(start[1], 10) - 1, 0), slides.length - 1) : 0;
    setActive(first, false);
    if (first) {
      userPaused = true;
      if (toggle) {
        toggle.classList.add("is-paused");
        var playLabel = toggle.getAttribute("data-label-play");
        if (playLabel) toggle.setAttribute("aria-label", playLabel);
      }
      scrollToSlide(first, false);
    }
    schedule();
  }

  // Effects: scroll reveal, number count-up and the query demo. All of them start from the finished page and only
  // run when IntersectionObserver exists and the visitor has not asked for reduced motion.
  function initEffects() {
    var reduced = !!(window.matchMedia && window.matchMedia("(prefers-reduced-motion: reduce)").matches);
    if (reduced || !("IntersectionObserver" in window)) return;

    function onVisible(elements, callback, options) {
      var observer = new IntersectionObserver(
        function (entries) {
          entries.forEach(function (entry) {
            if (!entry.isIntersecting) return;
            observer.unobserve(entry.target);
            callback(entry.target);
          });
        },
        options || { rootMargin: "0px 0px -8% 0px" },
      );
      Array.prototype.forEach.call(elements, function (el) {
        observer.observe(el);
      });
      return observer;
    }

    // Scroll reveal: section headings, cards and feature rows fade up, siblings slightly staggered.
    var revealable = document.querySelectorAll(".section-head, .stats, .why-card, .feature, .mini-card, .engine, .privacy-grid > li, .roadmap > li, .oss > *");
    Array.prototype.forEach.call(revealable, function (el) {
      // Anything already on screen at load stays as it is.
      if (el.getBoundingClientRect().top < window.innerHeight) return;
      var siblings = el.parentNode ? Array.prototype.indexOf.call(el.parentNode.children, el) : 0;
      el.style.setProperty("--reveal-delay", Math.min(siblings, 5) * 0.06 + "s");
      el.classList.add("reveal");
    });
    onVisible(document.querySelectorAll(".reveal"), function (el) {
      el.classList.add("is-in");
    });
    // Printing or a very tall window: show everything.
    window.addEventListener("beforeprint", function () {
      Array.prototype.forEach.call(document.querySelectorAll(".reveal"), function (el) {
        el.classList.add("is-in");
      });
    });

    // Count-up numbers.
    onVisible(document.querySelectorAll("[data-count-to]"), function (el) {
      var to = parseInt(el.getAttribute("data-count-to"), 10);
      if (!(to > 0)) return;
      var start = null;
      var duration = 900;
      var finished = false;
      function frame(now) {
        if (finished) return;
        if (start === null) start = now;
        var t = Math.min(1, (now - start) / duration);
        var eased = 1 - Math.pow(1 - t, 3);
        el.textContent = String(Math.round(to * eased));
        if (t < 1) window.requestAnimationFrame(frame);
      }
      el.textContent = "0";
      window.requestAnimationFrame(frame);
      // If frames are throttled or frozen (background tab), still end on the real number.
      window.setTimeout(function () {
        finished = true;
        el.textContent = String(to);
      }, duration + 200);
    });

    initQueryDemo();
  }

  // Query demo: types the query, presses Run, then the rows arrive one by one. Replays while it stays on screen.
  function initQueryDemo() {
    var demo = document.querySelector("[data-query-demo]");
    if (!demo) return;
    var code = demo.querySelector("[data-qd-code]");
    var run = demo.querySelector("[data-qd-run]");
    var status = demo.querySelector("[data-qd-status]");
    var rows = demo.querySelectorAll("[data-qd-rows] tr");
    if (!code || !run || !status) return;

    // Text nodes of the highlighted query with their full text, so typing keeps the colours.
    var nodes = [];
    (function collect(node) {
      for (var child = node.firstChild; child; child = child.nextSibling) {
        if (child.nodeType === 3) nodes.push({ node: child, text: child.nodeValue });
        else collect(child);
      }
    })(code);
    var total = nodes.reduce(function (sum, n) {
      return sum + n.text.length;
    }, 0);
    var done = status.getAttribute("data-done") || status.textContent;
    var timers = [];
    var visible = false;
    var playing = false;

    function later(fn, ms) {
      timers.push(window.setTimeout(fn, ms));
    }

    function show(count) {
      var left = count;
      nodes.forEach(function (n) {
        var take = Math.max(0, Math.min(n.text.length, left));
        n.node.nodeValue = n.text.slice(0, take);
        left -= take;
      });
    }

    function play() {
      if (playing) return;
      playing = true;
      Array.prototype.forEach.call(rows, function (row) {
        row.classList.add("is-pending");
        row.classList.remove("is-new");
      });
      status.textContent = "";
      status.classList.remove("is-running");
      code.classList.add("is-typing");
      var typed = 0;
      (function type() {
        typed += 2 + Math.floor(Math.random() * 3);
        show(Math.min(typed, total));
        if (typed < total) return later(type, 28 + Math.random() * 40);
        code.classList.remove("is-typing");
        later(function () {
          run.classList.add("is-pressed");
          status.textContent = "Running…";
          status.classList.add("is-running");
          later(function () {
            run.classList.remove("is-pressed");
          }, 180);
          later(function () {
            status.classList.remove("is-running");
            status.textContent = done;
            Array.prototype.forEach.call(rows, function (row, i) {
              later(function () {
                row.classList.remove("is-pending");
                row.classList.add("is-new");
              }, i * 110);
            });
            later(
              function () {
                playing = false;
                if (visible)
                  later(function () {
                    if (visible) play();
                  }, 5200);
              },
              rows.length * 110 + 400,
            );
          }, 700);
        }, 450);
      })();
    }

    // Start when it comes into view; replay only while it stays visible.
    new IntersectionObserver(
      function (entries) {
        visible = entries[0].isIntersecting;
        if (visible && !playing) play();
      },
      { threshold: 0.4 },
    ).observe(demo);
  }

  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", init);
  else init();
})();
