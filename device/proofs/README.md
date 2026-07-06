# Device proof lane — `make -C device prove`

Bounded model checking of the device core with CBMC (6.8.0, from the Kani
bundle: probe `~/.kani/kani-*/bin/cbmc`, fall back to system `cbmc`; if
neither exists the target fails pointing at the install ladder in
TOOLING.md). kissat (bundled next to cbmc) is used as the SAT back-end when
present — the boot-gate harness's CRC-determinism equivalence does not
finish on CBMC's built-in solver but solves in seconds on kissat.

## What is proven (zero failed checks, all harnesses)

Every harness runs with ALL of:
`--bounds-check --pointer-check --conversion-check --div-by-zero-check
--signed-overflow-check --unsigned-overflow-check --unwinding-assertions`.

**No check is dropped.** `--unsigned-overflow-check` and
`--conversion-check` initially flagged the core's intentional mod-256
arithmetic (`(uint8_t)(len+3)` wrap test in the codec, `(uint8_t)(crc<<1)`
in CRC-8) and one real latent issue; instead of dropping the lint, the core
was rewritten to compute in wide unsigned types and compare/mask *before*
any narrowing cast — the accept/reject sets and CRC values are bit-for-bit
identical (unit tests unchanged and green), but no wrapping or truncating
operation is ever executed, so the checks pass as genuine proofs.

- `harness_rte.c` — RTE freedom. Nondet-drives `upd_crc8`,
  `upd_crc32_init/update/final`, `upd_frame_parse`, `upd_frame_build` and
  `upd_handle` (all commands, both `erased` states, full 8-bit `len` and
  `rsp_cap` ranges). Buffers are `malloc`'d at their exact declared length,
  so any off-by-one access is a pointer-check counterexample rather than a
  silent read of slack space. Also checks the codec postconditions
  (`payload == buf+2`, length bookkeeping) and `upd_handle`'s return
  contract (`0 iff rsp_cap == 0`, `<= rsp_cap`).
- `harness_link.c` — **stream link resync safety.** `link_poll` is driven
  over a fully nondet byte stream (<= 10 bytes) delivered in two
  nondet-sized chunks into an exact-`malloc`'d receive buffer of nondet
  size <= 10; the split point is arbitrary, so the second poll covers
  resuming from every parked state (hunting, mid-header, mid-payload).
  Proves the parser never writes outside its buffer, never reads past the
  bytes the stream handed it, always returns once the stream is drained
  (`--unwinding-assertions` — no byte sequence wedges the pump), and that
  delivered frames satisfy the link contract (`payload == buf+2`, frame
  fits the buffer) while the state invariant (`in_frame ==> n < buf_len`)
  survives every poll. `link_send` is driven with an exact-length nondet
  frame so an over-read is a pointer-check failure. Runs with its own
  `--unwind 12` (the model's largest loop is the 11-iteration pump; the
  shared 70 turns the pump x parse x CRC-8 nesting into minutes of solver
  time) and links only `crc8.c + proto.c + link_stream.c` — `update.c`
  would drag the port contract in for code the harness never reaches.
- `harness_confinement.c` — **Invariant 1: flash confinement.** Port stubs
  assert `page < app_pages` (erase/write), `offset < page_size*app_pages`
  (read), and that `upd_handle` never reaches `port_jump_to_app`. The
  write stub reads back `page_size` bytes from the supplied pointer,
  proving the core always hands the port a full page.
- `harness_boot_gate.c` — **Invariant 2: jump gating.** Flash is a stable
  array of fully nondet bytes (footer included), so `upd_app_valid` is a
  deterministic predicate; the harness pre-evaluates it and proves
  `jumped ==> valid`, plus completeness (`valid ==> jumped`), that the
  return value reports the jump exactly, and that the boot path never
  erases/writes flash and never reads outside the app region.

Latest run: `0 of 654 failed` (rte), `0 of 646 failed` (confinement),
`0 of 636 failed` (boot gate), `0 of 335 failed` (link). Whole
`make -C device prove`: ~106 s (~70 s of it the link harness).

## Model bounds (`PROOF_SMALL_MODEL`) and why they are sound to use

The harnesses bound the nondet port geometry to `page_size <= 16`,
`app_pages <= 4` (region <= 64 bytes) and codec buffers to <= 24 bytes;
`--unwind 70` then strictly dominates every reachable loop bound (largest:
the 64-byte flash fill and the <= 48-byte CRC32 sweep), and
`--unwinding-assertions` *proves* that dominance — an unwind assertion
failure means the bound argument is wrong, and the fix is raising
`--unwind`, never removing the flag. Degenerate geometries (page_size or
app_pages of 0, region < 16) are **included** in the model, which is what
forced the `region < 16` guards below.

Why the small model is meaningful: none of the proved properties depend on
loop trip counts beyond the covered patterns — the control flow of
`upd_handle`, the codec's header/CRC arithmetic (LEN spans its full 8-bit
range in the harnesses, including the >= 253 rejection edge), and the
bounds comparisons are identical at page_size 16 and 512. What the small
model does NOT cover is arithmetic that only misbehaves at large operand
values; those sites are guarded structurally (`region_bytes` casts to
`uint32_t` before multiplying, all length math is done in `unsigned`) and
carry ACSL contracts so the full-range claim is discharged deductively by
the Frama-C leg when it lands.

## What Frama-C/WP adds later

CBMC here is *bounded* model checking: exhaustive over the small model.
Frama-C/WP discharges the ACSL function contracts and loop invariants
already annotated in `core/` for **unbounded** inputs and the full
`uint16_t` geometry range — quantified, not enumerated. The Makefile's
prover probe means no restructuring is needed: install Frama-C (opam, no
sudo) and add its WP invocation as a second leg. Known ACSL gap carried
forward deliberately: `upd_crc8` has no ACSL logic-function model of the
CRC (the `behavior ok` completeness of callers is stated over lengths and
bytes, not CRC values); it was assessed as high-effort/low-yield for this
task and is deferred to the Frama-C leg.

## Core changes forced by CBMC (all verified equivalent by the unit tests)

1. `core/update.c` `handle_verify`: `region_bytes - 16u` wrapped for a
   degenerate port (region < 16), letting a huge `length` pass the range
   check and drive `flash_crc` out of the app region (also the cause of a
   `flash_crc` unwinding-assertion failure). Added the `region < 16 =>
   OUT_OF_RANGE` guard mirroring `upd_app_valid`.
2. `core/crc8.c`: shift into a wide `unsigned`, mask, then cast — the
   truncation is explicit arithmetic and the cast is value-preserving.
3. `core/proto.c` parse/build: length checks now compare
   `(unsigned)len + UPD_FRAME_OVERHEAD` directly (<= 258, never wraps);
   `build` truncates to `uint8_t` only after proving `total <= 255`.
