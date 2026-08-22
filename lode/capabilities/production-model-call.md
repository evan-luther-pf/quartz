# Production model call

## Problem

Slices 0 through 5 can reconstruct a bounded agent turn and inspect admitted repository bytes, but the provider is deterministic. A production model call requires credential-bearing network authority, bounded byte exchange, durable ambiguity handling, and a response path that does not give sandboxed components ambient network, environment, or secret access.

## Observable behavior

A sandboxed client submits one host-admitted UTF-8 prompt through `quartz.agent/submit@1` and commits it as a bounded durable payload. A production provider component reads only committed turn facts, derives the stable invocation identity, and invokes one exact host-installed exchange adapter during its callable dispatch. The adapter starts one OpenAI Responses API request with `background: true` and `store: false`, then polls that response identity to a terminal state without exposing its API key, endpoint, or HTTP client to guest code. The provider returns a usage scalar; the host transfers the staged normalized text only through the caller's committed provider view, and the agent loop commits it as a durable assistant payload followed by usage and one terminal stop. Fresh processes reconstruct every boundary.

## Non-goals

- Streaming output or partial-token persistence.
- Model-selected tools, multi-turn continuation, prompt templates, or provider registries.
- Automatic retries, retry backoff, failover providers, or inferred idempotency.
- Zero Data Retention guarantees.
- Repository mutation, TUI work, package transport, or kernel handover.

## Invariants

- The production provider and agent loop remain WebAssembly components. The kernel knows only an indexed bounded exchange capability and opaque request and response bytes.
- One runtime receives at most one explicit host exchange adapter. A component can use it only through a matching `ExchangeGrant`; no global adapter registry exists.
- API credentials, endpoint configuration, and HTTP response objects remain host-side. Guest code receives only a status or usage scalar and normalized response bytes.
- The request is the exact payload of one committed event visible through the caller's committed event-stream provider. Missing, ungranted, non-UTF-8, or oversized input is rejected before network emission.
- A synchronized started record without a successful terminal result is never retried. Terminal failures retain one bounded non-secret category: `authentication`, `request-rejected`, `remote-failed`, `empty-response`, `response-limit`, `protocol`, or `ambiguous`. A started-only record is durably closed as `ambiguous` during replay before the component observes it.
- Reusing an invocation identity with a different request digest fails closed.
- OpenAI generation is capped at 1,024 output tokens; response bytes, usage, provenance, and digest are independently bounded and synchronized in the exchange ledger before they can enter the event outbox.
- The host supplies the deadline to the adapter and independently stops waiting at that deadline. The adapter bounds create and polling work by the same deadline. Timeout is durably terminal and ambiguous; Quartz does not claim remote cancellation or safe retry.
- Callable dispatch releases the core borrow and marks exactly one provider in flight. That provider may call only its declared `event-count`, `read-event`, and `exchange` imports; concurrent or nested provider invocation fails closed. At most one adapter worker may remain outstanding, and a new invocation cannot emit while it is running.
- Component recovery closes the exchange ledger, clears staged response authority, and joins any timed-out adapter worker before reporting a clean context. Durable ledger records and API emissions remain external.

## Public contract

ABI 7 adds three host capabilities:

- `open-exchange(index) -> status` registers one component-owned, host-admitted `ExchangeGrant` and its durable ledger as a reversible effect;
- `exchange(event-index, invocation) -> s64` runs or reconstructs one bounded exchange during the invoked provider's callable dispatch and returns usage, or a negative Quartz status;
- `resume-exchange(event-grant-index, value) -> status` queues the response transferred from that provider as one durable event payload through the existing transactional outbox.

An `ExchangeGrant` binds an adapter identity, ledger path, request and response byte limits, and timeout. `Runtime::new_with_exchange` and `Runtime::open_persistent_with_exchange` install the one explicit host adapter. The adapter contract accepts opaque UTF-8 request bytes and a deadline and returns bounded normalized bytes, provenance, and non-negative usage; failures use the seven terminal categories above. The runtime stops waiting at the deadline and tracks a still-running worker until exchange recovery joins it.

Exchange ledger schema 2 uses the eight-byte magic `QUARTZX2`. Failed records
contain only the fixed category; request identity remains a SHA-256 digest.

The production provider implements the existing `quartz.agent/provider@1` callable, so deterministic and production providers follow the same dependency and replacement lifecycle. Its facts retain the existing bit layout and use the existing user-prompt, provider-request, assistant-message, usage, stop, and interrupted/unknown kinds. The user-prompt and assistant-message facts carry durable payloads. The assistant scalar projection stores usage so the following activation can commit the separate usage fact without transient state.

## External-effect classification

- Prompt and response event facts are withheld until composition-journal commit and delivered idempotently by the existing event outbox.
- The OpenAI adapter uses background mode with `store: false`. OpenAI temporarily stores background response data to support asynchronous execution and polling; this remote retention is outside Quartz's recovery boundary.
- Exchange ledger records are irreversible durable facts outside the recovered context.
- A model request is an irreversible external emission. Quartz synchronizes intent before emission, reuses a synchronized success, and reconstructs a synchronized terminal category without another request. A started-only replay is durably classified `ambiguous`; Quartz does not claim to undo or safely retry the request.
- A host deadline is ambiguity handling, not inversion: it bounds Quartz's wait but cannot erase consumed compute, billing, data already transmitted, or a remote operation that completes later.

## Acceptance scenario

1. Start with one admitted prompt snapshot, the public submit gateway, production client, production provider, production loop, event storage, and one explicit exchange adapter.
2. Commit the prompt payload through the public gateway, then terminate the process.
3. A fresh process commits the stable provider request and terminates.
4. A fresh loop activation invokes the provider callable; the provider synchronizes exchange intent, starts one background Responses API request, polls its identity to completion under the host deadline, synchronizes normalized response text and usage, and the loop commits one assistant payload.
5. Fresh processes commit usage and one stop. Restart reconstructs the exact response bytes, usage, invocation identity, and terminal state without a second API request.
6. Replacing the provider withdraws the old exchange registration before the new provider opens the same ledger; committed history does not change.
7. Removing the application and persistence roots recovers all live exchange, event, snapshot, callable, and component authority while durable records remain external.

## Verification gate

Slice 6 closes only when focused contracts cover successful response reconstruction, missing adapter or grant denial, request and response bounds, host-enforced timeout, started-only ambiguity, invocation collision, cached-success replay, provider replacement, and complete authority recovery; the workspace checks pass; the release executable completes the deterministic exchange path; and the exact credentialed smoke command is documented. A live OpenAI run additionally requires `OPENAI_API_KEY` and is reported separately from deterministic contract proof.
