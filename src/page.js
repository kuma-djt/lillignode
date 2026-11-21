// Lillignode's page. It listens to Lillignode on this machine and shows what
// it hears. It sends nothing back: the only requests it makes are to read.
//
// Everything an agent wrote is put on the page as text, never as markup.
"use strict";

(() => {
  const el = (id) => document.getElementById(id);
  const contact = el("contact");
  const blocking = el("blocking");
  const blockingList = el("blocking-list");
  const agentsList = el("agents");
  const noAgents = el("no-agents");
  const terminal = el("terminal");
  const terminalNote = el("terminal-note");
  const announce = el("announce");
  let announced = "";
  // Say aloud only what has changed: who is waiting, and whether there is
  // contact. The counts that tick each second are not said.
  function say() {
    const waiting = state ? state.agents.filter((s) => s.running && s.needs_you).map((s) => s.label) : [];
    const words =
      (lostAt !== null ? "Lost contact with Lillignode. " : "") +
      (waiting.length ? "Needs you: " + waiting.join(", ") + "." : "");
    if (words !== announced) {
      announced = words;
      announce.textContent = words;
    }
  }
  const terminalName = el("terminal-name");
  const blockingTitle = el("blocking-title");

  // After this long without a word, the header says so. Lillignode sends
  // one at least every second.
  const LATE_MS = 3000;
  const RETRY_MS = 2000;

  let stream = null;
  let state = null;        // the last state heard
  let heardAt = null;      // when it was heard, by this page's clock
  let lostAt = null;       // when contact was lost, if it is
  let everHeard = false;
  let chosen = null;       // the agent the reader chose; null follows the agent in front
  let asked = undefined;   // the agent the open stream was asked for
  let run = null;
  let lastEvent = 0;
  let retry = null;        // the one pending look after contact was lost
  // Set once Lillignode is found answering under another word: each start
  // has its own, so this page's address will never work again.
  let movedOn = false;
  let drawn = null;        // the screen now drawn in the terminal

  // The 16 colours a terminal names, in the order terminals number them.
  const NAMED = [
    // The yellows lean to yellow, away from the orange that means needs you.
    "#1b2733", "#d9655f", "#7fbf7a", "#d9c85f", "#6c9bd9", "#b98ad9", "#4fb3a5", "#d8d2c4",
    "#5d6b78", "#f08a84", "#a3dd9d", "#f0e085", "#93b8ee", "#d3abee", "#7fd6c9", "#f2ead9",
  ];
  const STEPS = [0, 95, 135, 175, 215, 255];
  const hex = (n) => n.toString(16).padStart(2, "0");
  function indexed(i) {
    if (i < 16) return NAMED[i];
    if (i < 232) {
      const n = i - 16;
      return "#" + hex(STEPS[Math.floor(n / 36)]) + hex(STEPS[Math.floor(n / 6) % 6]) + hex(STEPS[n % 6]);
    }
    const grey = 8 + (i - 232) * 10;
    return "#" + hex(grey) + hex(grey) + hex(grey);
  }
  // A colour as it came: nothing for the terminal's own, a number for one
  // of the 256, or three numbers.
  function colour(c) {
    if (c === null || c === undefined) return null;
    if (typeof c === "number") return indexed(c);
    return "#" + hex(c[0]) + hex(c[1]) + hex(c[2]);
  }

  function ago(ms) {
    const s = Math.max(0, Math.floor(ms / 1000));
    if (s < 60) return s + " s";
    const m = Math.floor(s / 60);
    if (m < 60) return m + " min";
    const h = Math.floor(m / 60);
    return h + " h " + (m % 60) + " min";
  }
  const clock = (date) => date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });

  // How long ago something Lillignode timed happened: by Lillignode's own
  // clock up to the state, then by this page's clock since it arrived.
  function since(at) {
    if (at === null || at === undefined || !state) return null;
    // Once contact is lost nothing more is known: the count stops there.
    const now = lostAt === null ? Date.now() : lostAt.getTime();
    return state.taken_at - at + (now - heardAt);
  }

  function shownKey() {
    if (!state) return null;
    if (chosen !== null && state.agents.some((s) => s.key === chosen)) return chosen;
    const front = state.agents.find((s) => s.front);
    return front ? front.key : state.agents.length ? state.agents[0].key : null;
  }

  // What a row says after the name: the state, and how old it is.
  function stateWords(agent) {
    if (!agent.running) return [agent.words, ""];
    if (agent.needs_you) {
      const waited = since(agent.needs_you_since);
      return ["Needs you", waited === null ? "" : ", waiting " + ago(waited)];
    }
    const age = since(agent.reported_at);
    return [agent.words, age === null ? "" : ", " + ago(age) + " ago"];
  }

  function agentClass(agent) {
    if (!agent.running) return "gone";
    if (agent.needs_you) return "needs-you";
    return agent.state === "working" ? "working" : "";
  }

  function text(tag, cls, words) {
    const node = document.createElement(tag);
    if (cls) node.className = cls;
    node.textContent = words;
    return node;
  }

  function drawAgents() {
    const shown = shownKey();
    noAgents.hidden = !state || state.agents.length > 0;
    const rows = state ? state.agents : [];
    // Rows are kept and changed in place, so a row keeps the keyboard's
    // place while its words change.
    while (agentsList.children.length > rows.length) agentsList.lastChild.remove();
    rows.forEach((agent, i) => {
      let li = agentsList.children[i];
      if (!li) {
        li = document.createElement("li");
        const button = document.createElement("button");
        button.type = "button";
        const name = text("span", "agent-name", "");
        name.append(text("span", "agent-title", ""), text("span", "agent-folder", ""));
        const line = text("span", "agent-state", "");
        line.append(text("span", "word", ""), text("span", "age", ""));
        button.append(name, line);
        button.addEventListener("click", () => choose(Number(li.dataset.key)));
        li.append(button);
        agentsList.append(li);
      }
      li.dataset.key = String(agent.key);
      li.className = agentClass(agent);
      const button = li.firstChild;
      button.setAttribute("aria-pressed", agent.key === shown ? "true" : "false");
      button.querySelector(".agent-title").textContent = agent.name;
      button.querySelector(".agent-folder").textContent = agent.folder ? "  " + agent.folder : "";
      const [word, age] = stateWords(agent);
      button.querySelector(".word").textContent = word;
      button.querySelector(".age").textContent = age;
    });
  }

  function drawBlocking() {
    const waiting = state ? state.agents.filter((s) => s.running && s.needs_you) : [];
    blocking.hidden = waiting.length === 0;
    blockingTitle.textContent = lostAt === null ? "Blocking now" : "Blocking when last heard";
    blockingList.replaceChildren(
      ...waiting.map((agent) => {
        const waited = since(agent.needs_you_since);
        const what = agent.needs ? " needs you: " + agent.needs + "." : " needs you.";
        return text("li", "", agent.label + what + (waited === null ? "" : " Waiting " + ago(waited) + "."));
      }),
    );
    document.title = waiting.length ? "(" + waiting.length + " need you) Lillignode" : "Lillignode";
  }

  function drawTerminal() {
    const shown = shownKey();
    const agent = state && state.agents.find((s) => s.key === shown);
    const screen = state && state.screen && state.screen.key === shown ? state.screen : null;
    terminalName.textContent = agent ? agent.label : "";
    if (!agent) {
      terminalNote.textContent = state && state.agents.length ? "Choose an agent to see its terminal." : "";
      terminalNote.hidden = false;
      terminal.replaceChildren();
      drawn = null;
      return;
    }
    const notes = [];
    if (agent.program_note) notes.push(agent.program_note);
    if (!screen) notes.push("Waiting for its screen.");
    else if (screen.scrolled_back) {
      notes.push("Someone is reading this agent's history in Lillignode. This is what they see, not its live screen.");
    }
    terminalNote.textContent = notes.join(" ");
    terminalNote.hidden = notes.length === 0;
    if (!screen) {
      terminal.replaceChildren();
      drawn = null;
      return;
    }
    // The same screen as is already drawn: left alone, so text a reader
    // has selected in it stays selected.
    const text = JSON.stringify(screen);
    if (drawn === text) return;
    drawn = text;
    const lines = screen.lines.map((runs, row) => {
      const line = document.createElement("span");
      for (const run of runs) {
        const span = document.createElement("span");
        span.textContent = run.t;
        let fg = colour(run.fg);
        let bg = colour(run.bg);
        if (run.v) [fg, bg] = [bg || "#0E1820", fg || "#F2EAD9"];
        if (fg) span.style.color = fg;
        if (bg) span.style.backgroundColor = bg;
        if (run.b) span.style.fontWeight = "700";
        if (run.d) span.style.opacity = "0.6";
        if (run.i) span.style.fontStyle = "italic";
        if (run.u) span.style.textDecoration = "underline";
        line.append(span);
      }
      line.append(row + 1 < screen.lines.length ? "\n" : "");
      return line;
    });
    terminal.replaceChildren(...lines);
  }

  function drawContact() {
    document.body.classList.toggle("stale", lostAt !== null);
    if (movedOn) {
      contact.className = "lost";
      contact.textContent =
        "Lillignode was started again, with a new address for its page. This page cannot follow it and will not " +
        "change again. Open the address Lillignode shows now: its menu key, then s.";
      return;
    }
    if (lostAt !== null) {
      contact.className = "lost";
      contact.textContent = everHeard
        ? "Lost contact with Lillignode at " + clock(lostAt) + ". What is below is as it was then, and may be wrong now. Trying again."
        : "Cannot reach Lillignode. Nothing is shown. Trying again.";
      return;
    }
    if (heardAt === null) {
      contact.className = "";
      contact.textContent = "Waiting to hear from Lillignode.";
      return;
    }
    const quiet = Date.now() - heardAt;
    if (quiet >= LATE_MS) {
      contact.className = "late";
      contact.textContent = "No word from Lillignode for " + ago(quiet) + ". What is below may be out of date.";
    } else {
      contact.className = "";
      contact.textContent = "Last heard from Lillignode " + ago(quiet) + " ago.";
    }
  }

  function draw() {
    drawContact();
    drawBlocking();
    drawAgents();
    drawTerminal();
    say();
  }

  function choose(key) {
    chosen = key;
    draw();
    // The stream carries one agent's screen: ask for this one's. While
    // contact is lost the look already pending will ask for it.
    if (lostAt === null && !movedOn && asked !== shownKey()) listen();
  }

  function heard(message) {
    heardAt = Date.now();
    everHeard = true;
    lostAt = null;
    if (message.state) {
      const next = message.state;
      // Each run of Lillignode has its own address, so the run never
      // changes under an open page.
      run = next.run;
      for (const event of next.events) lastEvent = Math.max(lastEvent, event.n);
      state = next;
    }
    if (state) state.taken_at = message.at;
    draw();
    // The agent in front changed and the reader has chosen none: follow it.
    if (state && asked !== shownKey()) listen();
  }

  function lost() {
    if (stream) stream.close();
    stream = null;
    if (lostAt === null) lostAt = new Date();
    draw();
    if (movedOn) return;
    clearTimeout(retry);
    retry = setTimeout(lookAgain, RETRY_MS);
  }

  // Ask once, plainly, before listening again: a refusal that is
  // Lillignode's own means it is running under a new word.
  function lookAgain() {
    retry = null;
    fetch("state", { cache: "no-store", signal: AbortSignal.timeout(5000) })
      .then((answer) => (answer.status === 404 ? answer.text() : null))
      .then((said) => {
        if (said !== null && said.includes("Lillignode shows its page's address")) {
          movedOn = true;
          draw();
          return;
        }
        listen();
      })
      .catch(() => lost());
  }

  function listen() {
    if (stream) stream.close();
    asked = shownKey();
    const query = "?after=" + lastEvent + (run === null ? "" : "&run=" + run) + (asked === null ? "" : "&agent=" + asked);
    // Beside the page, under its word.
    const mine = new EventSource("stream" + query);
    stream = mine;
    mine.addEventListener("state", (e) => {
      if (stream === mine) heard(JSON.parse(e.data));
    });
    mine.addEventListener("ended", () => {
      if (stream === mine) lost();
    });
    mine.onerror = () => {
      if (stream === mine) lost();
    };
  }

  setInterval(draw, 1000);
  draw();
  listen();
})();
