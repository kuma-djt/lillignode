// Lillignode's page. It listens to Lillignode on this machine and shows what
// it hears. It sends nothing back, with one exception that Lillignode's
// settings must allow: the Send button under the terminal, which passes on
// the text in its box for the agent named beside it.
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
  const standing = el("standing");
  const typing = el("typing");
  const typingTo = el("typing-to");
  const typingText = el("typing-text");
  const typingSend = el("typing-send");
  const typingEnter = el("typing-enter");
  const typingSaid = el("typing-said");
  const typingAsk = el("typing-ask");
  const typingQuestion = el("typing-question");

  const READ_ONLY = "Read only. This page shows what Lillignode shows and sends nothing back.";
  const CAN_TYPE =
    "This page can type to your agents: what you send under the terminal goes to the agent named there, " +
    "as if you had typed it in Lillignode.";
  const CAN_ACT =
    "This page can act on your Board: what you press on a card is passed to Lillignode, which adds it to " +
    "the Board's outbox.";
  const CAN_BOTH =
    "This page can type to your agents and act on your Board. Text goes to the agent named under the " +
    "terminal; what you press on a card goes to the Board's outbox.";
  const NO_ANSWER =
    "No answer from Lillignode, so it is not known whether this was sent. Look at the agent's terminal " +
    "before sending it again.";
  // Stands for an agent of an earlier run of Lillignode: no agent has it.
  const GONE = Number.MAX_SAFE_INTEGER;

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
  // Set once Lillignode is found answering under another word: the word
  // was replaced, so this page's address will never work again.
  let movedOn = false;
  // The agent the text in the box was written for. The box never changes
  // whom it is for by itself: only the reader choosing another agent does.
  let draftFor = null;
  let sending = false;
  let asking = null;       // the send the reader is being asked about: its agent and text
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
    // With no agents: what Lillignode is and the one next step, in place of
    // an empty terminal.
    // Not while contact is lost or the page has stopped: what it would
    // promise ("it appears here at once") would not be so.
    const none = !!state && state.agents.length === 0 && lostAt === null && !movedOn;
    el("first-run").hidden = !none;
    terminal.hidden = none;
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
        // A tap on who is waiting opens that agent: a real button, in a list.
        const li = document.createElement("li");
        const open = document.createElement("button");
        open.type = "button";
        open.className = "blocking-open";
        open.textContent = agent.label + what + (waited === null ? "" : " Waiting " + ago(waited) + ".");
        open.addEventListener("click", () => choose(agent.key));
        li.append(open);
        return li;
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
        "This page's address was replaced with a new one, so this page has stopped and will not change again. " +
        "Open the address Lillignode shows now: its menu key, then s.";
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

  // The agent a Send would go to: the one the text was written for, or,
  // with the box empty, the one shown.
  function target() {
    return draftFor !== null ? draftFor : shownKey();
  }

  function drawTyping() {
    const token = state ? state.typing : null;
    const acts = !!(state && state.acting);
    standing.textContent = token && acts ? CAN_BOTH : token ? CAN_TYPE : acts ? CAN_ACT : READ_ONLY;
    // Where a program keeps the Board, a press goes to it, not to a file.
    if (acts && board && board.adapter) {
      const onward = board.adapter.name + ", ten seconds later unless you undo it.";
      standing.textContent = token
        ? "This page can type to your agents and act on your Board. Text goes to the agent named under the " +
          "terminal; what you press on a card goes to " + onward
        : "This page can act on your Board: what you press on a card is passed to Lillignode, which sends it to " + onward;
    }
    standing.className = token || acts ? "can-type" : "";
    const key = target();
    typing.hidden = !token || key === null;
    if (typing.hidden) return;
    const agent = state.agents.find((s) => s.key === key);
    typingTo.textContent = agent ? "To " + agent.label : "To an agent that is no longer open in Lillignode";
    typingEnter.textContent = agent ? "Press Enter in " + agent.label : "Press Enter";
    typingEnter.disabled = sending || asking !== null || lostAt !== null || movedOn || !agent;
    // Read only, not disabled, while sending or asking: it keeps the keyboard.
    typingText.readOnly = sending || asking !== null;
    typingSend.disabled =
      sending || asking !== null || lostAt !== null || movedOn || typingText.value === "";
    typingAsk.hidden = asking === null;
  }

  function said(outcome, words) {
    typingSaid.className = outcome;
    typingSaid.textContent = outcome === "ask" ? "" : words;
    typingQuestion.textContent = outcome === "ask" ? words : "";
  }

  // Pass on the text for one agent. `agreed`: the reader was asked whether
  // it may go line by line, and said yes. `enter`: Enter alone, with no
  // text. What Lillignode says came of it is shown as it was said.
  function send(agent, text, agreed, enter = false) {
    if (!state || !state.typing || agent === null || (text === "") !== enter || sending) return;
    sending = true;
    asking = null;
    said("", "Sending.");
    drawTyping();
    fetch("send", {
      method: "POST",
      cache: "no-store",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ token: state.typing, run: state.run, agent, text, agreed, enter }),
      signal: AbortSignal.timeout(10000),
    })
      .then((answer) => (answer.ok ? answer.json() : answer.text().then((words) => ({ outcome: "refused", words }))))
      .then((heard) => {
        sending = false;
        if (heard.outcome === "ask") {
          asking = { agent, text };
        } else if (heard.outcome === "sent" && !enter && typingText.value === text) {
          typingText.value = "";
          draftFor = null;
        }
        said(heard.outcome, heard.words);
        drawTyping();
      })
      .catch(() => {
        sending = false;
        said("refused", NO_ANSWER);
        drawTyping();
      });
  }

  typingText.addEventListener("input", () => {
    if (typingText.value === "") {
      draftFor = null;
    } else if (draftFor === null) {
      // From the first letter the text is for the agent shown now, and
      // the page stops following the agent in front.
      draftFor = shownKey();
      chosen = draftFor;
    }
    drawTyping();
  });
  // Ctrl+Enter (or Cmd+Enter) sends; Enter alone makes a new line.
  typingText.addEventListener("keydown", (e) => {
    if (e.key === "Enter" && (e.ctrlKey || e.metaKey) && !e.altKey && !e.shiftKey) {
      e.preventDefault();
      if (!typingSend.disabled) typingSend.click();
    }
  });
  // Enter alone, in the agent named on the button. Never focused for the
  // reader: nothing moves the keyboard to it.
  typingEnter.addEventListener("click", () => {
    const key = target();
    if (key === null || typingEnter.disabled) return;
    // A click leaves the keyboard on the button, where a stray Enter would
    // press it again: it goes back to the box.
    typingText.focus();
    send(key, "", false, true);
  });
  typingSend.addEventListener("click", () => {
    // Text that got into the box without being typed there (dropped, or
    // put back by the browser) is for the agent named beside it now, and
    // stays for that one.
    if (draftFor === null) {
      draftFor = shownKey();
      chosen = draftFor;
    }
    send(draftFor, typingText.value, false);
  });
  el("typing-yes").addEventListener("click", () => {
    if (asking) send(asking.agent, asking.text, true);
  });
  el("typing-no").addEventListener("click", () => {
    asking = null;
    said("refused", "Nothing was sent.");
    drawTyping();
  });

  function draw() {
    drawContact();
    drawBlocking();
    drawAgents();
    drawPhone();
    drawTerminal();
    drawTyping();
    say();
  }

  // At phone width the page is one thing at a time: the list of agents, or
  // one agent's terminal; the Board slides in over either. Same page, same
  // data: only what is shown changes.
  const phoneWidth = window.matchMedia("(max-width: 760px)");
  let phoneView = "list";
  let boardOpen = false;
  function drawPhone() {
    const phone = phoneWidth.matches;
    const agent = state && state.agents.find((s) => s.key === shownKey());
    // An agent that has gone leaves its view, and its history entry.
    if (phone && phoneView === "agent" && state && !agent) {
      phoneView = "list";
      if (history.state && history.state.lillignode === "agent") history.back();
    }
    document.body.classList.toggle("phone", phone);
    document.body.dataset.view = phone ? phoneView : "";
    document.body.classList.toggle("board-open", phone && boardOpen);
    // The header, and who is blocking under it, are pinned at the top: their
    // heights, from the layout (not where they happen to be scrolled to), so
    // the strip sits below the header and the Board opens below both.
    const header = document.querySelector("header");
    const headerHeight = phone ? Math.round(header.offsetHeight) : 0;
    const blockingHeight = phone && !blocking.hidden ? Math.round(blocking.offsetHeight) : 0;
    const root = document.documentElement.style;
    root.setProperty("--pinned-header", headerHeight + "px");
    root.setProperty("--pinned-all", headerHeight + blockingHeight + "px");
    el("board-shade").hidden = !(phone && boardOpen);
    // While the Board is open it is the only thing that takes the keyboard.
    for (const behind of [document.querySelector("main > #agents-column"), el("middle"), blocking, header]) {
      if (behind) behind.inert = phone && boardOpen && behind !== header;
    }
    el("board").setAttribute("role", phone ? "dialog" : "region");
    if (phone) el("board").setAttribute("aria-modal", boardOpen ? "true" : "false");
    else el("board").removeAttribute("aria-modal");
    el("board-open").setAttribute("aria-expanded", phone && boardOpen ? "true" : "false");
    const open = board ? (board.decide || []).length + (board.yours || []).length : 0;
    el("board-open").textContent = open ? "Board (" + open + ")" : "Board";
    el("back-to-agents").textContent = "\u2190 All agents" + (state && state.agents.length > 1 ? " (" + state.agents.length + ")" : "");
  }
  function showAgentView() {
    if (!phoneWidth.matches) return;
    if (phoneView !== "agent") history.pushState({ lillignode: "agent" }, "");
    phoneView = "agent";
    drawPhone();
    window.scrollTo(0, 0);
  }
  function showList() {
    phoneView = "list";
    drawPhone();
    window.scrollTo(0, 0);
  }
  function setBoard(open) {
    boardOpen = open;
    drawPhone();
    if (open) el("board-close").focus();
    else el("board-open").focus();
  }
  el("board-open").addEventListener("click", () => setBoard(!boardOpen));
  el("board-close").addEventListener("click", () => setBoard(false));
  el("board-shade").addEventListener("click", () => setBoard(false));
  el("back-to-agents").addEventListener("click", () => {
    if (history.state && history.state.lillignode === "agent") history.back();
    else showList();
  });
  // The phone's own Back takes the agent's terminal back to the list.
  window.addEventListener("popstate", () => showList());
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && boardOpen) setBoard(false);
  });
  // From phone width to wider: the Board is a column again, not a drawer,
  // and nothing waits to open by itself when the width comes back.
  phoneWidth.addEventListener("change", () => {
    if (!phoneWidth.matches) boardOpen = false;
    drawPhone();
  });
  window.addEventListener("resize", () => drawPhone());
  // A reload while an agent was open starts at the list, with no entry left
  // over for Back to land on.
  if (history.state && history.state.lillignode === "agent") history.replaceState(null, "");

  function choose(key) {
    showAgentView();
    chosen = key;
    // The reader's own choice is the one thing that changes whom the text
    // in the box is for; the label beside it says so.
    if (draftFor !== null) draftFor = key;
    // A question about text for another agent is not left standing under
    // this one's name: it is asked again, of this one, on the next Send.
    if (asking !== null && asking.agent !== key) {
      asking = null;
      said("refused", "Nothing was sent.");
    }
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
      // Lillignode was started again: its address is the same, but agents'
      // keys and event numbers have begun again. Nothing chosen before
      // names the same agent now, so nothing is carried over: text in the
      // box is kept, for no agent, until the reader chooses one.
      if (run !== null && next.run !== run) {
        lastEvent = 0;
        chosen = null;
        asking = null;
        if (draftFor !== null) draftFor = GONE;
      }
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

  // Ask once, plainly, before listening again. Lillignode started again
  // answers under the same word, and the page carries on; a refusal that
  // is Lillignode's own means the word was replaced.
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

  // The Board: read from Lillignode, which reads it from a file of the
  // user's, every few seconds. What a card says is put on the page as text.
  // Where Lillignode's settings allow it, a card has buttons: what is done
  // is passed to Lillignode, which adds a line to the Board's outbox. This
  // page writes nothing itself.
  const BOARD_MS = 2000;
  const UNDO_MS = 10000;
  const boardNote = el("board-note");
  const boardNotRead = el("board-not-read");
  const boardSaid = el("board-said");
  const KIND_WORDS = { decide: "Decide", fire: "Fire", question: "Question" };
  const ACTION_WORDS = { approve: "Approve", revise: "Revise", hold: "Hold", done: "Done", "got-it": "Got it" };
  const STATUS_WORDS = { revise: "being revised", hold: "on hold", more: "more was asked for" };
  const HELP_WORDS = { approve: "Approve: go ahead.", revise: "Revise: send it back with your note.", hold: "Hold: not now." };
  const HUB = "Approve this one on the Hub";
  // Cards opened whole by a tap, by id.
  const opened = new Set();
  let board = null;        // the Board last heard
  let boardDrawn = "";     // what the Board on the page was drawn from
  let revising = null;     // the card a note is being written for
  let noteDraft = "";      // that note, so far
  let actingNow = false;   // something done to a card is on its way

  // A card due on a day is shown as that day wherever the reader is; one
  // due at a moment, as the day that moment falls on here.
  const day = (ms, whole) =>
    new Date(ms).toLocaleDateString([], { day: "numeric", month: "short", ...(whole ? { timeZone: "UTC" } : {}) });
  // Buttons are offered only where Lillignode's settings allow acting, and
  // not while Lillignode cannot tell which cards were already acted on.
  const canAct = () => !!(state && state.acting) && !(board && board.acting_off);
  // A word from one of the tables above, and nothing for a key it has not.
  const word = (table, key) => (Object.hasOwn(table, key) ? table[key] : "");
  // A click that lands within this long of the Board changing is not taken:
  // the card under the pointer may not be the one that was read.
  const SETTLE_MS = 700;
  let cardsDrawn = "";     // the cards as last drawn, apart from what is waiting
  let cardsChangedAt = 0;  // when they last changed
  const undoable = (card) => !!card.sent && Date.now() - card.sent.at < UNDO_MS;

  // What is said under a card's title: what it is, who raised it, when it
  // is due, and where it stands if it is not simply waiting its turn.
  function metaWords(card, withKind) {
    const parts = [];
    if (withKind) parts.push(word(KIND_WORDS, card.kind) + (card.tier === "next" ? ", next" : ""));
    parts.push("Raised by " + card.raised_by);
    if (card.due_at !== null && card.due_at !== undefined) parts.push("Due " + day(card.due_at, card.due_whole_day));
    if (word(STATUS_WORDS, card.status)) parts.push(word(STATUS_WORDS, card.status));
    return parts.join(" · ");
  }

  function button(words, cls, onClick) {
    const b = text("button", cls, words);
    b.type = "button";
    b.disabled = actingNow || lostAt !== null || movedOn;
    b.addEventListener("click", onClick);
    return b;
  }

  // Pass on what was done to a card, or the undoing of it, and show what
  // Lillignode said came of it.
  function act(card, action, note, undo) {
    if (!canAct() || actingNow) return;
    if (!undo && Date.now() - cardsChangedAt < SETTLE_MS) {
      boardSaid.hidden = false;
      boardSaid.textContent = "The Board had only just changed, so that press was not taken. Read the card, then press again.";
      return;
    }
    actingNow = true;
    drawBoard();
    fetch("act", {
      method: "POST",
      cache: "no-store",
      headers: { "Content-Type": "application/json" },
      // With what was shown of the card: Lillignode refuses if the card
      // with that id now says something else.
      body: JSON.stringify({
        token: state.acting,
        card: card.id,
        action,
        note,
        undo,
        title: card.title,
        recommendation: card.recommendation === undefined ? null : card.recommendation,
      }),
      signal: AbortSignal.timeout(10000),
    })
      .then((answer) => (answer.ok ? answer.json() : answer.text().then((words) => ({ outcome: "refused", words }))))
      .then((heard) => {
        actingNow = false;
        if (heard.outcome === "sent" && revising === card.id) {
          revising = null;
          noteDraft = "";
        }
        boardSaid.hidden = false;
        boardSaid.textContent = heard.words;
        // Where a program keeps the Board, the card itself says what became
        // of it from here; the sentence would soon be out of date.
        if (board && board.adapter) {
          const said = heard.words;
          setTimeout(() => {
            if (boardSaid.textContent === said) boardSaid.hidden = true;
          }, UNDO_MS + 1000);
        }
        readBoard();
        drawBoard();
      })
      .catch(() => {
        actingNow = false;
        boardSaid.hidden = false;
        boardSaid.textContent =
          "No answer from Lillignode, so it is not known whether this was sent. Wait for the card to change before doing it again.";
        drawBoard();
      });
  }

  // What stands where a card's buttons would: that it was sent and waits,
  // with Undo while there is still time.
  function sentLine(card) {
    const did = word(ACTION_WORDS, card.sent.action) || "something";
    // Where a program keeps the Board: held for its ten seconds first.
    const onward = board && board.adapter ? (board.adapter.actions || []).filter((a) => a.card === card.id).pop() : null;
    const keeper = board && board.adapter ? board.adapter.name : null;
    const left = Math.max(1, Math.ceil((UNDO_MS - (Date.now() - card.sent.at)) / 1000));
    const gone = " It leaves this list when " + keeper + " closes it.";
    const state = onward ? onward.state : "earlier";
    const said = !keeper
      ? "Sent, waiting: " + did + "."
      : {
          held: "Held: " + did + " goes to " + keeper + " in " + left + " s.",
          sent: "Sent to " + keeper + ": " + did + ". Waiting for its answer.",
          late: "No answer from " + keeper + " yet: " + did + ".",
          taken: "Taken by " + keeper + ": " + did + "." + gone,
          unsure: "Not known whether " + keeper + " has it: " + did + ". Look there before acting again.",
          earlier: "Sent to " + keeper + " earlier: " + did + "." + gone,
        }[state] || "Sent to " + keeper + ": " + did + ".";
    const line = text("p", "card-sent", said);
    if (canAct() && undoable(card)) {
      line.append(" ", button("Undo", "undo", () => act(card, card.sent.action, "", true)));
    }
    return line;
  }

  function decideCard(card) {
    const li = document.createElement("li");
    li.className = "card " + (Object.hasOwn(KIND_WORDS, card.kind) ? card.kind : "") + (card.sent ? " waiting" : "");
    li.dataset.id = card.id;
    li.append(text("strong", "card-title", card.title));
    if (card.recommendation) {
      const said = text("p", "card-recommendation", card.recommendation);
      said.title = card.recommendation;
      li.append(said);
    }
    const meta = text("p", "card-meta", "");
    meta.append(text("span", "card-kind", word(KIND_WORDS, card.kind) + (card.tier === "next" ? ", next" : "")));
    meta.append(" · " + metaWords(card, false));
    li.append(meta);
    if (card.sent) {
      li.append(sentLine(card));
    } else if (canAct() && revising === card.id) {
      const row = text("div", "card-actions", "");
      const note = document.createElement("input");
      note.className = "note";
      note.type = "text";
      note.maxLength = 280;
      note.value = noteDraft;
      note.placeholder = "What should change?";
      note.setAttribute("aria-label", "What should change in: " + card.title);
      note.addEventListener("input", () => {
        noteDraft = note.value;
      });
      const sendNote = () => act(card, "revise", noteDraft, false);
      note.addEventListener("keydown", (e) => {
        if (e.key === "Enter") {
          e.preventDefault();
          sendNote();
        }
      });
      row.append(note, button("Send note", "main", sendNote), button("Cancel", "", () => {
        revising = null;
        noteDraft = "";
        drawBoard();
      }));
      li.append(row);
    } else if (card.hub_only) {
      // Not from here, whatever the settings: Lillignode refuses it too.
      li.append(text("p", "card-hub", HUB));
    } else if (canAct()) {
      // The buttons this card allows as it stands: a card on hold is not
      // offered Hold. Lillignode says which, and refuses any other.
      const allowed = (card.actions || []).filter((a) => Object.hasOwn(HELP_WORDS, a));
      const row = text("div", "card-actions", "");
      for (const a of allowed) {
        if (a === "approve") row.append(button("Approve", "main", () => act(card, "approve", "", false)));
        if (a === "revise") {
          row.append(
            button("Revise", "", () => {
              revising = card.id;
              noteDraft = "";
              drawBoard();
              const note = document.querySelector("#board input.note");
              if (note) note.focus();
            }),
          );
        }
        if (a === "hold") row.append(button("Hold", "", () => act(card, "hold", "", false)));
      }
      li.append(row, text("p", "card-help", allowed.map((a) => HELP_WORDS[a]).join(" ")));
    }
    openable(li, card);
    return li;
  }

  // A tap on a card, not on one of its buttons or its note, opens it whole:
  // its title and recommendation, not cut. Another tap closes it. It can be
  // reached and opened from the keyboard too.
  function openable(li, card) {
    li.tabIndex = 0;
    li.setAttribute("aria-expanded", opened.has(card.id) ? "true" : "false");
    if (opened.has(card.id)) li.classList.add("opened");
    const toggle = () => {
      if (opened.has(card.id)) opened.delete(card.id);
      else opened.add(card.id);
      drawBoard();
      const again = [...document.querySelectorAll("#board li")].find((x) => x.dataset.id === card.id);
      if (again) again.focus();
    };
    li.addEventListener("click", (e) => {
      if (e.target.closest("button, input")) return;
      toggle();
    });
    li.addEventListener("keydown", (e) => {
      if (e.target === li && (e.key === "Enter" || e.key === " ")) {
        e.preventDefault();
        toggle();
      }
    });
  }

  // A row of one of the two lists: its title, who raised it, and the one
  // thing that can be done to it.
  function listRow(card, action, words) {
    const li = document.createElement("li");
    li.dataset.id = card.id;
    if (card.sent) li.className = "waiting";
    li.append(text("span", "card-title", card.title));
    const foot = text("div", "card-foot", "");
    foot.append(text("span", "card-meta", metaWords(card, false) + (card.tier === "next" && action === "done" ? " · next" : "")));
    if (card.sent) foot.append(sentLine(card));
    else if (card.hub_only) foot.append(text("span", "card-hub", HUB));
    else if (canAct() && (card.actions || []).includes(action)) foot.append(button(words, "", () => act(card, action, "", false)));
    li.append(foot);
    openable(li, card);
    return li;
  }

  function drawBoard() {
    if (!board) return;
    // Drawn again only when what it would show has changed, so text a
    // reader has selected, or a note being written, is left alone.
    // The seconds left to undo each card: so a countdown is drawn again.
    const now = [...board.decide, ...board.yours, ...board.know]
      .filter(undoable)
      .map((c) => c.id + ":" + Math.ceil((UNDO_MS - (Date.now() - c.sent.at)) / 1000));
    const from = JSON.stringify([board, canAct(), now, revising, actingNow, lostAt !== null, movedOn, [...opened]]);
    if (from === boardDrawn) return;
    boardDrawn = from;
    // When the cards themselves last changed: what each says and where it
    // stands, not whether one is waiting or can still be undone.
    const cards = JSON.stringify([board.decide, board.yours, board.know].map((part) => part.map((c) => [c.id, c.title, c.recommendation])));
    if (cards !== cardsDrawn) {
      if (cardsDrawn !== "") cardsChangedAt = Date.now();
      cardsDrawn = cards;
    }
    const focused = document.activeElement && document.activeElement.classList.contains("note");
    boardNote.hidden = !(board.problem || board.empty);
    boardNote.textContent = board.problem || (board.empty ? emptyBoardWords() : "");
    const part = (id, cards, row) => {
      el(id).hidden = cards.length === 0;
      el(id + "-list").replaceChildren(...cards.map(row));
    };
    // A note is for a card that is still there to be revised.
    if (revising !== null && !board.decide.some((c) => c.id === revising && !c.sent)) {
      revising = null;
      noteDraft = "";
    }
    part("board-decide", board.decide, decideCard);
    part("board-yours", board.yours, (card) => listRow(card, "done", "✓ Done"));
    part("board-know", board.know, (card) => listRow(card, "got-it", "Got it"));
    drawAdapter();
    const under = [board.not_read, board.acting_off].filter(Boolean).join(" ");
    boardNotRead.hidden = under === "";
    boardNotRead.textContent = under;
    if (focused) {
      const note = document.querySelector("#board input.note");
      if (note) note.focus();
    }
  }

  // Where an adapter keeps the Board: who reports through it, and what came
  // of each action. Ages are counted against Lillignode's clock, not the
  // page's, so a page whose own clock is wrong still says them right; the
  // times themselves are the service's.
  function drawAdapter() {
    const adapter = board.adapter;
    const reporting = adapter ? adapter.reporting || [] : [];
    el("reporting").hidden = reporting.length === 0;
    el("reporting-title").textContent = "Reporting through " + (adapter ? adapter.name : "the adapter");
    el("reporting-list").replaceChildren(...reporting.map((r) => {
      const li = text("li", "reporting-row", "");
      li.append(text("span", "reporting-agent", r.agent));
      const age = r.at === null || r.at === undefined ? "" : " " + ago(Math.max(0, adapter.now - r.at)) + " ago";
      li.append(text("span", "reporting-age quiet", age));
      li.append(text("p", "reporting-line", r.line || "(nothing said)"));
      return li;
    }));
    const actions = adapter ? adapter.actions || [] : [];
    el("board-sent-on").hidden = actions.length === 0;
    el("board-sent-on").replaceChildren(...actions.slice(-5).reverse().map((a) => {
      const did = word(ACTION_WORDS, a.action) || a.action;
      return text("li", "sent-on sent-on-" + a.state, did + " on “" + a.title + "”: " + a.words);
    }));
    const quiet = adapter && adapter.board_quiet_secs !== null && adapter.board_quiet_secs >= 15
      ? "The Board was last heard from " + adapter.name + " " + ago(adapter.board_quiet_secs * 1000) + " ago" +
        (adapter.board_quiet_secs >= 60 ? "; nothing on it can be acted on until it is heard from again." : ".")
      : null;
    const trouble = adapter && adapter.problem
      ? adapter.problem + " The Board below is as it last came" +
        (adapter.board_quiet_secs !== null ? ", " + ago(adapter.board_quiet_secs * 1000) + " ago" : "") +
        "; nothing on it can be acted on until it comes through again."
      : quiet;
    const said = adapter && [adapter.stopped, trouble].filter(Boolean).join(" ");
    if (said && !board.problem) {
      boardNote.hidden = false;
      boardNote.textContent = said;
    }
    boardNote.classList.toggle("board-trouble", !!said);
  }

  // What an empty Board says: where cards come from, so a reader who has
  // none yet knows how they would appear.
  function emptyBoardWords() {
    if (board.adapter) return "Nothing on the Board at " + board.adapter.name + ".";
    if (!board.no_file) return "Nothing open on the Board.";
    return "Nothing on the Board. The Board shows things to decide and to do, from a file you keep " +
      "(board.json, beside your workspace: lillignode --where says where) or from a program your settings name. " +
      "None is needed to run agents.";
  }

  function readBoard() {
    if (movedOn || lostAt !== null) return;
    fetch("board", { cache: "no-store", signal: AbortSignal.timeout(5000) })
      .then((answer) => (answer.ok ? answer.json() : null))
      .then((heard) => {
        if (heard && heard.board) {
          board = heard.board;
          drawBoard();
        }
      })
      .catch(() => {});
  }

  setInterval(drawBoard, 1000);
  setInterval(draw, 1000);
  setInterval(readBoard, BOARD_MS);
  draw();
  listen();
  readBoard();
})();
