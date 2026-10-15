# Recorded upstream traffic

Byte-for-byte responses from real providers, replayed through the gateway by
`gateway::tests::recorded_*`. No keys or prompts beyond the fixed test prompts
are stored.

| File | Source | Recorded | Request |
|---|---|---|---|
| `ollama-chat-text.sse` | Ollama 0.x, `qwen3:4b-instruct-2507-q4_K_M`, `/v1/chat/completions` | 2026-10-14 | stream, "Reply with exactly: PRISMFLOW_OK" |
| `ollama-chat-tool.sse` | same | 2026-10-14 | stream, one `read_file(path)` tool offered |
| `ollama-chat-text.json` | same | 2026-10-14 | non-stream, same prompt |

Notable real-world shape: Ollama sends a whole tool call (id, name, full
arguments) in one chunk, with `content: ""` beside it.

Add a provider: record with `curl -sN` against its endpoint using your own
key, strip nothing but the `Authorization` header (never stored), and add a
row here.
