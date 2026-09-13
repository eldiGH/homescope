# TWIM async cancellation — the orphaned-DMA footgun

> **Status: ⏳ open bug.** Checked 2026-09-13: `firmware/sensor/src/sensors/sht4x.rs`
> still wraps its I²C transfer in `with_timeout`. Written 2026-08-26, when this
> resurfaced as a **prerequisite** for boot-time I²C sensor detection
> ([firmware-variants.md](firmware-variants.md)); before that it was a one-line
> gotcha in `CLAUDE.md`, and it needed a fix design, not just a warning.
>
> Line numbers are against `embassy-nrf` **0.10.0**, `src/twim.rs`, re-checked
> 2026-09-13. Check them again before acting on them — the crate moves.

## The defect

`Twim::transaction` (twim.rs:616) is **not cancel-safe**:

```rust
pub async fn transaction(&mut self, address: u8, mut operations: &mut [Operation<'_>])
    -> Result<(), Error>
{
    let mut last_op = None;
    while !operations.is_empty() {
        let ops = self.setup_operations(address, operations, last_op, true)?;  // starts EasyDMA
        let (in_progress, rest) = operations.split_at_mut(ops);
        self.async_wait().await?;                                              // <-- droppable
        ...
    }
}
```

`setup_operations` programs the DMA pointers and kicks off the transfer. Then
`async_wait` (twim.rs:360) is a **bare `poll_fn`**: it registers a waker and
polls the event registers. There is no `OnDrop` guard, no `Drop` impl on the
future — nothing that runs when the future is dropped instead of completed.

The only `Drop` in the module belongs to the `Twim` struct itself
(twim.rs:724), and it is not a rescue path:

```rust
impl<'a> Drop for Twim<'a> {
    fn drop(&mut self) {
        trace!("twim drop");
        // TODO: check for abort          <-- upstream's own comment
        let r = self.r;
        r.enable().write(|w| w.set_enable(vals::Enable::DISABLED));
        ...
    }
}
```

That comment is upstream acknowledging the gap. And the firmware never drops the
`Twim` anyway — it lives for the life of the program.

So dropping the future **abandons a live transfer**. Nothing issues
`tasks_stop`, nothing waits for `EVENTS_STOPPED`, nothing disables the
peripheral.

## Why that is worse than a leak

Two distinct harms, and the first is the serious one:

1. **EasyDMA keeps writing into a buffer that no longer exists.** On an
   `Operation::Read`, the destination pointer points into the dropped future's
   stack frame. That memory is immediately reused by whatever runs next. This
   is memory corruption with no diagnostic — arbitrary bytes appearing in an
   unrelated local, minutes later, once.

2. **The next transaction can return success having transferred nothing.**
   `setup_operations` does clear `events_suspended`, `events_stopped` and
   `events_error` before starting (twim.rs:402). But the abandoned transfer is
   still in flight: it completes on its own schedule, sets `EVENTS_STOPPED`, and
   the *new* transaction's `async_wait` sees that flag and returns `Ok(())`
   immediately. The caller reads an unwritten buffer and believes it.

⚠️ **The observed field symptom is worse than either of these predicts.**
`CLAUDE.md` records it as: *the first timeout looks like a one-off, then
everything fails forever.* A single abandoned transfer does not obviously explain
a permanent failure. A plausible mechanism is a multi-operation transaction
abandoned between operations, which uses `SUSPEND` rather than `STOP` and can
leave the peripheral latched suspended — but **this has not been confirmed
against the hardware.** Treat the permanence as observed and the mechanism as
unverified; do not build a fix that only addresses the mechanism.

## Where it is today

`firmware/sensor/src/sensors/sht4x.rs` uses exactly this pattern —
`with_timeout` around an I²C transfer — and has **no recovery path**.

A debugging rule worth keeping even after the fix: separate the two questions.

- *Why did it hang the first time?* → wiring or power: stuck SCL/SDA, pull-ups
  sitting on a gated rail that is currently off, a sensor mid-power-up.
- *Why does it never recover?* → this bug.

Conflating them sends you chasing a hardware fault that is downstream of a
software one.

## The fix: `blocking_transaction_timeout`

It already exists (twim.rs:588), behind the `time` cargo feature — which
`firmware/sensor/Cargo.toml` **already enables**. And it does the one thing
`with_timeout` cannot:

```rust
fn blocking_wait_timeout(&mut self, timeout: Duration) -> Result<(), Error> {
    ...
    if Instant::now() > deadline {
        r.tasks_stop().write_value(1);        // <-- aborts the transfer in hardware
        return Err(Error::Timeout);
    }
}
```

It stops the peripheral before returning, so DMA is not left writing into a
frame that is about to be reused. That is the whole difference.

**Cost**: it busy-waits, blocking the executor for up to `timeout`.

Which makes the trade-off depend on the caller:

- **Boot-time bus probe** ([firmware-variants.md](firmware-variants.md)): use
  it, without reservation. Nothing else is running, the timeout is ~10 ms, and an
  absent device NACKs immediately anyway — the timeout only fires on a genuinely
  stuck bus, which is the case being survived.
- **The 60 s telemetry loop**: acceptable with a short timeout, but the
  advertiser and MPSL are live. Prefer keeping the async path and simply **not
  wrapping it in `with_timeout`** — a hung bus then hangs that task rather than
  corrupting memory, and the watchdog (still unwritten) is the correct backstop
  for a hung task.

## The rules

1. ⚠️ **Never wrap `Twim::transaction` — or anything calling it — in
   `with_timeout`, `select`, or any other combinator that can drop it.** That
   includes `embedded-hal-async`'s `I2c::transaction`, which forwards to it
   (twim.rs:843), so it applies to every driver crate too, `sht4x` included.
2. Where a bounded wait is genuinely required, use `blocking_transaction_timeout`
   and accept the busy-wait.
3. A timeout is not a recovery. Even with the abort, a stuck bus stays stuck
   until the peripheral is re-enabled or the rail is power-cycled. The gated
   sensor rail on the custom PCB makes a real recovery possible — drop the LDO
   EN, wait, bring it back — and that is worth building once the hardware has
   it.
4. Fix `sensors/sht4x.rs` **before** adding the boot probe, not after — the probe
   is more transactions on the same bus, and one of them is against an address
   that may not answer.

## Also worth checking when this is fixed

`nrf-sdc`'s `Flash` (used by `seq_counter.rs`) is a different peripheral and a
different driver, but the same question applies: is its future safe to drop
mid-erase? Nothing in the current code cancels it, so this is a latent question
rather than a live bug — but the seq counter is the one piece of state where a
corrupted write is a **security** failure (nonce reuse; see `seq_counter.rs`'s
module docs), so it deserves an answer before anything starts racing it against
a timeout.
