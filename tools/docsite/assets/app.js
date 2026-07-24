// docsite — routing, filtering, and the command palette.
//
// the page ships every module as real markup, so this script never renders
// documentation. it reads the search index out of the dom, shows one module
// at a time, and gets out of the way.
(function () {
  "use strict";

  var root = document.documentElement;

  // --- theme ---------------------------------------------------------
  // the saved theme is applied by the inline script in <head>; this only
  // handles the toggle.

  var THEME_KEY = "pith-docs-theme";

  function currentTheme() {
    var explicit = root.getAttribute("data-theme");
    if (explicit) return explicit;
    var query = window.matchMedia && window.matchMedia("(prefers-color-scheme: dark)");
    return query && query.matches ? "dark" : "light";
  }

  var themeButton = document.querySelector(".theme-toggle");
  function paintThemeButton() {
    if (themeButton) themeButton.textContent = currentTheme() === "dark" ? "light" : "dark";
  }
  paintThemeButton();

  if (themeButton) {
    themeButton.addEventListener("click", function () {
      var next = currentTheme() === "dark" ? "light" : "dark";
      root.setAttribute("data-theme", next);
      try { localStorage.setItem(THEME_KEY, next); } catch (e) { /* ignore */ }
      paintThemeButton();
    });
  }

  // --- routing -------------------------------------------------------

  var sections = Array.prototype.slice.call(document.querySelectorAll("[data-module]"));
  var home = document.getElementById("home");
  var navLinks = Array.prototype.slice.call(document.querySelectorAll(".nav a"));
  var navByModule = {};
  navLinks.forEach(function (link) { navByModule[link.dataset.module] = link; });

  var byModule = {};
  sections.forEach(function (section) { byModule[section.dataset.module] = section; });

  // a hash is either "#std.net.http" or "#std.net.http.read_request"; the
  // longest matching module name wins so item anchors resolve to their module.
  function moduleForHash(hash) {
    var target = hash.replace(/^#/, "");
    if (!target) return null;
    if (byModule[target]) return target;
    var best = null;
    for (var name in byModule) {
      if (target.indexOf(name + ".") === 0 && (!best || name.length > best.length)) best = name;
    }
    return best;
  }

  function show(moduleName) {
    sections.forEach(function (section) {
      section.classList.toggle("is-active", section.dataset.module === moduleName);
    });
    if (home) home.classList.toggle("is-active", moduleName === null);
    navLinks.forEach(function (link) {
      link.classList.toggle("is-active", link.dataset.module === moduleName);
    });
    var active = moduleName ? navByModule[moduleName] : null;
    if (active && active.scrollIntoView) active.scrollIntoView({ block: "nearest" });
    document.title = moduleName ? moduleName + " — pith stdlib" : "pith stdlib";
  }

  function route() {
    var moduleName = moduleForHash(location.hash);
    show(moduleName);
    if (moduleName) {
      var anchor = location.hash ? document.getElementById(location.hash.slice(1)) : null;
      if (anchor && anchor.scrollIntoView) anchor.scrollIntoView({ block: "center" });
      else window.scrollTo(0, 0);
    } else {
      window.scrollTo(0, 0);
    }
  }

  window.addEventListener("hashchange", route);
  route();

  // --- search index --------------------------------------------------

  var index = Array.prototype.slice.call(document.querySelectorAll("[data-search]")).map(function (node) {
    return {
      id: node.id,
      module: node.dataset.module || node.closest("[data-module]").dataset.module,
      name: node.dataset.name,
      kind: node.dataset.kind,
      haystack: node.dataset.search.toLowerCase()
    };
  });

  // subsequence match: "rdreq" finds "read_request". returns the matched
  // positions so the result row can highlight them, or null for no match.
  function subsequence(haystack, needle) {
    var positions = [];
    var at = 0;
    for (var i = 0; i < needle.length; i++) {
      var found = haystack.indexOf(needle[i], at);
      if (found === -1) return null;
      positions.push(found);
      at = found + 1;
    }
    return positions;
  }

  function score(entry, needle) {
    var name = entry.name.toLowerCase();
    if (name === needle) return 0;
    if (name.indexOf(needle) === 0) return 1;
    if (name.indexOf(needle) !== -1) return 2;
    var qualified = (entry.module + "." + entry.name).toLowerCase();
    if (qualified.indexOf(needle) !== -1) return 3;
    if (subsequence(name, needle)) return 4;
    if (entry.haystack.indexOf(needle) !== -1) return 5;
    return -1;
  }

  function search(query) {
    var needle = query.trim().toLowerCase();
    if (!needle) return [];
    var hits = [];
    for (var i = 0; i < index.length; i++) {
      var rank = score(index[i], needle);
      if (rank >= 0) hits.push({ entry: index[i], rank: rank });
    }
    hits.sort(function (a, b) {
      if (a.rank !== b.rank) return a.rank - b.rank;
      if (a.entry.name.length !== b.entry.name.length) return a.entry.name.length - b.entry.name.length;
      return a.entry.name < b.entry.name ? -1 : 1;
    });
    return hits.slice(0, 40).map(function (hit) { return hit.entry; });
  }

  // --- command palette -----------------------------------------------

  var backdrop = document.querySelector(".palette-backdrop");
  var input = document.querySelector(".palette input");
  var results = document.querySelector(".palette-results");
  var empty = document.querySelector(".palette-empty");
  var cursor = 0;
  var shown = [];

  function escapeHtml(text) {
    return text.replace(/[&<>]/g, function (c) {
      return c === "&" ? "&amp;" : c === "<" ? "&lt;" : "&gt;";
    });
  }

  function markMatch(name, needle) {
    var lower = name.toLowerCase();
    var at = lower.indexOf(needle);
    if (at === -1 || !needle) return escapeHtml(name);
    return escapeHtml(name.slice(0, at)) +
      "<mark>" + escapeHtml(name.slice(at, at + needle.length)) + "</mark>" +
      escapeHtml(name.slice(at + needle.length));
  }

  function paint(query) {
    shown = search(query);
    var needle = query.trim().toLowerCase();
    results.innerHTML = shown.map(function (entry, i) {
      return '<li role="option" data-i="' + i + '" aria-selected="' + (i === cursor) + '">' +
        '<span class="badge k-' + entry.kind + '">' + entry.kind + "</span>" +
        '<span class="r-name">' + markMatch(entry.name, needle) + "</span>" +
        '<span class="r-mod">' + escapeHtml(entry.module) + "</span></li>";
    }).join("");
    empty.hidden = shown.length > 0 || !query.trim();
    if (!query.trim()) empty.hidden = true;
    empty.textContent = "no matches for “" + query.trim() + "”";
  }

  function moveCursor(delta) {
    if (!shown.length) return;
    cursor = (cursor + delta + shown.length) % shown.length;
    Array.prototype.forEach.call(results.children, function (li, i) {
      li.setAttribute("aria-selected", i === cursor);
      if (i === cursor && li.scrollIntoView) li.scrollIntoView({ block: "nearest" });
    });
  }

  function openPalette() {
    backdrop.hidden = false;
    input.value = "";
    cursor = 0;
    paint("");
    input.focus();
  }

  function closePalette() { backdrop.hidden = true; }

  function choose(i) {
    var entry = shown[i];
    if (!entry) return;
    closePalette();
    location.hash = "#" + entry.id;
    route();
  }

  document.querySelector(".search-trigger").addEventListener("click", openPalette);

  input.addEventListener("input", function () { cursor = 0; paint(input.value); });

  input.addEventListener("keydown", function (event) {
    if (event.key === "ArrowDown") { event.preventDefault(); moveCursor(1); }
    else if (event.key === "ArrowUp") { event.preventDefault(); moveCursor(-1); }
    else if (event.key === "Enter") { event.preventDefault(); choose(cursor); }
    else if (event.key === "Escape") { closePalette(); }
  });

  results.addEventListener("click", function (event) {
    var row = event.target.closest("li[data-i]");
    if (row) choose(Number(row.dataset.i));
  });

  backdrop.addEventListener("mousedown", function (event) {
    if (event.target === backdrop) closePalette();
  });

  document.addEventListener("keydown", function (event) {
    var typing = /^(INPUT|TEXTAREA)$/.test(event.target.tagName);
    if ((event.key === "k" || event.key === "K") && (event.metaKey || event.ctrlKey)) {
      event.preventDefault();
      openPalette();
    } else if (event.key === "/" && !typing && backdrop.hidden) {
      event.preventDefault();
      openPalette();
    } else if (event.key === "Escape" && !backdrop.hidden) {
      closePalette();
    }
  });

  // --- sidebar filter -------------------------------------------------

  var filter = document.querySelector(".nav-filter");
  if (filter) {
    filter.addEventListener("input", function () {
      var needle = filter.value.trim().toLowerCase();
      var anyVisible = false;
      document.querySelectorAll(".nav-group").forEach(function (group) {
        var groupVisible = false;
        group.querySelectorAll("li").forEach(function (li) {
          // match the qualified name, so "crypto" keeps every std.crypto.*
          // module even though the label only shows the leaf.
          var link = li.querySelector("a");
          var name = (link && link.dataset.module ? link.dataset.module : li.textContent).toLowerCase();
          var hit = !needle || name.indexOf(needle) !== -1;
          li.hidden = !hit;
          if (hit) groupVisible = true;
        });
        group.hidden = !groupVisible;
        if (groupVisible) anyVisible = true;
      });
      var note = document.querySelector(".nav .empty-note");
      if (note) note.hidden = anyVisible;
    });
  }
})();
