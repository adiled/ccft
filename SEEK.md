# SEEK — open research questions

A living list of questions we don't have answers to yet, collected from the
driver-vs-bot interview. Each entry records the question, what we know so far,
and what's open. Keep adding as we learn.

---

## Q5 — What arrives on the request side that is NOT the driver?

> On the request side, what kinds of things arrive that are NOT the driver —
> and which of them is the hardest to tell apart from a real driving message?

**Framing (Q5 context):** "bot vs driver", not "AI vs human". The response
side is bot *today* because providers are hosted LLMs, but a future provider
could be a human labor swarm behind the API — so we can never assume
"assistant output = machine". Our axis is *loop machinery* vs *the thing
driving the loop*, not machine vs human.

**What we know so far (request-side non-driver content):**
- tool results (captured as `tr_ch`)
- context md files (CLAUDE.md / context files) — arrival shape unknown
- summarizations / continuations when a session runs out of context limit
  (partially handled in `clean_user_text`)

**Status:** OPEN — a difficult undertaking, not fully mapped.

---

## Q6 — How do context md files arrive on the wire?

> How do context md files actually arrive — as what `role` and what content
> type? If we know the exact shape, we can catch them like tool results.

**Working guess:** they arrive either as `system` role/type OR as `user`,
and it likely varies across harnesses.

**Status:** OPEN.

---

## Q11 — Resend-all (full-conversation) leakage: what is the "delta"?

> In a resend-all API, every request resends the whole history as an array of
> message parts. If we attribute the "last user message" per request, stale
> content is re-counted every call. What is the genuinely-new slice?

**Design (layered, most-precise-first):**
1. **Message-id cursor** — if parts carry stable ids, extract everything after
   the last seen id. Exact.
2. **Size-increment** — for sessions we can attribute to a session (we have
   `sid`): if the array strictly grew, the new turn was appended; that request
   is worth only the LAST user turn extraction. Untrustworthy when a harness
   prunes/summarizes history (array shrinks/stays) — fall through.
3. **Text-block hash** — robust fallback: per-message filter over ONLY the
   stable text core, because harnesses prune volatile fields
   (reasoning_content, tool_calls, image blocks) but rarely the actual text.

**What we know:**
- This OpenAI-compatible wire carries **no message ids** (`id=None` on all 129
  parts) and **no session attribution** (no headers/metadata → `sid=None`).
- With no session attribution, every request is FirstContact: attribute ONLY
  the last user turn — never over-count a resumed backlog. That is the safe
  no-leakage fallback; `tr_ch`/`th_ch` are 0 on such wires (trade-off).
- Ledger persists only `sid` — there is NO per-session message cursor in the
  record; delta state is in-memory only.

**Status:** IMPLEMENTED in `src/handler.rs` (`SessionDelta` / `DeltaMode` /
`text_fingerprint` / `extract_request_delta`). Live: `mode=FirstContact
msgs=141 new_flags=1/141` in dev log.

---

## Q12 — Do we keep a per-session message reference in the ledger?

> Asked: "do we not keep a reference of that in ledger?" — i.e. a cursor for
> the last-seen message so resumed sessions don't re-count a backlog.

**Answer:** No — the ledger `Record` persists only `sid`; there is no message
cursor. FirstContact (last-turn-only on first contact) already avoids
over-counting a resumed backlog without persistence. A persisted cursor would
only help the message-id strategy across restarts (unavailable here — no ids).
