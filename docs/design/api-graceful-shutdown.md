# API graceful shutdown

> **Status: ⏳ planned — not started.** Checked 2026-09-13: nothing in
> `homescope-api` listens for SIGTERM or SIGINT. Written 2026-07-10 against an
> ingest-only API, and updated for the API as it stands now: an MQTT ingest half
> *and* an HTTP half, which changes the sketch.

## The problem

The API runs as a supervised service on the Pi (a podman quadlet under
systemd). Stopping a service means **SIGTERM**, then SIGKILL after
`TimeoutStopSec` (90 s by default). In dev, Ctrl-C sends SIGINT. Either way, the
default for a Rust process is **immediate termination** — tokio does not
intercept signals for you, no destructors run for in-flight work, and tasks are
killed mid-`.await`.

What every restart or deploy can lose:

1. **Queued envelopes.** Up to 256 `ObservationEnvelope`s waiting in the ingest
   channel. rumqttc has already acked them to the broker — it acks QoS 1 when a
   publish is polled, not when it is stored (see
   [ingest-db-error-handling.md](ingest-db-error-handling.md)) — so the broker
   will not redeliver them. They are gone. At production cadence that is
   usually zero to a few readings; at benchmarking cadence, dozens.
2. **The in-flight insert.** No corruption risk — a single Postgres `INSERT` is
   atomic — just possibly one lost row.
3. **In-flight HTTP requests.** A `POST /devices` or `rotate-key` cut off
   mid-request. The mint is one database statement, so the registry is never
   half-written — but if it commits and the process dies before the response
   leaves, `homescope-provision` sees a transport error on the mint step and
   cannot tell it from "nothing happened". The registry holds a new key the
   board never received, so the device is dark until rotated again. That
   ambiguity is the strongest argument for this work.
4. **A clean MQTT disconnect.** The broker sees an abrupt socket close instead of
   a `DISCONNECT`. The API's session is durable (`clean_session=false`, client id
   `api`), so the broker keeps queueing for it across the restart regardless;
   what a clean disconnect adds is quieter broker logs and, once manual acks
   land, a well-defined boundary for what was processed.

## What the current structure gives — and what it throws away

`main.rs` races two halves in a `tokio::select!`: `ingest::run` and
`http::serve`. Inside, `ingest::run` races the producer `subscribe_mqtt` against
the writer `store_envelopes`, joined by `channel::<ObservationEnvelope>(256)`.

The drain mechanism exists in principle, and falls out of mpsc close
semantics. When every `Sender` is dropped, `Receiver::recv()` keeps returning
buffered messages until the queue is empty, and only then returns `None`. The
writer's `while let Some(envelope) = … .recv().await` loop would therefore
finish the backlog and exit on its own.

⚠️ **The current `select!` structure throws that away.** The producer and the
writer are *sibling branches of the same `select!`*, and `ingest::run` as a
whole is a branch of `main`'s. Cancelling any of them drops both futures at
once — the writer is dropped mid-backlog, not drained. And `store_envelopes`
treats a closed channel as an error (`bail!("ingestion channel closed")`),
which is right today, because the producer ending means something broke, and is
exactly the normal end of a graceful shutdown.

So this is a small restructure, not just a signal handler:

1. Listen for SIGTERM and SIGINT.
2. Stop ingress: cancel `subscribe_mqtt` (dropping it drops the `Sender`, which
   closes the channel), and tell axum to stop accepting connections.
3. Let the writer finish the backlog. It must not be a sibling that gets
   cancelled with the producer: spawn it, keep its `JoinHandle`, and let a
   closed channel be its normal `Ok(())` rather than an error.
4. Bound the wait, then close the pool last.

## Sketch

A signal future:

```rust
async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut sigterm = signal(SignalKind::terminate()).expect("sigterm handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}, // SIGINT, dev
        _ = sigterm.recv() => {},          // SIGTERM, systemd/podman
    }
}
```

Share it between both halves through one cancellation handle — for example a
`tokio_util::sync::CancellationToken`, cancelled when the signal arrives.

Ingest, restructured so the writer outlives the producer:

```rust
let writer = tokio::spawn(store_envelopes(pool.clone(), envelope_receiver, devices));

tokio::select! {
    r = subscribe_mqtt(host, port, envelope_sender) => r?, // the producer failed
    _ = shutdown.cancelled() => {}                          // asked to stop
}
// The producer future is dropped here: Sender dropped → channel closed → writer drains.

if tokio::time::timeout(Duration::from_secs(5), writer).await.is_err() {
    warn!("writer did not drain within 5 s; exiting anyway");
}
pool.close().await;
```

HTTP: axum has this built in — `axum::serve(listener, app)
.with_graceful_shutdown(…)` stops accepting and lets in-flight requests finish,
which is what closes the ambiguous-mint window in item 3 above.

**Order matters:** stop ingress first (HTTP and MQTT), drain the writer, close
the pool last. Five seconds is far below systemd's 90 s SIGKILL deadline, so the
process always exits on its own terms.

Once manual acks land (see
[ingest-db-error-handling.md](ingest-db-error-handling.md)), envelopes drained
at shutdown must be acked before the MQTT disconnect, or they are redelivered on
the next start. That is harmless — `UNIQUE (device_id, seq, time)` makes a
redelivered insert a no-op — but it is noise.

## Concepts this exercise teaches

- `tokio::signal`, and why SIGTERM needs the unix module while Ctrl-C is
  cross-platform
- `tokio::select!` and cancellation-by-drop — including dropping more than you
  meant to
- mpsc close-and-drain semantics (the `recv() -> None` contract)
- `JoinHandle` as the "wait for this task" primitive
- Bounded shutdown time against a supervisor's kill deadline
