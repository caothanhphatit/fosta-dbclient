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

  // "Run with Docker" and "Download for macOS" open the command panel under the hero buttons; the macOS button also
  // shows the note for Mac users. A second click on the same button closes it. Without JavaScript the panel and the
  // note are always shown. "Download for Windows" is a plain link to the installer: the click is left alone, so the
  // download starts, and opens the download panel; only one panel is open at a time, and the download panel stays open
  // on a second click (it starts the download again). Without JavaScript the panel stays hidden.
  function initDocker() {
    var toggles = document.querySelectorAll("[data-docker-toggle], [data-download-toggle]");
    var panels = document.querySelectorAll("#docker, #download");
    var panel = document.getElementById("docker");
    if (!toggles.length || !panel) return;
    root.classList.add("js-docker");
    var current = null;
    function set(toggle) {
      current = toggle;
      var target = toggle && toggle.getAttribute("aria-controls");
      Array.prototype.forEach.call(panels, function (p) {
        p.classList.toggle("is-open", p.id === target);
      });
      panel.classList.toggle("is-mac", !!toggle && toggle.getAttribute("data-docker-toggle") === "mac");
      Array.prototype.forEach.call(toggles, function (t) {
        t.setAttribute("aria-expanded", t === toggle ? "true" : "false");
      });
    }
    Array.prototype.forEach.call(toggles, function (toggle) {
      var isDownload = toggle.hasAttribute("data-download-toggle");
      toggle.addEventListener("click", function (event) {
        if (isDownload) return set(toggle);
        event.preventDefault();
        set(current === toggle ? null : toggle);
      });
    });
    if (window.location.hash === "#docker") set(document.querySelector('[data-docker-toggle=""]') || toggles[0]);
  }

  // Copy buttons: copy the commands without their "$" prompts. Hidden without the clipboard API.
  function initCopy() {
    if (!navigator.clipboard || !navigator.clipboard.writeText) return;
    Array.prototype.forEach.call(document.querySelectorAll("[data-copy]"), function (button) {
      var source = document.getElementById(button.getAttribute("data-copy"));
      var label = button.querySelector("[data-copy-label]");
      if (!source || !label) return;
      var text = label.textContent;
      var timer = 0;
      button.hidden = false;
      button.addEventListener("click", function () {
        var lines = Array.prototype.map.call(source.querySelectorAll(".line"), function (line) {
          var copy = line.cloneNode(true);
          Array.prototype.forEach.call(copy.querySelectorAll(".prompt"), function (prompt) {
            prompt.remove();
          });
          return copy.textContent.trim();
        });
        navigator.clipboard.writeText(lines.join("\n")).then(function () {
          button.classList.add("is-copied");
          label.textContent = label.getAttribute("data-label-copied") || text;
          window.clearTimeout(timer);
          timer = window.setTimeout(function () {
            button.classList.remove("is-copied");
            label.textContent = text;
          }, 2000);
        });
      });
    });
  }

  function init() {
    initDocker();
    initCopy();
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
    initHeroNet();
    initMotion();

    // Screenshot slots: each <figure data-shot="..."> shows its HTML mock until its PNG is listed in
    // assets/screenshots/shots.json. Donation QR slots (<figure data-qr="...">, Support page) work the same way with
    // assets/donate/qr.json and show "QR code coming soon" until then.
    initSlots("shot", "shots.json", "shot-img", "has-shot");
    initSlots("qr", "qr.json", "qr-img", "has-qr");
  }

  // Optional images: each <figure data-NAME="path/file.png"> keeps its placeholder until the file name is listed in
  // the manifest (a JSON array of file names) next to the images. Listing them avoids 404s for missing files. The
  // manifest's URL follows the data attribute, which the build already made relative to the page (../assets/...).
  function initSlots(name, manifest, imgClass, readyClass) {
    var slots = document.querySelectorAll("figure[data-" + name + "]");
    if (!slots.length || !window.fetch) return;
    fetch(slots[0].getAttribute("data-" + name).replace(/[^/]*$/, "") + manifest, { cache: "no-cache" })
      .then(function (response) {
        return response.ok ? response.json() : [];
      })
      .then(function (available) {
        if (!Array.isArray(available)) return;
        Array.prototype.forEach.call(slots, function (figure) {
          var src = figure.getAttribute("data-" + name);
          if (available.indexOf(src.split("/").pop()) === -1) return;
          var img = new Image();
          img.className = imgClass;
          img.alt = figure.getAttribute("data-" + name + "-alt") || "";
          img.decoding = "async";
          img.onload = function () {
            if (!img.naturalWidth) return;
            figure.insertBefore(img, figure.firstChild);
            figure.classList.add(readyClass);
          };
          img.src = src;
        });
      })
      .catch(function () {
        /* keep the placeholders */
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

  // Hero carousel. Without JavaScript the views form a horizontally scrollable, snapping strip. With JavaScript
  // they are stacked and cross-fade every 5 s, with no buttons or dots; a horizontal swipe or the arrow keys
  // (when the carousel has focus) move between views. Auto-advance pauses on hover, on keyboard focus and in
  // hidden tabs, and is off for prefers-reduced-motion. "#slide=N" in the URL opens view N and holds it.
  function initCarousel() {
    var root = document.querySelector("[data-carousel]");
    if (!root) return;
    var viewport = root.querySelector("[data-carousel-viewport]");
    var slides = root.querySelectorAll("[data-carousel-slide]");
    if (!viewport || slides.length < 2) return;
    var status = root.querySelector("[data-carousel-status]");
    var reduced = !!(window.matchMedia && window.matchMedia("(prefers-reduced-motion: reduce)").matches);
    var index = 0;
    var held = false;
    var hovered = false;
    var focused = false;
    var timer = null;
    var touchX = null;
    var touchY = null;

    function show(i, announce) {
      index = (i + slides.length) % slides.length;
      Array.prototype.forEach.call(slides, function (slide, j) {
        var active = j === index;
        slide.classList.toggle("is-active", active);
        if (active) slide.removeAttribute("aria-hidden");
        else slide.setAttribute("aria-hidden", "true");
      });
      if (announce && status) {
        status.textContent = (status.getAttribute("data-template") || "{n} / {total}")
          .replace("{n}", String(index + 1))
          .replace("{total}", String(slides.length))
          .replace("{name}", slides[index].getAttribute("aria-label") || "");
      }
    }

    function running() {
      return !reduced && !held && !hovered && !focused && !document.hidden;
    }

    function schedule() {
      window.clearTimeout(timer);
      if (running()) {
        timer = window.setTimeout(function () {
          show(index + 1, false);
          schedule();
        }, 5000);
      }
    }

    viewport.addEventListener(
      "touchstart",
      function (event) {
        touchX = event.touches[0].clientX;
        touchY = event.touches[0].clientY;
      },
      { passive: true },
    );
    viewport.addEventListener(
      "touchend",
      function (event) {
        if (touchX === null) return;
        var dx = event.changedTouches[0].clientX - touchX;
        var dy = event.changedTouches[0].clientY - touchY;
        touchX = null;
        if (Math.abs(dx) > 40 && Math.abs(dx) > Math.abs(dy) * 1.5) {
          show(index + (dx < 0 ? 1 : -1), true);
          schedule();
        }
      },
      { passive: true },
    );

    viewport.tabIndex = 0;
    viewport.addEventListener("keydown", function (event) {
      if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
      event.preventDefault();
      show(index + (event.key === "ArrowRight" ? 1 : -1), true);
    });

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

    var start = /(?:^#|&)slide=(\d+)/.exec(window.location.hash || "");
    var first = start ? Math.min(Math.max(parseInt(start[1], 10) - 1, 0), slides.length - 1) : 0;
    held = !!start;
    if ("IntersectionObserver" in window) {
      new IntersectionObserver(function (entries) {
        root.classList.toggle("is-offscreen", !entries[0].isIntersecting);
      }).observe(root);
    }
    root.classList.add("is-enhanced");
    show(first, false);
    schedule();
  }

  // Hero background: a slowly drifting network of tables (nodes) with rows (packets) travelling along the
  // relations between them. Canvas only, about 2% CPU: it stops when the hero is off screen or the tab is hidden,
  // caps the pixel ratio at 2, and draws a single still frame for prefers-reduced-motion.
  function initHeroNet() {
    var canvas = document.querySelector("[data-hero-net]");
    if (!canvas || !canvas.getContext) return;
    var ctx = canvas.getContext("2d");
    var hero = canvas.parentElement;
    var reduced = !!(window.matchMedia && window.matchMedia("(prefers-reduced-motion: reduce)").matches);
    var nodes = [];
    var packets = [];
    var width = 0;
    var height = 0;
    var dpr = 1;
    var link = 150;
    var colors = { node: "#2f6fed", line: "#8a93a3", packet: "#2f6fed" };
    var visible = true;
    var frame = 0;
    var last = 0;
    var pointer = { x: -9999, y: -9999 };

    function readColors() {
      var style = window.getComputedStyle(document.documentElement);
      var accent = style.getPropertyValue("--accent").trim() || "#2f6fed";
      var muted = style.getPropertyValue("--text-2").trim() || "#8a93a3";
      colors = { node: accent, line: muted, packet: accent };
    }

    function resize() {
      var rect = canvas.getBoundingClientRect();
      dpr = Math.min(window.devicePixelRatio || 1, 2);
      width = Math.max(1, rect.width);
      height = Math.max(1, rect.height);
      canvas.width = Math.round(width * dpr);
      canvas.height = Math.round(height * dpr);
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      link = width < 600 ? 110 : 150;
      var want = Math.round(Math.min(width < 600 ? 22 : 54, (width * height) / 22000));
      while (nodes.length < want) nodes.push(makeNode());
      nodes.length = want;
      nodes.forEach(function (n) {
        n.x = Math.min(n.x, width);
        n.y = Math.min(n.y, height);
      });
      packets = [];
      if (reduced) draw(0);
    }

    function makeNode() {
      var table = Math.random() < 0.22;
      return {
        x: Math.random() * (width || 1200),
        y: Math.random() * (height || 800),
        vx: (Math.random() - 0.5) * 0.18,
        vy: (Math.random() - 0.5) * 0.18,
        r: table ? 3.2 : 1.2 + Math.random() * 1.2,
        table: table,
        pulse: Math.random() * Math.PI * 2,
      };
    }

    // Fade towards the bottom of the hero (replaces a CSS mask, which is costly on a canvas that changes every frame).
    function vfade(y) {
      var t = y / height;
      return t < 0.55 ? 1 : Math.max(0, 1 - (t - 0.55) / 0.45);
    }

    function rgba(color, alpha) {
      ctx.globalAlpha = alpha;
      return color;
    }

    function spawnPacket() {
      if (nodes.length < 2) return;
      var a = nodes[(Math.random() * nodes.length) | 0];
      var best = null;
      var bestD = link * link;
      nodes.forEach(function (b) {
        if (b === a) return;
        var dx = b.x - a.x;
        var dy = b.y - a.y;
        var d = dx * dx + dy * dy;
        if (d < bestD && Math.random() < 0.6) {
          best = b;
          bestD = d;
        }
      });
      if (best) packets.push({ a: a, b: best, t: 0, speed: 0.006 + Math.random() * 0.01 });
    }

    function draw(dt) {
      ctx.clearRect(0, 0, width, height);
      var i;
      var j;
      for (i = 0; i < nodes.length; i++) {
        var n = nodes[i];
        if (dt) {
          n.x += n.vx * dt;
          n.y += n.vy * dt;
          if (n.x < -20) n.x = width + 20;
          if (n.x > width + 20) n.x = -20;
          if (n.y < -20) n.y = height + 20;
          if (n.y > height + 20) n.y = -20;
          n.pulse += 0.002 * dt;
        }
      }
      // Edges are grouped into four opacity buckets and stroked once per bucket (one draw call each).
      ctx.lineWidth = 1;
      var buckets = [[], [], [], []];
      for (i = 0; i < nodes.length; i++) {
        for (j = i + 1; j < nodes.length; j++) {
          var dx = nodes[i].x - nodes[j].x;
          if (dx > link || dx < -link) continue;
          var dy = nodes[i].y - nodes[j].y;
          if (dy > link || dy < -link) continue;
          var d2 = dx * dx + dy * dy;
          if (d2 < link * link) {
            var strength = (1 - Math.sqrt(d2) / link) * vfade(nodes[i].y);
            if (strength > 0.05) buckets[Math.min(3, (strength * 4) | 0)].push(nodes[i], nodes[j]);
          }
        }
      }
      ctx.strokeStyle = colors.line;
      for (var b = 0; b < 4; b++) {
        var list = buckets[b];
        if (!list.length) continue;
        ctx.globalAlpha = ((b + 0.5) / 4) * 0.22;
        ctx.beginPath();
        for (var k = 0; k < list.length; k += 2) {
          ctx.moveTo(list[k].x, list[k].y);
          ctx.lineTo(list[k + 1].x, list[k + 1].y);
        }
        ctx.stroke();
      }
      if (pointer.x > -9000) {
        ctx.strokeStyle = colors.node;
        ctx.globalAlpha = 0.22;
        ctx.beginPath();
        for (i = 0; i < nodes.length; i++) {
          var px = nodes[i].x - pointer.x;
          var py = nodes[i].y - pointer.y;
          if (px * px + py * py < link * link * 1.4) {
            ctx.moveTo(nodes[i].x, nodes[i].y);
            ctx.lineTo(pointer.x, pointer.y);
          }
        }
        ctx.stroke();
      }
      for (i = 0; i < nodes.length; i++) {
        var node = nodes[i];
        if (node.table) {
          var glow = 0.35 + 0.25 * Math.sin(node.pulse);
          ctx.fillStyle = rgba(colors.node, glow * 0.25 * vfade(node.y));
          ctx.beginPath();
          ctx.arc(node.x, node.y, node.r * 3.2, 0, Math.PI * 2);
          ctx.fill();
          ctx.fillStyle = rgba(colors.node, 0.75 * vfade(node.y));
          ctx.fillRect(node.x - node.r, node.y - node.r, node.r * 2, node.r * 2);
        } else {
          ctx.fillStyle = rgba(colors.line, 0.45 * vfade(node.y));
          ctx.beginPath();
          ctx.arc(node.x, node.y, node.r, 0, Math.PI * 2);
          ctx.fill();
        }
      }
      if (dt) {
        if (packets.length < (width < 600 ? 6 : 14) && Math.random() < 0.08) spawnPacket();
        for (i = packets.length - 1; i >= 0; i--) {
          var p = packets[i];
          p.t += p.speed * dt * 0.06;
          if (p.t >= 1) {
            packets.splice(i, 1);
            continue;
          }
          var x = p.a.x + (p.b.x - p.a.x) * p.t;
          var y = p.a.y + (p.b.y - p.a.y) * p.t;
          var tx = p.a.x + (p.b.x - p.a.x) * Math.max(0, p.t - 0.12);
          var ty = p.a.y + (p.b.y - p.a.y) * Math.max(0, p.t - 0.12);
          var fade = Math.sin(p.t * Math.PI);
          ctx.globalAlpha = fade * 0.55 * vfade(y);
          ctx.strokeStyle = colors.packet;
          ctx.lineWidth = 1.6;
          ctx.beginPath();
          ctx.moveTo(tx, ty);
          ctx.lineTo(x, y);
          ctx.stroke();
          ctx.globalAlpha = fade * 0.9 * vfade(y);
          ctx.fillStyle = colors.packet;
          ctx.beginPath();
          ctx.arc(x, y, 1.9, 0, Math.PI * 2);
          ctx.fill();
        }
        ctx.lineWidth = 1;
      }
      ctx.globalAlpha = 1;
    }

    // 30 fps is plenty for a slow drift and halves the work.
    function loop(now) {
      frame = 0;
      if (!visible || document.hidden) return;
      if (!last || now - last >= (width < 600 ? 47 : 31)) {
        var dt = last ? Math.min(now - last, 66) : 33;
        last = now;
        draw(dt);
      }
      frame = window.requestAnimationFrame(loop);
    }

    function start() {
      if (reduced || frame || !visible || document.hidden) return;
      last = 0;
      frame = window.requestAnimationFrame(loop);
    }

    readColors();
    resize();
    window.addEventListener("resize", function () {
      resize();
    });
    if (window.MutationObserver) {
      new MutationObserver(function () {
        readColors();
        if (reduced) draw(0);
      }).observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
    }
    if (window.matchMedia) {
      var scheme = window.matchMedia("(prefers-color-scheme: dark)");
      var onScheme = function () {
        readColors();
        if (reduced) draw(0);
      };
      if (scheme.addEventListener) scheme.addEventListener("change", onScheme);
      else if (scheme.addListener) scheme.addListener(onScheme);
    }
    if (reduced) return;
    hero.addEventListener("pointermove", function (event) {
      var rect = canvas.getBoundingClientRect();
      pointer.x = event.clientX - rect.left;
      pointer.y = event.clientY - rect.top;
    });
    hero.addEventListener("pointerleave", function () {
      pointer.x = -9999;
      pointer.y = -9999;
    });
    document.addEventListener("visibilitychange", start);
    if ("IntersectionObserver" in window) {
      new IntersectionObserver(
        function (entries) {
          visible = entries[0].isIntersecting;
          start();
        },
        { rootMargin: "0px 0px -45% 0px" },
      ).observe(hero);
    }
    start();
  }

  // Motion layer: the app window tilts back to flat as the hero scrolls away, cards get a spotlight that follows
  // the pointer, and a thin bar at the top shows reading progress. Off for prefers-reduced-motion.
  function initMotion() {
    var reduced = !!(window.matchMedia && window.matchMedia("(prefers-reduced-motion: reduce)").matches);
    if (reduced) return;

    var bar = document.createElement("div");
    bar.className = "scroll-progress";
    bar.setAttribute("aria-hidden", "true");
    document.body.appendChild(bar);

    var visual = document.querySelector(".hero-visual");
    var ticking = false;
    if (visual) {
      visual.addEventListener("animationend", function (event) {
        if (event.target !== visual) return;
        visual.classList.add("is-tilting");
        update();
      });
    }

    function update() {
      ticking = false;
      var doc = document.documentElement;
      var max = Math.max(1, doc.scrollHeight - window.innerHeight);
      bar.style.transform = "scaleX(" + Math.min(1, Math.max(0, window.scrollY / max)).toFixed(4) + ")";
      if (visual && window.scrollY < window.innerHeight * 1.5) {
        var rect = visual.getBoundingClientRect();
        // 10deg while the window sits low in the viewport, flat once its top reaches the upper quarter.
        var t = Math.min(1, Math.max(0, (rect.top - window.innerHeight * 0.2) / (window.innerHeight * 0.6)));
        if (visual.classList.contains("is-tilting")) {
          visual.style.transform = "perspective(1600px) rotateX(" + (t * 10).toFixed(2) + "deg) scale(" + (1 - t * 0.04).toFixed(3) + ")";
        }
      }
    }

    // The entrance animation ends on the tilt the scroll position calls for, so nothing jumps when it hands over.
    if (visual) {
      var r0 = visual.getBoundingClientRect();
      var t0 = Math.min(1, Math.max(0, (r0.top - window.innerHeight * 0.2) / (window.innerHeight * 0.6)));
      visual.style.setProperty("--tilt-end", (t0 * 10).toFixed(2) + "deg");
      visual.style.setProperty("--tilt-end-scale", (1 - t0 * 0.04).toFixed(3));
    }

    window.addEventListener(
      "scroll",
      function () {
        if (!ticking) {
          ticking = true;
          window.requestAnimationFrame(update);
        }
      },
      { passive: true },
    );
    window.addEventListener("resize", update);
    update();

    var cards = document.querySelectorAll(".why-card, .mini-card, .engine, .privacy-grid > li");
    Array.prototype.forEach.call(cards, function (card) {
      card.addEventListener("pointermove", function (event) {
        var rect = card.getBoundingClientRect();
        card.style.setProperty("--mx", event.clientX - rect.left + "px");
        card.style.setProperty("--my", event.clientY - rect.top + "px");
      });
    });
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
