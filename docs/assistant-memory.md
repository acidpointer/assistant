# Using the personal assistant

The Linux daemon supports durable semantic memory, arbitrary freeform personality,
and optional assistant-initiated messages. WebUI is the first client. Memory and
behavior live in the core; a future socket or D-Bus adapter can use the same
`application::OwnerHandle` without depending on HTTP or React.

## Start

Use the existing local chat model and selected multilingual E5 small Q8 artifact.
There are no downloads or additional model requirements. Startup uses CLI flags:

```sh
assistant --model /path/to/chat.gguf \
  --memory-dir "$HOME/.local/share/assistant/memory" \
  --embedding-model /path/to/multilingual-e5-small-q8_0.gguf \
  --port 3333
```

Supply `--memory-dir` and `--embedding-model` together. Relative paths resolve
from the working directory, just like `--model`. The daemon creates a missing
private memory directory. Point `--memory-dir` at existing memory to reuse it;
no configuration file is required. Keep private data outside the checkout.

Open the printed loopback WebUI URL and start chatting. There is no pairing,
login, token or session expiration. Reloading reconnects directly and restores
saved conversation state. Omitting both memory flags keeps temporary-chat mode.

## Personality and initiative

Open **Assistant** to describe any desired personality, role, tone, interests or
preferences in ordinary text. The core pins these instructions on subsequent
runs, even when old working context is evicted. The conversation tool
`assistant.configure` also supports explicit requests to change personality or
initiative. Configuration changes use version checks, so concurrent clients do
not silently overwrite each other.

Enable **Let my assistant start a conversation** and choose a check interval.
Initiative is disabled by default. Intervals are 1 minute to 24 hours. A due
check uses the same resident chat model, personality, relevant source quotations
and connected memory; it may choose to remain silent. Foreground work cancels
background decisions. One decision has a 120-second deadline, at most 4,096 input
and 128 output tokens. Runtime context capacity still applies.

A generated message appears in the conversation without a user submission. It
is saved in the core mailbox and reappears after reconnect/restart until
**Dismiss**. Dismissal hides the message; it does not erase its memory source.
Recent undismissed messages provide context for short replies such as “yes.”
The schedule is advanced durably before inference, preventing immediate replay
after a crash. A failed decision records a sanitized status and waits until the
next scheduled consideration. Changing personality invalidates an older in-flight
proposal. Initiative proposes dialogue only; it cannot execute unrelated tools.

## Memory

Literal conversations are saved before and after inference. There is no
supported-topic list: a person, a private preference, a roleplay request, a Rust
project and a later correction can coexist in any order. The selected E5 model
indexes literal UTF-8 windows. Lexical and semantic retrieval locate entry
points; source links connect related experiences and assistant notes.

Open **Memory** to save a standalone note, search, inspect speaker/time/literal
text, preserve a source,
browse saved history, and **Explore connections**. Saving an explicit note commits
user-attributed memory without a model run or a new chat exchange. Its observation
time is host supplied; historical imports with unknown dates remain unknown. Continue from another source
to walk beyond the current neighborhood. **More connections** pages a busy
source's remaining links; memory.follow exposes the same cursor to the model. The old typed-claim explorer remains
under “Structured claims from earlier experiments.” Specialized family/job
extraction is isolated behind the optional `experimental-extraction` feature;
it is not a production dependency. The runtime retires its pending
experimental jobs while retaining existing sources and claims.

The assistant uses the ordinary typed tool loop:

- `memory.search`: a natural-language question returns bounded attributed source
  excerpts and identities.
- `memory.follow`: traverse links from an identity in either direction; repeat
  from returned identities to explore further.
- `memory.remember`: save a concise freeform assistant note, automatically linked
  to its current input and optionally to related sources.
- `assistant.configure`: apply an explicit conversational request to change
  persistent instructions or the initiative interval.

A note remains an assistant interpretation. Link direction and source role/time
are retained; similarity and graph connectivity do not establish truth. The
host issues run-scoped authority; JSON arguments cannot create capabilities or
execute code. Evidence forgetting suppresses dependent notes before physical
purge. Profile instructions edited independently are separate owner settings.

## Verification and limits

Default tests use temporary private stores and deterministic model doubles for
transactions, graph traversal, correction, forgetting, restart, authority,
personality and initiative scheduling/delivery. No weights are required. The
ignored model-quality extraction experiment is historical development evidence,
not the product release criterion. See [ADR 0007](decisions/0007-universal-assistant-memory.md).

For a real end-to-end local-model check, build the frontend and daemon once, then
reuse the binary:

```sh
python3 scripts/check-memory-web.py \
  --binary target/debug/assistant \
  --model /path/to/chat.gguf \
  --embedding /path/to/multilingual-e5-small-q8_0.gguf \
  --browser --initiative
```

The script owns synthetic stores and two daemon processes; it verifies direct local access,
saved chat, source connections, personality, browser use and an actual initiative
decision. A standalone satin-finish note never enters user chat history; the
fresh process must retrieve it to answer the bench question. It performs no build/download. The
initiative option adds at least 60 seconds of wall time. On the four-thread CPU
reference, prompt prefills took roughly 55–127 seconds in development; multi-turn
requests can take minutes. GPU latency was not measured. This is distinct from
the fixed native-build reuse issue. No browser credentials or pairing socket are involved. Optional `--work-directory` retains a
marked synthetic store and reuses committed request keys on retry; it refuses an
existing non-smoke directory. Resumed initiative checks require a new message ID.
The [journey evidence](benchmarks/memory-journey-2026-11-06.json) records the checked
artifacts and coverage limits.

Model-selected notes, retrieval queries, configuration and outreach remain
nondeterministic. Host bounds and provenance are deterministic; no finite test
suite proves correct handling of every possible conversation. Whole-memory
restore and end-to-end forgetting controls are not yet exposed in WebUI.
No D-Bus/socket transport has been implemented. Native calls cannot be preempted
inside a single evaluation step. Cross-platform native cache reuse and GPU
acceptance require their own checks.
