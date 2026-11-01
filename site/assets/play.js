// The homepage demo: a small model of SplitView's layout tree and prefix keymap.
// Like the app, it only takes keys while it has focus, and only after the prefix.
(() => {
  const stage = document.getElementById("stage");
  const status = document.getElementById("play-status");
  if (!stage) return;

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
    if (node.kind === "leaf") {
      out[node.id] = { x, y, w, h };
    } else if (node.orientation === "row") {
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

  function leafEl(id) {
    const el = document.createElement("div");
    el.className = id === focused ? "leaf focused" : "leaf";
    el.innerHTML =
      '<div class="frame">┌─[<span class="dir">pane ' + id + "</span>]</div>" +
      '<div class="frame">└──╼ <span class="dollar">$</span> <span class="cursor"></span></div>';
    return el;
  }

  function nodeEl(node) {
    if (node.kind === "leaf") return leafEl(node.id);
    const el = document.createElement("div");
    el.className = "split " + node.orientation;
    for (const child of [node.first, node.second]) {
      const cell = document.createElement("div");
      cell.append(nodeEl(child));
      el.append(cell);
    }
    return el;
  }

  function render() {
    stage.replaceChildren(zoomed ? leafEl(zoomed) : nodeEl(root));
  }

  function act(key) {
    if (key === "v" || key === "\\" || key === "s" || key === "-") {
      if (leaves(root).length >= 8) return say("That's enough panes for a demo.");
      const orientation = key === "v" || key === "\\" ? "row" : "column";
      const id = nextId++;
      root = replace(root, focused, (leaf) => ({ kind: "split", orientation, first: leaf, second: { kind: "leaf", id } }));
      focused = id;
      zoomed = null;
    } else if ("hjkl".includes(key)) {
      if (zoomed) return;
      focused = neighbour(key) ?? focused;
    } else if (key === "o") {
      const ids = leaves(root);
      focused = ids[(ids.indexOf(focused) + 1) % ids.length];
      if (zoomed) zoomed = focused;
    } else if (key === "z") {
      zoomed = zoomed ? null : focused;
    } else if (key === "x") {
      const ids = leaves(root);
      if (ids.length === 1) return say("The last pane stays.");
      const at = ids.indexOf(focused);
      root = remove(root, focused);
      const left = leaves(root);
      focused = left[Math.min(at, left.length - 1)];
      zoomed = null;
    } else {
      return say("That key isn't in this demo.");
    }
    say("");
    render();
  }

  function say(text) { status.textContent = text; }

  function disarm() {
    armed = false;
    clearTimeout(timer);
    document.getElementById("prefix").setAttribute("aria-pressed", "false");
  }

  function arm() {
    armed = true;
    clearTimeout(timer);
    timer = setTimeout(disarm, 1500);
    document.getElementById("prefix").setAttribute("aria-pressed", "true");
    say("Prefix on. Press a command key.");
  }

  stage.addEventListener("keydown", (ev) => {
    if (ev.ctrlKey && ev.key.toLowerCase() === "a") {
      ev.preventDefault();
      arm();
    } else if (armed && !["Shift", "Control", "Alt", "Meta"].includes(ev.key)) {
      ev.preventDefault();
      disarm();
      act(ev.key.toLowerCase());
    }
  });
  stage.addEventListener("focus", () => {
    if (!armed) say("Press Ctrl+A, then v.");
  });
  stage.addEventListener("click", () => stage.focus());

  // The buttons do the same for touch screens. They run the command directly.
  document.querySelector(".play-keys").addEventListener("click", (ev) => {
    const key = ev.target.closest("button")?.dataset.key;
    if (key === "prefix") {
      if (armed) { disarm(); say(""); } else arm();
      stage.focus({ preventScroll: true });
    } else if (key) {
      disarm();
      act(key);
    }
  });

  render();
})();
