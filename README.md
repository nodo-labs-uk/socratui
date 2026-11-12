<h1 align="center"><img src="docs/media/banner.svg" width="640" alt="Socratik: a Socratic tutor in your terminal"></h1>

<p align="center"><b>A tutor in your terminal that teaches by asking, and remembers what you know.</b><br>
A skill map that persists, spaced review by questioning, and flashcards you can take to Anki.<br>
<sub>Linux · Rust · Claude, GPT, open or local models · MIT</sub></p>

Tell Socratik what you want to learn. The tutor splits the topic into subskills, asks a few questions to find your level, then teaches by asking: one question at a time, a narrower question when you get it wrong, and a push-back when you get it right. Each subskill has a level from 0 to 5 that you earn on evidence. What you work through becomes flashcards, and the concepts you've learnt come back for review on a spaced schedule, as questions rather than cards. Everything is kept between sessions, so the next one picks up where you left off.

![A new topic from start to finish: the diagnostic, a wrong answer met with a narrower question, levels set, flashcards made and reviewed, then a concept review a day later](docs/media/hero.svg)

> [!TIP]
> **Quickest install: paste this into Claude Code and let it do the setup.**
>
> ```text
> Set up Socratik (https://github.com/nodo-labs-uk/socratik) for me.
>
> 1. Install it: download socratik-x86_64-linux.tar.gz from the latest release
>    (https://github.com/nodo-labs-uk/socratik/releases/latest), or clone the
>    repo and run `cargo install --path .`.
> 2. Make sure `socratik` is on my PATH.
> 3. Tell me to run `socratik` in a terminal. On first start it asks which
>    model to use, unless ANTHROPIC_API_KEY, OPENAI_API_KEY or HF_TOKEN is set.
>
> Don't change anything else.
> ```

**Contents:** [Design principles](#design-principles) · [Learning](#learning) · [Flashcards](#flashcards) · [Concept review](#concept-review) · [History](#history) · [Models](#models) · [Keys](#keys) · [Compared with other tools](#compared-with-other-tools) · [Install](#install) · [Files](#files) · [Development](#development) · [Licence](#licence)

## Design principles

- **Asking, not telling.** The tutor asks one question and waits. A wrong answer gets a narrower question that shows the gap in your reasoning; it explains directly only after two or three tries, then tests the same idea from another angle. A right answer sometimes gets "are you sure?", so you learn to defend it.
- **Levels are earned.** Each subskill goes from 0 (unaware) to 5 (can teach it). A level goes up only after two independent correct checks, one of them unprompted, and level 5 needs an edge case handled and a clear explanation. Every change is logged with the reason.
- **It remembers you.** Topics, levels, sessions and the tutor's own notes live in one SQLite file. A new session starts with the skill map, the notes and the end of the last session, so the tutor knows where you were without resending every old conversation.
- **Review the ideas, not just the cards.** Each concept you have learnt comes due on a spaced schedule. A review is a short Socratic session on the due concepts: the tutor grades your recall, which sets the next due date, and lowers the level if you've lost it.
- **Flashcards from your own work.** The tutor makes cards from what you have worked through, never from material you haven't met. Review them in the app, or export them to Anki.
- **Your choice of model.** Claude, OpenAI, open models through Hugging Face, or a model on your own machine through llama.cpp.
- **Keyboard first.** Each screen lists its keys along the bottom.

## Learning

`n` starts a topic: type what you want to learn and choose the aim, level 4 (can do it independently) or level 5 (can teach it). The tutor builds the skill map, runs a short diagnostic, and proposes where to start. `Enter` on a topic continues it, with the tutor picking up from its notes. The skill map beside the chat changes as levels do.

![Continuing a topic: the tutor recalls a past mistake, asks for a prediction about some code, pushes back on the right answer, and raises the level when it holds](docs/media/learn.svg)

You can say "show progress", "harder", "easier" or "just tell me" at any time. The course ends when every subskill reaches the aim, after a final mixed assessment.

## Flashcards

The tutor saves a card when you have worked through a fact worth memorising, and `Ctrl+F` asks for cards on what you have covered so far. `f` on the home screen reviews every card that is due, across topics. `Space` shows the answer and `1`-`4` grade it (again, hard, good, easy), with the next interval shown on each button. A card you get wrong comes round again before the review ends.

`e` writes every card to `anki-export.txt` in Anki's text format, with a deck per topic (`Socratik::<topic>`) and tags for the topic and subskill. In Anki, use File > Import.

![Reviewing due flashcards with good, again and easy, then exporting them for Anki](docs/media/cards.svg)

## Concept review

A subskill comes due for review the day after it first reaches level 1, and then on a growing interval. The home screen counts what is due per topic, and `r` reviews that topic's due concepts. The tutor asks one or two recall questions per concept, with no hints, then records a grade: a good answer pushes the next review further out, a miss brings it back sooner and can lower the level.

![Reviewing two due SQL concepts: one answered at once, one only after a narrower question, so its level drops and it comes back tomorrow](docs/media/review.svg)

Cards and concepts share one scheduler, in the style of SM-2: after a first good answer the next review is in 1 day, then 3, then the interval times an ease factor that starts at 2.5. A missed card comes back in 10 minutes, a missed concept the next day.

## History

The home screen shows each topic's progress, its recent level changes with the reasons, and the tutor's notes. `h` lists every session with its token count, and `Enter` opens one to read.

![The session history, opening a past review and a past lesson](docs/media/history.svg)

## Models

On first start, Socratik uses the first of `ANTHROPIC_API_KEY`, `OPENAI_API_KEY` or `HF_TOKEN` it finds. With none set, it asks: pick a provider, then paste the key or give the server's address, then confirm the model. `s` opens the same dialog later. The key is saved in a file only you can read.

![First start with no key set: choosing a local model, then switching to Anthropic with a key](docs/media/setup.svg)

| Provider | Asks for | Default model |
|---|---|---|
| Anthropic | API key | `claude-fable-5-2` |
| OpenAI | API key | `gpt-5.6-sol` |
| Hugging Face | token | `openai/gpt-oss-120b` |
| Local model | server address, `http://localhost:8080/v1` by default | whatever the server loaded |

The tutor changes the skill map, cards and notes through tool calls, so the model must support them. For llama.cpp, start the server with `llama-server -m model.gguf --jinja`. The Local option works with any server that speaks OpenAI's chat completions API without a key, such as Ollama. Small models follow the method less reliably than large ones.

Your conversations go to the provider you choose; with a local model they stay on your machine. The token count for each session is under the chat. With Anthropic, earlier turns are read from the prompt cache, which costs less than sending them again.

## Keys

| Key | Where | What it does |
|---|---|---|
| `n` | home | new topic |
| `Enter` | home | continue the selected topic |
| `r` | home | review the topic's due concepts |
| `f` | home | review the flashcards that are due |
| `h` | home | session history |
| `e` | home | export every flashcard for Anki |
| `s` | home | choose the model |
| `d` | home | delete the topic, with its sessions and cards |
| `Enter` | chat | send your answer |
| `Alt+Enter`, `Ctrl+J` | chat | new line |
| `Ctrl+F` | chat | make flashcards for what you have covered |
| `Ctrl+R` | chat | retry after an error |
| `↑` `↓` `PgUp` `PgDn` | chat | scroll |
| `Space` | flashcards | show the answer |
| `1`-`4` | flashcards | again, hard, good, easy |
| `d` | flashcards | delete the card |
| `Esc` | anywhere | back |

Lists move with `j` `k` or the arrows. `q` quits from the home screen.

## Compared with other tools

| | Socratik | A tutor prompt in a chat | [Anki](https://apps.ankiweb.net) |
|---|---|---|---|
| Teaches by asking questions | ✓ | ✓ | – |
| Skill map with levels | ✓ kept between sessions | for one conversation | – |
| Reviews concepts on a schedule | ✓ by questioning | – | – |
| Flashcards | ✓ made by the tutor | – | ✓ you write them |
| Spaced repetition of cards | ✓ SM-2 style | – | ✓ SM-2 or FSRS |
| Runs in | terminal | chat app | desktop, mobile, web |

Anki is the better place to keep a large deck for years, which is why Socratik exports to it.

## Install

The prompt at the top of this page does this for you. By hand: take the binary from the [latest release](https://github.com/nodo-labs-uk/socratik/releases/latest):

```sh
curl -fsSL https://github.com/nodo-labs-uk/socratik/releases/latest/download/socratik-x86_64-linux.tar.gz | tar xz
install socratik-x86_64-linux/socratik ~/.local/bin/
```

or build it from source with `cargo install --path .`. Then run `socratik`.

## Files

Everything is in `~/.local/share/socratik/`, or in `$SOCRATIK_DATA_DIR` if set:

- `socratik.db`: topics, subskills and levels, every level change, sessions and their messages, and flashcards with their review history.
- `config.json`: the provider, its address, the key and the model, readable only by you.
- `anki-export.txt`: written by `e`.

`SOCRATIK_MODEL` overrides the saved model name for one run.

## Development

`cargo test` runs the tests. `demo/record.py` re-records the videos: it runs the app in a private tmux server, with a stand-in model server that answers from a script, so it needs no API key and costs nothing. Pushing a `v*` tag builds a release.

## Licence

[MIT](LICENSE): use it, change it and share it, for anything.

---

Socratik was built with [Claude Code](https://claude.com/claude-code), from a Claude Code skill for Socratic tutoring. Issues, PRs and other contributions are welcome.
