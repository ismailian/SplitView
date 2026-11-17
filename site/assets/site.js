// SplitView site: the live demo, reveal-on-scroll, and copy-the-hash.
// No dependencies; the CSP allows only this file.
document.documentElement.classList.add("js");

// ---------------------------------------------------------------------------
// The demo: a small model of SplitView's layout tree and prefix keymap.
// Like the app, it only takes keys while it has focus, and only after the prefix.
(() => {
  const stage = document.getElementById("stage");
  const status = document.getElementById("demo-status");
  const badge = document.getElementById("prefix-badge");
  const count = document.getElementById("pane-count");
  const prefixBtn = document.getElementById("prefix");
  if (!stage) return;

  const dirs = ["~/project", "~/project/src", "~/project/web", "~/project/api", "~/notes", "~/project/docs", "~/project/tests", "~/dotfiles"];
  let nextId = 2;
  let root = { kind: "leaf", id: 1 };
  let focused = 1;
  let zoomed = null;
  let armed = false;
  let timer = null;

  const leaves = (n) => (n.kind === "leaf" ? [n.id] : [...leaves(n.first), ...leaves(n.second)]);

  function replace(node, id, make) {
    if (node.kind === "leaf") return node.id === id ? make(node) : node;
    return { ...node, first: replace(node.first, id, make), second: replace(node.second, id, make) };
  }

  function remove(node, id) {
    if (node.kind === "leaf") return node.id === id ? null : node;
    const first = remove(node.first, id);
    const second = remove(node.second, id);
    if (!first) return second;
    if (!second) return first;
    return { ...node, first, second };
  }

  function rects(node, x, y, w, h, out) {
    if (node.kind === "leaf") out[node.id] = { x, y, w, h };
    else if (node.orientation === "row") {
      rects(node.first, x, y, w / 2, h, out);
      rects(node.second, x + w / 2, y, w / 2, h, out);
    } else {
      rects(node.first, x, y, w, h / 2, out);
      rects(node.second, x, y + h / 2, w, h / 2, out);
    }
    return out;
  }

  function neighbour(dir) {
    const all = rects(root, 0, 0, 1, 1, {});
    const a = all[focused];
    const horizontal = dir === "h" || dir === "l";
    let best = null;
    let bestGap = Infinity;
    for (const [key, b] of Object.entries(all)) {
      const id = Number(key);
      if (id === focused) continue;
      const overlaps = horizontal
        ? b.y < a.y + a.h - 1e-6 && a.y < b.y + b.h - 1e-6
        : b.x < a.x + a.w - 1e-6 && a.x < b.x + b.w - 1e-6;
      if (!overlaps) continue;
      const gap = { h: a.x - (b.x + b.w), l: b.x - (a.x + a.w), k: a.y - (b.y + b.h), j: b.y - (a.y + a.h) }[dir];
      if (gap > -1e-6 && gap < bestGap) { bestGap = gap; best = id; }
    }
    return best;
  }

  function el(tag, cls, text) {
    const e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text) e.textContent = text;
    return e;
  }

  function leafEl(id) {
    const leaf = el("div", id === focused ? "leaf focused" : "leaf");
    const head = el("div", "leaf-head");
    head.append(el("b", "", "Git Bash"), el("span", "", dirs[(id - 1) % dirs.length]));
    const body = el("div", "leaf-body");
    const l1 = el("div");
    l1.append(el("span", "fr", "┌─["), el("span", "dir", dirs[(id - 1) % dirs.length]), el("span", "fr", "]─["), el("span", "br", "main"), el("span", "fr", "]"));
    const l2 = el("div");
    l2.append(el("span", "fr", "└──╼ "), el("span", "ok", "$ "), el("span", "cursor"));
    body.append(l1, l2);
    leaf.append(head, body);
    leaf.addEventListener("click", () => { focused = id; render(); stage.focus({ preventScroll: true }); });
    return leaf;
  }

  function nodeEl(node) {
    if (node.kind === "leaf") return leafEl(node.id);
    const split = el("div", "split " + node.orientation);
    for (const child of [node.first, node.second]) {
      const cell = el("div");
      cell.append(nodeEl(child));
      split.append(cell);
    }
    return split;
  }

  function render() {
    stage.replaceChildren(zoomed ? leafEl(zoomed) : nodeEl(root));
    const n = leaves(root).length;
    count.textContent = zoomed && n > 1 ? `zoomed · ${n - 1} hidden` : `${n} pane${n === 1 ? "" : "s"}`;
  }

  function say(text) { status.textContent = text; }

  function act(key) {
    if (key === "v" || key === "\\" || key === "s" || key === "-") {
      if (leaves(root).length >= 8) return say("That's plenty of panes for a demo.");
      const orientation = key === "v" || key === "\\" ? "row" : "column";
      const id = nextId++;
      root = replace(root, focused, (leaf) => ({ kind: "split", orientation, first: leaf, second: { kind: "leaf", id } }));
      focused = id;
      zoomed = null;
      say(orientation === "row" ? "Split side by side." : "Split stacked.");
    } else if ("hjkl".includes(key)) {
      if (zoomed) return say("Zoomed: z to restore first.");
      const n = neighbour(key);
      if (n) { focused = n; say("Focus moved."); } else say("No pane that way.");
    } else if (key === "o") {
      const ids = leaves(root);
      focused = ids[(ids.indexOf(focused) + 1) % ids.length];
      if (zoomed) zoomed = focused;
      say("Next pane.");
    } else if (key === "z") {
      zoomed = zoomed ? null : focused;
      say(zoomed ? "Zoomed. z again to restore." : "Restored.");
    } else if (key === "x") {
      const ids = leaves(root);
      if (ids.length === 1) return say("The last pane stays.");
      const at = ids.indexOf(focused);
      root = remove(root, focused);
      const left = leaves(root);
      focused = left[Math.min(at, left.length - 1)];
      zoomed = null;
      say("Pane closed.");
    } else {
      return say("That key isn't in this demo — try v, -, h/j/k/l, o, z or x.");
    }
    render();
  }

  function disarm() {
    armed = false;
    clearTimeout(timer);
    prefixBtn.setAttribute("aria-pressed", "false");
    badge.classList.remove("on");
    badge.textContent = "Ctrl A prefix";
  }

  function arm() {
    armed = true;
    clearTimeout(timer);
    timer = setTimeout(() => { disarm(); say("Prefix timed out."); }, 1500);
    prefixBtn.setAttribute("aria-pressed", "true");
    badge.classList.add("on");
    badge.textContent = "PREFIX";
    say("Prefix on — now a command key.");
  }

  const arrows = { ArrowLeft: "h", ArrowDown: "j", ArrowUp: "k", ArrowRight: "l" };
  stage.addEventListener("keydown", (ev) => {
    if (ev.ctrlKey && ev.key.toLowerCase() === "a") {
      ev.preventDefault();
      arm();
    } else if (armed && !["Shift", "Control", "Alt", "Meta"].includes(ev.key)) {
      ev.preventDefault();
      disarm();
      act(arrows[ev.key] ?? ev.key.toLowerCase());
    }
  });
  stage.addEventListener("focus", () => { if (!armed) say("Press Ctrl+A, then v."); });
  stage.addEventListener("click", () => stage.focus({ preventScroll: true }));

  // The buttons do the same for touch screens and mice.
  document.getElementById("keypad").addEventListener("click", (ev) => {
    const key = ev.target.closest("button")?.dataset.key;
    if (key === "prefix") {
      if (armed) { disarm(); say(""); } else arm();
    } else if (key) {
      disarm();
      act(key);
    }
  });

  render();
})();

// ---------------------------------------------------------------------------
// Reveal sections as they scroll in (skipped under reduced motion via CSS).
(() => {
  const items = document.querySelectorAll(".reveal");
  if (!("IntersectionObserver" in window)) {
    items.forEach((e) => e.classList.add("in"));
    return;
  }
  const io = new IntersectionObserver((entries) => {
    for (const e of entries) {
      if (e.isIntersecting) {
        e.target.classList.add("in");
        io.unobserve(e.target);
      }
    }
  }, { rootMargin: "0px 0px -8% 0px", threshold: 0.08 });
  items.forEach((e) => io.observe(e));
})();

// ---------------------------------------------------------------------------
// Copy the installer's SHA-256.
(() => {
  const btn = document.getElementById("copy-hash");
  const hash = document.getElementById("hash");
  if (!btn || !hash || !navigator.clipboard) {
    if (btn) btn.hidden = true;
    return;
  }
  btn.addEventListener("click", async () => {
    try {
      await navigator.clipboard.writeText(hash.textContent.trim());
      btn.textContent = "Copied";
    } catch {
      btn.textContent = "Select it instead";
    }
    setTimeout(() => { btn.textContent = "Copy"; }, 1800);
  });
})();
