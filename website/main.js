// DB Studio website: progressive enhancement only. The page works without this file.
(function () {
  "use strict";

  var root = document.documentElement;
  var KEY = "dbstudio-site-theme";
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
    if (button) button.setAttribute("aria-label", isDark() ? "Switch to light theme" : "Switch to dark theme");
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

    // Screenshot slots: each <figure data-shot="..."> shows its HTML mock until its PNG is listed in
    // assets/screenshots/shots.json (a JSON array of file names). Listing them avoids 404s for missing files.
    var shots = document.querySelectorAll("figure[data-shot]");
    if (!shots.length || !window.fetch) return;
    fetch("assets/screenshots/shots.json", { cache: "no-cache" })
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

  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", init);
  else init();
})();
