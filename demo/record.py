#!/usr/bin/env python3
"""Records the README's demos as animated SVGs, with nothing but the
standard library, tmux and a debug build of socratik.

Each scene runs socratik in a private tmux server with its own data folder.
A small server in this script stands in for the Claude API: it answers each
request with the next reply in the scene's script, streamed the way the API
streams. Keys go in with `send-keys`; the pane is read with `capture-pane -e`
every 0.1 s, and the frames become one SVG whose CSS animation steps through
them. Nothing touches your own data, settings or API key, and nothing is
billed.

usage: demo/record.py [scene ...]     (default: every scene)
"""

import html
import json
import os
import re
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
import unicodedata
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
OUT = ROOT / "docs" / "media"
BIN = ROOT / "target" / "debug" / "socratik"
COLS, ROWS = 120, 30
DAY = 86_400

FG, BG = "#ffffff", "#282c34"


def run(*args, cwd=None, env=None, check=True, inp=None):
    r = subprocess.run(args, cwd=cwd, env=env, input=inp, capture_output=True, text=True)
    if check and r.returncode != 0:
        sys.exit(f"{args}: {r.stderr}")
    return r.stdout


# ---- the stand-in model ----------------------------------------------------

def say(text):
    return {"text": text}


def call(name, **input):
    return {"tools": [(name, input)]}


class Model:
    """Answers POST /v1/messages from a script, one reply per request."""

    def __init__(self):
        self.script, self.problems, self.expect_tool_result = [], [], False
        model = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *a):
                pass

            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers["content-length"])))
                model.answer(self, body)

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.port = self.server.server_address[1]
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def answer(self, handler, body):
        last = body["messages"][-1]["content"][0]
        if self.expect_tool_result != (last.get("type") == "tool_result"):
            self.problems.append(f"out of step at: {json.dumps(last)[:120]}")
        reply = self.script.pop(0) if self.script else say("(the script ran out)")
        self.expect_tool_result = bool(reply.get("tools"))

        handler.send_response(200)
        handler.send_header("content-type", "text/event-stream")
        handler.end_headers()

        def send(event):
            handler.wfile.write(f"data: {json.dumps(event)}\n\n".encode())
            handler.wfile.flush()

        send({"type": "message_start",
              "message": {"usage": {"input_tokens": len(json.dumps(body)) // 4}}})
        index, words = 0, 0
        time.sleep(0.6)
        if text := reply.get("text"):
            send({"type": "content_block_start", "index": index,
                  "content_block": {"type": "text", "text": ""}})
            for chunk in re.findall(r"\S+\s*|\s+", text):
                send({"type": "content_block_delta", "index": index,
                      "delta": {"type": "text_delta", "text": chunk}})
                words += 1
                time.sleep(0.03)
            send({"type": "content_block_stop", "index": index})
            index += 1
        for i, (name, input) in enumerate(reply.get("tools", [])):
            send({"type": "content_block_start", "index": index,
                  "content_block": {"type": "tool_use", "id": f"toolu_{time.time_ns()}_{i}",
                                    "name": name, "input": {}}})
            send({"type": "content_block_delta", "index": index,
                  "delta": {"type": "input_json_delta", "partial_json": json.dumps(input)}})
            send({"type": "content_block_stop", "index": index})
            words += len(json.dumps(input)) // 5
            index += 1
        send({"type": "message_delta",
              "delta": {"stop_reason": "tool_use" if reply.get("tools") else "end_turn"},
              "usage": {"output_tokens": int(words * 1.4) + 40}})
        send({"type": "message_stop"})

    def check(self):
        if self.script:
            self.problems.append(f"{len(self.script)} replies were never asked for")
        if self.problems:
            raise RuntimeError("the model script and the app disagree:\n" + "\n".join(self.problems))


MODEL = None


# ---- seeding the database ----------------------------------------------------

SCHEMA = re.search(r'const SCHEMA: &str = "(.*?)";', (ROOT / "src" / "db.rs").read_text(), re.S)[1]


class Seed:
    def __init__(self, path):
        self.db = sqlite3.connect(path)
        self.db.executescript(SCHEMA)
        self.now = int(time.time())

    def topic(self, title, subskills, notes="", days_ago=10, target=4):
        """`subskills` is a list of (name, level, due in days or None)."""
        c = self.db.execute("INSERT INTO topics(title, target_level, notes, created_at) VALUES (?,?,?,?)",
                            (title, target, notes, self.now - days_ago * DAY))
        topic = c.lastrowid
        ids = {}
        for pos, (name, level, due) in enumerate(subskills, 1):
            due_at = None if due is None else self.now + int(due * DAY)
            c = self.db.execute(
                "INSERT INTO subskills(topic_id, name, position, level, due_at, interval_days, reps)"
                " VALUES (?,?,?,?,?,?,?)",
                (topic, name, pos, level, due_at, 3.0 if level >= 3 else 1.0, 2 if level >= 3 else 1))
            ids[name] = c.lastrowid
        self.db.commit()
        return topic, ids

    def event(self, subskill, old, new, reason, days_ago):
        self.db.execute("INSERT INTO level_events(subskill_id, old_level, new_level, reason, at)"
                        " VALUES (?,?,?,?,?)", (subskill, old, new, reason, self.now - int(days_ago * DAY)))
        self.db.commit()

    def card(self, topic, subskill, front, back):
        self.db.execute("INSERT INTO cards(topic_id, subskill_id, front, back, created_at, due_at)"
                        " VALUES (?,?,?,?,?,?)", (topic, subskill, front, back, self.now - DAY, self.now - 60))
        self.db.commit()

    def session(self, topic, kind, days_ago, talk, tokens=(18_000, 2_400)):
        """`talk` alternates the tutor's and the learner's messages, tutor first."""
        at = self.now - int(days_ago * DAY)
        c = self.db.execute("INSERT INTO sessions(topic_id, kind, started_at, input_tokens, output_tokens)"
                            " VALUES (?,?,?,?,?)", (topic, kind, at, *tokens))
        session = c.lastrowid
        rows = [("user", "Let's continue the course where we left off.", 1)]
        rows += [("assistant" if i % 2 == 0 else "user", text, 0) for i, text in enumerate(talk)]
        for i, (role, text, hidden) in enumerate(rows):
            self.db.execute("INSERT INTO messages(session_id, role, content, hidden, at) VALUES (?,?,?,?,?)",
                            (session, role, json.dumps([{"type": "text", "text": text}]), hidden, at + i * 40))
        self.db.commit()


# ---- a private tmux server running socratik ---------------------------------

WORLDS = []


class World:
    def __init__(self, configured=True, seed=None):
        WORLDS.append(self)
        self.tmp = tempfile.TemporaryDirectory()
        self.base = Path(self.tmp.name).resolve()
        os.chmod(self.base, 0o700)
        # The app's own default folder, under a throwaway home.
        self.data = self.base / ".local" / "share" / "socratik"
        self.data.mkdir(parents=True)
        if configured:
            (self.data / "config.json").write_text(json.dumps({
                "provider": "anthropic", "base_url": f"http://127.0.0.1:{MODEL.port}/v1",
                "key": "demo", "model": "claude-fable-5-2"}))
        if seed:
            seed(Seed(self.data / "socratik.db"))
        self.socket = f"socratikdemo-{os.getpid()}"
        conf = self.base / "tmux.conf"
        conf.write_text("set -g status off\nset -g escape-time 10\n"
                        "set -g default-terminal tmux-256color\n")
        # `env` runs after the shell's startup files, so a key they export
        # cannot reach the app.
        cmd = ["env", "-u", "ANTHROPIC_API_KEY", "-u", "OPENAI_API_KEY", "-u", "HF_TOKEN",
               "-u", "SOCRATIK_MODEL", "-u", "SOCRATIK_DATA_DIR", "-u", "XDG_DATA_HOME",
               f"HOME={self.base}", str(BIN)]
        self.tmux("-f", str(conf), "new-session", "-d", "-s", "demo", "-x", str(COLS), "-y", str(ROWS),
                  " ".join(cmd))
        self.pane = self.tmux("display-message", "-p", "-t", "demo", "#{pane_id}").strip()
        self.caption = None
        self.see("Socratik")

    def db(self):
        return sqlite3.connect(self.data / "socratik.db")

    def tmux(self, *args):
        env = {k: v for k, v in os.environ.items() if k not in ("TMUX", "TMUX_PANE")}
        return run("tmux", "-L", self.socket, *args, env=env)

    def screen(self):
        return self.tmux("capture-pane", "-p", "-e", "-t", self.pane)

    def see(self, text, timeout=20):
        end = time.time() + timeout
        while text not in self.tmux("capture-pane", "-p", "-t", self.pane):
            if time.time() > end:
                raise TimeoutError(f"waiting for {text!r}:\n{self.tmux('capture-pane', '-p', '-t', self.pane)}")
            time.sleep(0.05)

    def idle(self, timeout=30):
        """Waits until the tutor has finished replying."""
        end = time.time() + timeout
        while "the tutor is replying" in self.tmux("capture-pane", "-p", "-t", self.pane):
            if time.time() > end:
                raise TimeoutError("the tutor never finished")
            time.sleep(0.05)

    def keys(self, *keys, gap=0.6):
        for k in keys:
            self.tmux("send-keys", "-t", self.pane, k)
            time.sleep(gap)

    def type(self, text, delay=0.03):
        for c in text:
            self.tmux("send-keys", "-t", self.pane, "-l", c)
            time.sleep(delay)

    def answer(self, text, pause=0.8):
        """Types an answer, sends it and waits for the tutor's reply."""
        self.type(text)
        time.sleep(pause)
        self.keys("Enter", gap=0.3)
        time.sleep(0.5)
        self.idle()

    def close(self):
        if self in WORLDS:
            WORLDS.remove(self)
        subprocess.run(["tmux", "-L", self.socket, "kill-server"], capture_output=True)
        self.tmp.cleanup()


class Recorder:
    def __init__(self, world):
        self.world, self.frames, self.stop = world, [], threading.Event()
        self.thread = threading.Thread(target=self.loop, daemon=True)

    def loop(self):
        start = time.time()
        while not self.stop.is_set():
            s, caption = self.world.screen(), self.world.caption
            if not self.frames or self.frames[-1][1:] != (s, caption):
                self.frames.append((time.time() - start, s, caption))
            time.sleep(0.1)
        self.end = time.time() - start

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, *exc):
        self.stop.set()
        self.thread.join()


# ---- turning frames into an SVG (as in attention-queue's recorder) -----------

PALETTE = ["#000000", "#cd3131", "#0dbc79", "#e5e510", "#2472c8", "#bc3fbc", "#11a8cd", "#e5e5e5",
           "#7f848e", "#f14c4c", "#23d18b", "#f5f543", "#3b8eea", "#d670d6", "#29b8db", "#ffffff"]


def colour_256(n):
    if n < 16:
        return PALETTE[n]
    if n < 232:
        n -= 16
        steps = [0, 95, 135, 175, 215, 255]
        return "#%02x%02x%02x" % (steps[n // 36], steps[n // 6 % 6], steps[n % 6])
    v = 8 + (n - 232) * 10
    return "#%02x%02x%02x" % (v, v, v)


def sgr(params, st):
    ps = [int(p) if p else 0 for p in params.split(";")] if params else [0]
    i = 0
    while i < len(ps):
        p = ps[i]
        if p == 0:
            st.update(fg=None, bg=None, bold=False, dim=False, italic=False, under=False, rev=False)
        elif p == 1:
            st["bold"] = True
        elif p == 2:
            st["dim"] = True
        elif p == 3:
            st["italic"] = True
        elif p == 4:
            st["under"] = True
        elif p == 7:
            st["rev"] = True
        elif p == 22:
            st["bold"] = st["dim"] = False
        elif p == 23:
            st["italic"] = False
        elif p == 24:
            st["under"] = False
        elif p == 27:
            st["rev"] = False
        elif p in (38, 48):
            key = "fg" if p == 38 else "bg"
            if i + 1 < len(ps) and ps[i + 1] == 2:
                st[key] = "#%02x%02x%02x" % tuple(ps[i + 2:i + 5])
                i += 4
            elif i + 1 < len(ps) and ps[i + 1] == 5:
                st[key] = colour_256(ps[i + 2])
                i += 2
        elif p == 39:
            st["fg"] = None
        elif p == 49:
            st["bg"] = None
        elif 30 <= p <= 37:
            st["fg"] = PALETTE[p - 30]
        elif 90 <= p <= 97:
            st["fg"] = PALETTE[p - 82]
        elif 40 <= p <= 47:
            st["bg"] = PALETTE[p - 40]
        elif 100 <= p <= 107:
            st["bg"] = PALETTE[p - 92]
        i += 1


def cells(line):
    """(column, char, width, style) for each printed character."""
    st = dict(fg=None, bg=None, bold=False, dim=False, italic=False, under=False, rev=False)
    out, col, i = [], 0, 0
    while i < len(line):
        c = line[i]
        if c == "\x1b" and i + 1 < len(line) and line[i + 1] == "[":
            j = i + 2
            while j < len(line) and not line[j].isalpha():
                j += 1
            if j < len(line) and line[j] == "m":
                sgr(line[i + 2:j], st)
            i = j + 1
            continue
        if unicodedata.combining(c):
            i += 1
            continue
        w = 2 if unicodedata.east_asian_width(c) in "WF" else 1
        fg, bg = st["fg"] or FG, st["bg"] or BG
        if st["rev"]:
            fg, bg = bg, fg
        out.append((col, c, w, (fg, bg, st["bold"], st["dim"], st["italic"], st["under"])))
        col += w
        i += 1
    return out


FS, CW, LH, PAD = 14, 8.43, 18, 12


def row_svg(line):
    parts = []
    cs = cells(line)
    k = 0
    while k < len(cs):
        col, _, w, style = cs[k]
        bg, end = style[1], col + w
        j = k + 1
        while j < len(cs) and cs[j][3][1] == bg:
            end = cs[j][0] + cs[j][2]
            j += 1
        if bg != BG:
            parts.append(f'<rect x="{col * CW:.1f}" width="{(end - col) * CW:.1f}" '
                         f'height="{LH}" fill="{bg}"/>')
        k = j
    k = 0
    while k < len(cs):
        col, _, _, style = cs[k]
        j, text = k, ""
        while j < len(cs) and cs[j][3][0] == style[0] and cs[j][3][2:] == style[2:]:
            text += cs[j][1]
            j += 1
        k = j
        if not text.strip():
            continue
        lead = len(text) - len(text.lstrip(" "))
        text = text.strip(" ")
        x = (col + lead) * CW
        fg, _, bold, dim, italic, under = style
        attrs = f'x="{x:.1f}" y="{FS}" fill="{fg}"'
        if bold:
            attrs += ' font-weight="bold"'
        if dim:
            attrs += ' fill-opacity="0.6"'
        if italic:
            attrs += ' font-style="italic"'
        if under:
            attrs += ' text-decoration="underline"'
        width = sum(2 if unicodedata.east_asian_width(c) in "WF" else 1 for c in text)
        if width > 1:
            attrs += f' textLength="{width * CW:.1f}" lengthAdjust="spacingAndGlyphs"'
        parts.append(f"<text {attrs}>{html.escape(text, quote=False)}</text>")
    return "".join(parts)


CAPTION_FS, CAPTION_H = 17, 44


def to_svg(frames, hold=4.0, max_idle=2.5):
    # Long waits are cut short; the last frame stays up a while before looping.
    times, t, prev = [], 0.0, 0.0
    for at, *_ in frames:
        t += min(at - prev, max_idle)
        prev = at
        times.append(t)
    duration = times[-1] + hold
    captioned = any(c for *_, c in frames)
    w = COLS * CW + 2 * PAD
    h = ROWS * LH + 2 * PAD + (CAPTION_H if captioned else 0)
    rows, defs, uses = {}, [], []
    for i, (_, screen, caption) in enumerate(frames):
        frame = []
        for r, line in enumerate(screen.split("\n")[:ROWS]):
            if not line.strip("\x1b[0m "):
                continue
            if line not in rows:
                rows[line] = f"r{len(rows)}"
                defs.append(f'<g id="{rows[line]}">{row_svg(line)}</g>')
            frame.append(f'<use href="#{rows[line]}" y="{r * LH}"/>')
        if caption:
            frame.append(f'<text class="cap" x="0" y="{ROWS * LH + PAD + 27}">'
                         f'{html.escape(caption, quote=False)}</text>')
        uses.append(f'<g transform="translate(0 {i * h})">{"".join(frame)}</g>')
    keys = [f"{t / duration * 100:.3f}%{{transform:translateY({-i * h}px)}}" for i, t in enumerate(times)]
    keys.append(f"100%{{transform:translateY({-(len(frames) - 1) * h}px)}}")
    return f'''<svg xmlns="http://www.w3.org/2000/svg" width="{w:.0f}" height="{h:.0f}" viewBox="0 0 {w:.0f} {h:.0f}">
<style>
text{{font-family:"JetBrains Mono","DejaVu Sans Mono",Menlo,Consolas,monospace;font-size:{FS}px;white-space:pre}}
.cap{{font-family:-apple-system,"Segoe UI",Helvetica,Arial,sans-serif;font-size:{CAPTION_FS}px;fill:#e6e6e6}}
.film{{animation:play {duration:.2f}s step-end infinite}}
@keyframes play{{{"".join(keys)}}}
</style>
<defs>{"".join(defs)}</defs>
<rect width="100%" height="100%" rx="8" fill="{BG}"/>
{f'<rect y="{ROWS * LH + 2 * PAD}" width="100%" height="{CAPTION_H}" fill="#1d2026"/>' if captioned else ''}
<svg x="{PAD}" y="{PAD}" width="{COLS * CW:.0f}" height="{h - 2 * PAD}">
<g class="film">{"".join(uses)}</g>
</svg>
</svg>
'''


# ---- the scenes ----------------------------------------------------------------

BIG_O = ["Counting steps", "Dropping constants", "Dominant terms", "Nested loops",
         "Logarithmic growth", "Best, worst, average"]


def scene_hero():
    """The README's first video: a topic from the first question to a review, captioned."""
    MODEL.script = [
        call("define_subskills", subskills=BIG_O),
        say("I'll start with a few questions, one at a time, to see what you already know.\n\n"
            "A function loops once over a list of n items and does one comparison per item. "
            "If n doubles, roughly what happens to its running time, and why?"),
        say("Next one. A function loops over n items, and inside that loop it loops over the same "
            "n items again. What is its Big-O, and what happens to the running time when n doubles?"),
        say("Take n = 3 and trace it. For the first item of the outer loop, how many times does the "
            "inner loop's body run? And for all three outer items together?"),
        call("update_levels", updates=[
            {"subskill": "Counting steps", "level": 3, "reason": "explained linear growth unprompted"},
            {"subskill": "Dropping constants", "level": 2, "reason": "knows the rule, misapplied it"},
            {"subskill": "Dominant terms", "level": 1, "reason": "named it, no example"},
            {"subskill": "Nested loops", "level": 2, "reason": "got n² only after tracing"}]),
        say("That's the diagnostic done; your levels are on the right.\n\n"
            "I suggest we start with nested loops, since you got there by tracing but not on your own. "
            "Why does the inner loop's work multiply with the outer loop's, instead of adding to it?"),
        call("add_flashcards", cards=[
            {"subskill": "Counting steps", "front": "A loop does one step per item. What happens to its time when n doubles?",
             "back": "It doubles: O(n) grows in step with n."},
            {"subskill": "Nested loops", "front": "What is the Big-O of a loop over n inside a loop over n?",
             "back": "O(n²): the inner body runs n times for each of the n outer items."},
            {"subskill": "Nested loops", "front": "If an O(n²) function's input doubles, how much slower is it?",
             "back": "About four times: (2n)² = 4n²."}]),
        say("I've saved three flashcards on linear growth and nested loops. Back to the question: "
            "why does the inner loop's work multiply?"),
        say("Two concepts are due. First: a function loops over n items, and for each one it calls a "
            "helper that loops over all n items. What is its Big-O?"),
        call("record_review", results=[
            {"subskill": "Nested loops", "grade": "good", "level": 3, "note": "correct and unprompted"}]),
        say("Right, and you said why. Next: counting steps."),
    ]
    w = World()
    with Recorder(w) as rec:
        w.caption = "Socratik is a tutor in your terminal. Press n and say what you want to learn."
        time.sleep(3.5)
        w.keys("n", gap=0.8)
        w.type("Big-O notation", delay=0.06)
        time.sleep(1)
        w.keys("Enter", gap=0.3)
        w.caption = "The tutor splits the topic into subskills, then asks a few questions to place you."
        w.idle()
        time.sleep(5)
        w.caption = "You answer in your own words."
        w.answer("It doubles, because there is one comparison per item, so twice the items is twice the work.")
        time.sleep(3)
        w.answer("O(n). There are two loops over n, so it's 2n, and you drop the constant.")
        w.caption = "A wrong answer gets a narrower question, not the answer."
        time.sleep(5)
        w.caption = "You answer in your own words."
        w.answer("3 times for each outer item, so 9 in total. So it's n times n: O(n²), "
                 "and doubling n makes it 4 times slower.")
        w.caption = "Each subskill gets a level from 0 to 5. Levels are earned on evidence, never given."
        time.sleep(6)
        w.caption = "Ctrl+F turns what you have worked through into flashcards."
        time.sleep(1.5)
        w.keys("C-f", gap=0.5)
        w.idle()
        time.sleep(4)
        w.keys("Escape", gap=0.5)
        w.caption = "Each topic keeps its progress, with every level change and the reason for it."
        time.sleep(5)
        w.caption = "f reviews the flashcards that are due. They also export to Anki."
        w.keys("f", gap=1.5)
        for grade in ("3", "3", "4"):
            w.keys("Space", gap=1.8)
            w.keys(grade, gap=1.2)
        # Time passes: two concepts come due.
        with w.db() as db:
            db.execute("UPDATE subskills SET due_at = ? WHERE name IN ('Counting steps', 'Nested loops')",
                       (int(time.time()) - 60,))
        w.keys("Escape", gap=0.5)
        w.caption = "Later, the concepts you have learnt come due again."
        time.sleep(4)
        w.caption = "r reviews them with questions, and the tutor grades your recall."
        time.sleep(1.5)
        w.keys("r", gap=0.3)
        time.sleep(0.5)
        w.idle()
        time.sleep(3)
        w.answer("O(n²): the helper's loop runs n times for each of the n items, so n × n.")
        w.caption = "A good answer pushes the next review further out; a miss brings it back sooner."
        time.sleep(6)
    return w, rec


def scene_setup():
    w = World(configured=False)
    with Recorder(w) as rec:
        time.sleep(2.5)
        for _ in range(3):
            w.keys("Right", gap=1.6)
        w.keys("Enter", gap=2.2)
        w.keys("Enter", gap=2.2)
        w.keys("Enter", gap=2.5)
        w.keys("s", gap=1.5)
        w.keys("1", gap=1.2)
        w.type("sk-ant-api03-demo-key-not-a-real-one", delay=0.02)
        time.sleep(1.2)
        w.keys("Enter", gap=2)
        w.keys("Enter", gap=3)
    return w, rec


def seed_rust(s):
    topic, ids = s.topic("Rust ownership", [
        ("Moves", 4, 5), ("Copy types", 4, 9), ("Borrowing", 2, 1), ("Mutable borrows", 1, 1),
        ("Lifetimes", 0, None), ("Slices", 0, None)],
        notes="Covered moves and Copy types. Thought once that passing &v moves v. "
              "Next: borrowing rules, then mutable borrows.", days_ago=6)
    s.event(ids["Moves"], 2, 4, "two clean predictions, one unprompted", 3)
    s.event(ids["Borrowing"], 1, 2, "followed the borrow example", 3)
    s.session(topic, "learn", 3, [
        "What happens to `v` after `let w = v;` when v is a Vec?",
        "It moves into w, so using v afterwards is a compile error.",
        "Right. And if v were an i32?",
        "Then it's copied, because i32 is Copy, so both work.",
    ])
    return topic, ids


def scene_learn():
    MODEL.script = [
        say("We left off before borrowing. Predict what this prints, or whether it compiles:\n\n"
            "```rust\nlet v = vec![1, 2, 3];\nlet total = sum(&v);\nprintln!(\"{} {}\", total, v.len());\n```\n\n"
            "`sum` takes a `&Vec<i32>` and returns the total."),
        say("Are you sure? Last time you said passing a vector to a function moves it. "
            "Why doesn't that happen here?"),
        call("update_levels", updates=[
            {"subskill": "Borrowing", "level": 3, "reason": "predicted it and defended it unprompted"}]),
        say("That's a second correct check without help, so borrowing goes up.\n\n"
            "Now `sum` takes `&mut Vec<i32>`, and before the call you keep `let first = &v[0];` "
            "and use it after. What does the compiler say?"),
    ]
    w = World(seed=seed_rust)
    with Recorder(w) as rec:
        time.sleep(2.5)
        w.keys("Enter", gap=0.3)
        time.sleep(0.5)
        w.idle()
        time.sleep(4)
        w.answer("It compiles and prints 6 3. sum only borrows v, so v still owns the vector.")
        time.sleep(3)
        w.answer("Passing v would move it. &v only lends a reference, and the borrow ends when "
                 "sum returns, so v can still be used.")
        time.sleep(5)
    return w, rec


def seed_cards(s):
    rust, rust_ids = seed_rust(s)
    sql, sql_ids = s.topic("SQL joins", [("Inner join", 4, 4), ("Left join", 3, 2), ("Join conditions", 2, 1)])
    for subskill, front, back in [
        ("Inner join", "Which rows does an INNER JOIN keep?", "Only pairs of rows where the join condition matches."),
        ("Left join", "In A LEFT JOIN B, what fills B's columns for a row of A with no match?", "NULL."),
    ]:
        s.card(sql, sql_ids[subskill], front, back)
    for subskill, front, back in [
        ("Moves", "After `let w = v;` with v a Vec, can you still use v?", "No: v moved into w. Using it is a compile error."),
        ("Copy types", "Why can you use x after `let y = x;` when x is an i32?", "i32 is Copy, so the value is copied, not moved."),
        ("Borrowing", "What does passing &v to a function do to v?", "It lends a reference; v keeps ownership and is usable after the call."),
    ]:
        s.card(rust, rust_ids[subskill], front, back)


def scene_cards():
    w = World(seed=seed_cards)
    with Recorder(w) as rec:
        time.sleep(2.5)
        w.keys("f", gap=2)
        for grade in ("3", "1", "4"):
            w.keys("Space", gap=2.5)
            w.keys(grade, gap=1.5)
        w.keys("Escape", gap=1.5)
        w.keys("e", gap=4)
    return w, rec


def seed_review(s):
    seed_rust(s)
    topic, ids = s.topic("SQL joins", [
        ("Inner join", 4, -0.1), ("Left join", 3, -0.1), ("Join conditions", 2, 3), ("Self joins", 1, 5),
        ("Joining three tables", 0, None)], notes="Solid on inner joins. Mixes up which side a left join keeps.",
        days_ago=12)
    s.event(ids["Left join"], 2, 3, "two correct row counts", 5)
    s.event(ids["Inner join"], 3, 4, "explained it with an edge case", 5)


def scene_review():
    MODEL.script = [
        say("Two concepts are due. First, inner joins: table A has 3 rows and table B has 2, and exactly "
            "one row of A matches one row of B. How many rows does `A INNER JOIN B` return, and why?"),
        say("Correct. Now left joins, same tables: how many rows does `A LEFT JOIN B` return, and what is "
            "in B's columns for the rows of A with no match?"),
        say("Not quite. Whatever happens with the match, a left join keeps every row of which table?"),
        call("record_review", results=[
            {"subskill": "Inner join", "grade": "good", "level": 4, "note": "correct at once"},
            {"subskill": "Left join", "grade": "hard", "level": 2, "note": "needed a narrower question"}]),
        say("Review done. Inner joins come back in a week. Left joins come back tomorrow, at level 2 "
            "until you get one right unprompted."),
    ]
    w = World(seed=seed_review)
    with Recorder(w) as rec:
        time.sleep(2)
        w.keys("j", gap=1.5)
        w.keys("r", gap=0.3)
        time.sleep(0.5)
        w.idle()
        time.sleep(3.5)
        w.answer("One row: an inner join keeps only the pairs that match.")
        time.sleep(3)
        w.answer("Two rows? I don't remember.")
        time.sleep(3)
        w.answer("Every row of A. So 3 rows, and B's columns are NULL for the two without a match.")
        time.sleep(4)
        w.keys("Escape", gap=4.5)
    return w, rec


def seed_history(s):
    seed_review(s)
    rust = s.db.execute("SELECT id FROM topics WHERE title = 'Rust ownership'").fetchone()[0]
    sql = s.db.execute("SELECT id FROM topics WHERE title = 'SQL joins'").fetchone()[0]
    s.session(sql, "review", 1, [
        "Two concepts are due. First, inner joins: how many rows does A INNER JOIN B return when one row matches?",
        "One row: an inner join keeps only the pairs that match.",
        "Correct. Now left joins, same tables: how many rows?",
        "Every row of A, so 3, with NULL in B's columns where nothing matched.",
    ], tokens=(9_800, 900))
    s.session(rust, "learn", 0.2, [
        "We left off before borrowing. Predict what this prints: let v = vec![1, 2, 3]; let total = sum(&v);",
        "It compiles and prints 6 3. sum only borrows v.",
        "Are you sure? Why doesn't passing it move v?",
        "&v only lends a reference, and the borrow ends when sum returns.",
    ], tokens=(24_000, 3_100))


def scene_history():
    w = World(seed=seed_history)
    with Recorder(w) as rec:
        time.sleep(2)
        w.keys("h", gap=2.5)
        w.keys("j", gap=1.5)
        w.keys("Enter", gap=4)
        w.keys("Escape", gap=1.5)
        w.keys("k", gap=1.2)
        w.keys("Enter", gap=4)
    return w, rec


SCENES = {"hero": scene_hero, "setup": scene_setup, "learn": scene_learn, "cards": scene_cards,
          "review": scene_review, "history": scene_history}


def main():
    global MODEL
    names = sys.argv[1:] or list(SCENES)
    run("cargo", "build", "-q", cwd=ROOT)
    OUT.mkdir(parents=True, exist_ok=True)
    MODEL = Model()
    for name in names:
        MODEL.script, MODEL.problems, MODEL.expect_tool_result = [], [], False
        try:
            w, rec = SCENES[name]()
            MODEL.check()
            # The hero's captions need time to be read.
            svg = to_svg(rec.frames, max_idle=6 if name == "hero" else 2.5)
        finally:
            for w in list(WORLDS):
                w.close()
        (OUT / f"{name}.svg").write_text(svg)
        if dump := os.environ.get("DEMO_DUMP"):
            plain = [re.sub(r"\x1b\[[0-9;]*m", "", f) for _, f, _ in rec.frames]
            Path(dump, f"{name}.txt").write_text("\n=====\n".join(plain))
        print(f"{name}: {len(rec.frames)} frames, {len(svg) // 1024} KB")


if __name__ == "__main__":
    main()
