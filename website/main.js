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

    function go(i, announce) {
      i = (i + slides.length) % slides.length;
      setActive(i, announce);
      programmatic = true;
      target = i;
      viewport.scrollTo({ left: i * viewport.clientWidth, behavior: reduced ? "auto" : "smooth" });
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
      viewport.scrollTo({ left: index * viewport.clientWidth, behavior: "auto" });
    });

    root.classList.add("is-enhanced");
    controls.hidden = false;
    setActive(0, false);
    schedule();
  }

  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", init);
  else init();
})();
