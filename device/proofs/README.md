# Device proof lane — `make -C device prove`

Two legs, run by the same target:

1. **CBMC** (bounded model checking, this directory's harnesses): CBMC
   6.8.0, from the Kani bundle: probe `~/.kani/kani-*/bin/cbmc`, fall back
   to system `cbmc`; if neither exists the target fails pointing at the
   install ladder in TOOLING.md. kissat (bundled next to cbmc) is used as
   the SAT back-end when present — the boot-gate harness's
   CRC-determinism equivalence does not finish on CBMC's built-in solver
   but solves in seconds on kissat.
2. **Frama-C/WP** (deductive, unbounded — see "Frama-C/WP leg" below):
   `run-wp.sh` in the pinned `framac/frama-c:32.1` container. Skipped with
   a notice if docker or the image is absent (`docker pull
   framac/frama-c:32.1` to enable); CI runs it unconditionally on every
   push/PR, so a local skip never hides a regression. `make -C device
   prove-wp` runs just this leg.

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

Latest run: `0 of 627 failed` (rte), `0 of 619 failed` (confinement),
`0 of 609 failed` (boot gate), `0 of 311 failed` (link) — counts dropped
slightly when the WP round replaced a few narrowing ops with wide-unsigned
forms (see "Core changes forced by WP"). CBMC leg of
`make -C device prove`: ~100 s (most of it the link harness); the WP leg
adds ~75 s.

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
carry ACSL contracts whose full-range claim is discharged deductively by
the Frama-C/WP leg (below).

## Frama-C/WP leg — deductive, unbounded (`run-wp.sh`)

CBMC above is *bounded* model checking: exhaustive over the small model.
The WP leg discharges the ACSL contracts and loop annotations in `core/`
and `include/updater/port.h` for **unbounded** inputs and the full
`uint16_t` geometry range — quantified, not enumerated. Environment is
pinned: Frama-C 32.1 (Germanium) + Alt-Ergo 2.5.4, i.e. the
`framac/frama-c:32.1` image (32.1 was the current stable release at
adoption, June 2026; the `dev` tags float and would let "proven" drift).
Latest run: **534 goals, 534 proven, 0 unproven** (Qed 314, Alt-Ergo 206,
plus terminating/unreachable bookkeeping goals), ~75 s wall on 20 threads.

What the proven goal set contains:

- **UB freedom** (`-wp-rte`): memory access validity, signed overflow,
  division, shift-width guards, for every function in the five core units.
- **The port contract** (`port.h`): all 9 `port_*` functions now carry
  ACSL. Device state (flash, wire, ticks, the entry pair) is reachable
  only through these functions, so `assigns \nothing` on the readers is
  the exact frame the core is proven in; results stay unconstrained (no
  false determinism is smuggled in).
- **Invariant 1 (confinement)**: the `page < app_pages` / offset-in-region
  ACSL asserts at every flash call site, now proven for the full geometry
  range including degenerate ports (`region < 16` guards).
- **Session contracts**: every `handle_*` has requires/assigns/ensures;
  `upd_handle`'s reply-length contract (`0 iff rsp_cap == 0`, always
  `<= rsp_cap`) and its `\separated` preconditions (rsp scratch vs
  session/request — what every shipped main loop does).
- **Codec behaviors** (`proto.c`): parse/build behaviors incl. `complete
  /disjoint behaviors`, the in-place build aliasing contract.
- **The link pump state invariant** (`link_stream.c`): the file-header
  invariant (`in_frame ==> n < buf_len`, LEN-bound) is now a
  machine-checked requires/ensures pair, established by `link_init` and
  re-established by every `link_poll` — plus in-bounds buffer writes for
  any byte stream.

Scope decisions (deliberate, in force):

- **Unsigned wrap / narrowing lints are the CBMC lane's job.** WP runs the
  default RTE set = actual UB. `--unsigned-overflow-check` and
  `--conversion-check` (defined behavior, style discipline) stay
  exhaustively enforced above; Alt-Ergo models `lsl/lxor/lor` as
  near-uninterpreted integer functions (no upper-bound axioms in WP's
  cbits theory), so keeping those non-UB goals in the WP gate would force
  rewriting every shift/xor idiom purely to please the prover.
- **CRC functional completeness is out of scope** (unchanged from the
  CBMC round): `upd_crc8`/`upd_crc32_*` carry `assigns \nothing` frames,
  not an ACSL logic-function model of the polynomial — high effort, no
  safety yield (CRC values are checked end-to-end by unit tests, golden
  vectors and the conformance campaigns).
- **Pump termination is not claimed deductively.** `link_poll` carries
  `terminates \false` — honest: termination rests on the io contract
  (get_byte must eventually run dry, link.h) which needs a ghost stream
  model to express. The CBMC link harness proves it bounded
  (`--unwinding-assertions` over every parked state); no fake variant.
- **Indirect calls are modeled.** WP cannot reason about calls through
  function pointers with an open target set, so under `__FRAMAC__` the two
  io-callback invocations route through extern prototypes whose contracts
  are exactly link.h's callback contract (`assigns *b` / `assigns
  \nothing`, nondet results). The shipped build calls through the pointers
  unchanged, and the CBMC link harness verifies that real indirect-call
  path.

Driver: `device/proofs/run-wp.sh` — the same file CI executes (job
`frama-c-proofs`, `.github/workflows/ci.yml`); it fails on any unproven
goal and leaves the full log in `device/proofs/wp.log` (CI uploads it as
the `wp-report` artifact).

## Core changes forced by WP (all verified equivalent: unit tests + CBMC)

1. `core/update.c` `handle_write`: the page-index LE assembly now computes
   in wide `unsigned` with an explicit mask (proto.c discipline) instead
   of int-promoted `uint16_t` shifts — same values, and the
   signed-overflow RTE goal becomes dischargeable (Alt-Ergo cannot bound
   `lsl`).
2. `core/update.c` `handle_echo` / `core/link_stream.c` `link_poll`:
   loop-stable heap fields (`req->len`; `l->buf`, `l->buf_len`, `l->io`)
   are snapshotted into locals so loop annotations range over logic
   constants instead of heap loads re-read under every memory update —
   same object code, goals go from timeout to proven.
3. `core/link_stream.c` `link_poll`: the cross-call state invariant was
   documentation; it is now a requires/ensures pair. That found no code
   bug (the invariant does hold) but it WAS a missing precondition: a
   caller fabricating a `link_t` with `in_frame` set and `n >= buf_len`
   would index out of bounds. `link_init` establishes it; polls preserve
   it.

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
