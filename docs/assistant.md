# The AI assistant

The right panel has an **AI** tab beside Files, Git, Ports and Automations. You
ask a question in plain language — *what is running right now?*, *why is the
nightly red?*, *commit the staged files* — and the answer comes from a snapshot
of what those tabs already hold. Each tab of the strip keeps its own
conversation: switch tabs, and the panel switches threads; close a tab, and its
thread is gone.

## What the assistant is shown

At the moment you send a question, the panel takes one look at the live state
and puts it in front of the model, section by section, each named for the tab
it came from:

| Section | From the tab | What it says |
| --- | --- | --- |
| Session | the shell itself | hosts, tmux workspaces and windows, the panes of the window on screen with their directories and running commands, and every coding agent with its state |
| Files | Files | the Explorer's root and the documents you have open |
| Git | Git | the repository, the branch, and every changed file with its status |
| Ports | Ports | active forwards and, on an SSH host, what is listening |
| Automations | Automations | each automation, its schedule, and how its last run ended |

The checkboxes at the top of the panel choose which sections go along; a
question can be about everything or only about Git. The model is told to answer
from the snapshot and its own tool results, and to say when a tab has nothing,
rather than invent what it might hold. The snapshot itself never carries file
contents or terminal text — the assistant sees screen text only when it asks,
with the `read_pane` tool below.

## What the assistant can do

It acts on the current tab through four tools, and nothing else:

- **send_to_pane** types text into a pane, optionally pressing Enter.
  **Every use waits for you**: the panel shows the exact text, the pane, and
  whether Enter will be pressed, and nothing is typed until you press Allow.
  When the text is too long for one line the card ends in **…** — click it to
  read the whole command before deciding.
- **run_automation** starts a scheduled automation outside its schedule.
  This too waits for Allow.
- **read_pane** reads a pane's screen as plain text — what the user actually
  sees, plus the scrollback the terminal holds. It reads the buffer the app is
  already rendering, not a fresh capture from the host, and it is read-only, so
  it does not ask.
- **list_panes** reads the current tab's panes as they are *now* — the
  assistant is told to call it after acting, since the question's snapshot goes
  stale the moment it types anything.
- **show_pane** brings a pane on screen. It changes nothing, so it does not ask.

Denied actions stay denied — the model is told plainly and is instructed not to
retry. Every action shows in the transcript as a small chip
(*type “git status” into the active pane and press Enter — done*), and the
model sees its own past actions in the history, so it cannot quietly pretend a
denial happened. Threads and their transcripts live in memory only; nothing is
written to disk, and nothing goes anywhere except to the endpoint you configure.

## Configuring it

Settings → **Assistant** takes three values:

- **Endpoint** — the base URL of an [OpenAI-compatible](https://github.com/openai/openai-python/blob/main/api.md)
  chat server, usually ending in `/v1`. That single shape covers Ollama, LM
  Studio, vLLM, llama.cpp's server, an MLX server, and the hosted APIs.
- **Model** — the model name the server expects, e.g. `qwen2.5-coder:32b`.
- **API key** — only hosted providers need one. It is sent to the endpoint
  above as a bearer token and nowhere else, and it is stored in the app's own
  state file, which is written so only your user can read it. If that trade is
  wrong for your machine, point the endpoint at a local server instead — a
  local server needs no key.

The request leaves from the app's native side, not from the webview: the same
closed content policy that keeps rendered files from phoning home also means
the webview cannot reach the network itself. Nothing about the question or the
snapshot ever travels through a connected host — Muxflow's own SSH lanes are
not involved.

## Limits

- One generation at a time per thread; **Stop** ends it, and the partial answer
  stays visible. A question buys the model six rounds that ask to *do*
  something — type in a pane, run an automation; rounds that only read the
  screen are free. When the six run out without an answer the panel asks —
  **Continue** spends six more on the same run, Stop ends it.
- A finished answer shows its token count beside the timestamp when the
  server reports one. Servers that stay silent about usage show no count,
  which is not the same as zero.
- The snapshot is bounded — sixty git files, twenty automations, and so on —
  so a huge repository arrives as a count plus the first part, not as a
  timeout. Tool arguments are bounded too: the assistant cannot type more than
  a screen of text in one call.
- Tools need a streaming endpoint. A server that ignores `stream:true` answers
  questions but cannot ask for tools, and says so.
- Answers are as good as the model you point at. Small local models answer
  small questions well and wander on big ones — and a wandering model that
  asks to type something is still stopped at the approval card.
