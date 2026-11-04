# Built-in tools

The daemon enables these tools by default. `--no-tools` disables them;
`--raw` does not register tools. Library users opt in with
`AssistantRuntime::builder(...).tool(...)` or a plugin's registrar. There is no
new HTTP endpoint: web chat and terminal diagnostics use the same tool pipeline.

| Tool | Capability | Platform |
| --- | --- | --- |
| `calculate` | Checked integer addition, subtraction, multiplication and division | All |
| `current_time` | Local date, time and timezone from the fixed host `date` command | Requires `date` |
| `process_top` | Live process rankings with CPU, RSS, virtual memory and name filters | Linux procfs |
| `process_details` | Live process metadata by process name or PID | Linux procfs |

## Process listings

`process_top` accepts an object with these optional fields:

| Argument | Meaning | Default |
| --- | --- | --- |
| `sort_by` | `cpu`, `rss`, `virt`, or `name` | `cpu` |
| `order` | `desc` or `asc`; PID ascending breaks ties | `desc` |
| `name` | Case-insensitive literal process-name substring | No filter |
| `min_cpu_percent` | Inclusive minimum CPU percentage; must be finite and nonnegative | No filter |
| `min_rss_bytes` | Inclusive minimum resident-memory estimate, in bytes | No filter |
| `min_virtual_bytes` | Inclusive minimum virtual address space, in bytes | No filter |
| `limit` | 1–25 rows | 10 |

Filters combine with AND before sorting and limiting. Examples of tool arguments:

```json
{"sort_by":"rss","limit":5}
```

```json
{"name":"firefox","min_cpu_percent":10,"sort_by":"cpu","limit":10}
```

Responses contain `processes`, readable `observed_processes`,
`unavailable_processes`, `matched_processes` before the result limit, and a
`truncated` flag. Missing CPU measurements sort last in either direction and do
not satisfy a CPU threshold. Empty matches are a successful empty result.

## Process lookup

For “tell me about the htop process,” the model calls `process_details` with:

```json
{"query":"htop","limit":5}
```

`query` is required. Decimal queries select one positive PID; other queries match
kernel process names, ignoring case. Exact matches rank before prefixes, then
substrings; PID ascending breaks ties. Names may be shortened by the kernel, so
use a PID for an exact lookup. Multiple instances are returned rather than
silently choosing one. `limit` defaults to 5 and accepts 1–10.

The model interprets the natural-language request and chooses the name to search.
The tool performs deterministic live name matching; it is not an embedding index
or a search of installed applications. No matching live process does not establish
that the program is uninstalled.

Details include PID and parent PID, name, state, effective UID, executable path,
thread count, raw scheduling priority, nice value, total CPU seconds and process
age, along with the listing's current CPU/memory fields. An inaccessible executable
is `null`. Paths are bounded to 256 UTF-8 bytes with `executable_truncated` marking
shortening. Queries are bounded to 128 UTF-8 bytes and reject control characters.

## Measurement and execution contract

- CPU uses two process snapshots separated by at least 250 ms, rather than a
  lifetime average. Each row reports its actual `cpu_interval_ms`. 100% means one
  CPU core; a multithreaded process can exceed 100%.
- Matching PID and `start_time_ticks` are required before comparing counters.
  A process without a comparable baseline has `cpu_percent: null`, not zero.
- `rss_bytes` is the inexpensive kernel RSS estimate from `/proc/PID/stat`,
  including shared resident pages. It is not PSS or an exact private-memory cost.
  `virtual_bytes` is virtual address space, not physically occupied RAM.
- Observations are not an atomic system snapshot. Process exit, access denial,
  malformed records and identity changes contribute to `unavailable_processes`.
  Visibility follows the daemon's PID namespace and OS permissions.
- Filesystem reads, filtering and sorting run on blocking workers. Cancellation
  interrupts the sample wait and is checked between bounded process reads.
  There is no background polling between tool calls.
- Scans allow at most 16,384 process entries and 17,408 directory entries overall;
  exceeding either bound returns an error. Each stat/status read is capped at
  8 KiB. Results fit the default 16 KiB tool payload budget even with JSON escaping.
- Process tools read OS metadata directly. They execute no shell commands and do
  not read command arguments, environment variables or open files, or send signals.
  Model text never becomes a path to inspect outside the fixed procfs provider.

These tools are registered by the daemon only on Linux. Explicit library calls on
other platforms return an unsupported error. They are trusted host capabilities;
process names and tool results remain untrusted model input.

Kernel sources: [process stat fields](https://www.man7.org/linux/man-pages/man5/proc_pid_stat.5.html),
[RSS limitations](https://www.man7.org/linux/man-pages/man5/proc_pid_statm.5.html),
and [procfs identity and visibility](https://www.kernel.org/doc/html/latest/filesystems/proc.html).
