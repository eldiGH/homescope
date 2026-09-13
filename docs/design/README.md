# Design records

Worked-out designs for Homescope — each with its decision, the reasoning, the
alternatives weighed and rejected, and what was actually built. They began as
local scratch notes and were committed on 2026-09-13, once they had become the
project's memory of *why*.

How to read them:

- **Settled means settled.** The reasoning is recorded so that reopening a
  decision starts from what was already weighed, not from scratch.
- **Every record opens with a status line.** Inline ✅ / ⚠️ markers record what
  has since been built, or built differently; the original reasoning stays
  beside them rather than being rewritten into hindsight.
- **Records are not the source of truth for the present.** `docs/protocol.md`
  is authoritative for what is on the wire, and the code for what is built.
  Where a record disagrees with either, the record is stale — fix it.

| Record | Status | Scope |
|---|---|---|
| [provisioning.md](provisioning.md) | 🔶 In progress | Device identity and per-device AEAD keys: the UICR record, keys encrypted at rest under a KEK, and the `homescope-provision` tool |
| [packet-tv-aead.md](packet-tv-aead.md) | ✅ Implemented | TV measurement encoding, flash-persisted seq, ChaCha20-Poly1305 on the air packet (protocol v0.5–v0.7) |
| [ingest-db-error-handling.md](ingest-db-error-handling.md) | ⏳ Planned | Ingest durability through database outages (manual MQTT acks) and the per-device seq check |
| [api-graceful-shutdown.md](api-graceful-shutdown.md) | ⏳ Planned | Draining ingest and in-flight HTTP requests on SIGTERM |
| [site-room-topology.md](site-room-topology.md) | 🔶 Partly done | `devices.site`/`room`, the per-gateway topic prefix, and the settled decisions around them |
| [mosquitto-acl.md](mosquitto-acl.md) | ⏳ Planned | Broker authentication and per-gateway topic ACLs |
| [receiver-usb-link.md](receiver-usb-link.md) | 🔶 Partly done | The receiver's VID/PID, its udev rule, and a gateway service bound to the dongle's presence |
| [firmware-variants.md](firmware-variants.md) | ⏳ Planned | One firmware per board with boot-time sensor detection; the firmware artifact store |
| [twim-cancel-safety.md](twim-cancel-safety.md) | ⏳ Open bug | The orphaned-DMA hazard in async TWIM, and its fix |
| [packet-batching.md](packet-batching.md) | ⏳ Deferred | A protocol v2 body carrying batched readings |

Legend: ✅ implemented · 🔶 in progress or partly done · ⏳ planned, deferred or
open.
