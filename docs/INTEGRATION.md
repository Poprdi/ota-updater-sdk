# Updater SDK — integration datasheet

How to put the updater into a product: what it does, exact limits,
copy-paste recipes per target, and every sharp edge we hit building it.
Wire format: [PROTOCOL.md](PROTOCOL.md). New MCU ports:
[PORTING.md](PORTING.md).

## 1. Capabilities and hard limits

What it does:

| Capability | Mechanism |
|---|---|
| Flash an app over I2C, UART, SPI, or 2 spare GPIOs | one wire protocol, transport bindings in PROTOCOL.md §10 |
| Survive power loss at ANY instant | CRC-validated footer; a torn image fails its own CRC, device stays in the bootloader, one reflash command recovers (demonstrated on silicon) |
| Survive lost/corrupt bytes | CRC-8 frames + idempotent retries; no state corruption from re-sent frames |
| Unbrickable by update | the bootloader never writes its own region — hardware-enforced (AVR BOOTSIZE fuse) or link-enforced (RP2350) |
| Re-enter the bootloader from a running app | `app_stub.h`, one function call |
| Same host code everywhere | `updater-core`/`updater-eh` are `no_std` and zero-alloc; `updater-core` has zero dependencies, `updater-eh` only the `embedded-hal`/`embedded-io` traits — identical on an RPi, a Pico 2 W, an ESP32 |

Hard limits — engineering around these is out of scope by design:

| Limit | Value |
|---|---|
| Image size | ≤ app region − 16 bytes (footer); host refuses larger before touching the device |
| No signing, no A/B slots, no compression | v1 reserves wire space for them; nothing more |
| One outstanding command | master-polled; no pipelining, no unsolicited device traffic |
| Protocol page ≤ 250 bytes | LEN is a u8 (PROTOCOL.md §6) |
| App region ≤ 65535 pages | app_pages is u16 |
| No bootloader self-update | install path is a debug adapter, once per board |

## 2. Host: Raspberry Pi / any Linux (updater-cli)

Build:

```sh
cd host && cargo build --release -p updater-cli
# binary: host/target/release/updater-cli
```

I2C prerequisites (RPi): enable the controller (`raspi-config` →
Interface Options → I2C, or `dtparam=i2c_arm=on` in `config.txt` +
reboot); the bus is `/dev/i2c-1`; put your user in the `i2c` group. But
read Gotcha 2 before choosing I2C on a Pi.

Commands (defaults: `--transport i2c --bus /dev/i2c-1 --addr 0x20`):

```sh
updater-cli info                          # identity, geometry, app valid?
updater-cli echo --data DEADBEEF          # link smoke test, ≤ 16 bytes hex
updater-cli flash app.bin                 # erase + write + verify (.hex parsed iff extension is .hex)
updater-cli boot                          # jump to the app
```

Other transports:

```sh
updater-cli --transport uart --dev /dev/ttyACM0 --baud 115200 info
updater-cli --transport spi  --dev /dev/spidev0.0 --speed-hz 100000 info
updater-cli --transport gpio --chip /dev/gpiochip0 --pin-tx 23 --pin-rx 24 info
```

`--baud` accepts the termios standard set 9600–230400. `--dev` defaults:
`/dev/ttyUSB0` (uart), `/dev/spidev0.0` (spi). Response polling:
`--poll-attempts`/`--poll-delay-ms`, default 100 × 10 ms. `flash`
additionally raises the ERASE_APP exchange's budget automatically from
the geometry the device reports — enough attempts to cover
`app_pages × 10 ms` of worst-case per-page erase; a larger
`--poll-attempts` wins (Gotcha 1).

## 3. Host: Pico 2 W / ESP32 (updater-eh, no_std)

`updater-core` + `updater-eh` are `no_std`, allocation-free, and take
buffers from you. `updater-eh` adapts any HAL speaking `embedded-hal` 1.x
/ `embedded-io` 0.7 (embassy-rp 0.10 and esp-hal 1.x qualify). Cargo:

```toml
[dependencies]
updater-core = { git = "<this repo>", package = "updater-core" }
updater-eh   = { git = "<this repo>", package = "updater-eh" }
```

One full self-serve update path (I2C shown; `UartTransport`,
`SpiTransport`, `SoftUartTransport` wire identically):

```rust
// buf sizing rule (updater_core::session docs): largest request +
// largest response; 320 covers every legal geometry.
let mut buf = [0u8; 320];
let transport = updater_eh::I2cTransport::new(i2c, delay, 0x20)
    .with_poll(2400, 50_000_000); // 120 s: cover ERASE_APP (Gotcha 1)
let mut session = updater_core::Session::new(transport, &mut buf);

let info = session.info()?;
let img = updater_core::image::Image::from_bin(app_bytes, info.page_size, info.app_pages)?;
session.flash(&img, &mut |done, total| { /* progress */ })?;
session.boot()?;
```

Notes:
- Default poll budget is ~1 s (200 × 5 ms) — sized for short commands
  and single page writes, NOT for ERASE_APP. Widen with `with_poll` so
  attempts × delay covers the target's erase envelope (Gotcha 1);
  `set_poll` re-tunes a live transport.
- The app binary arrives however your platform gets bytes (TCP, BLE,
  UART...); the SDK takes `&[u8]`. There is no `Vec` anywhere.
- ESP32 + `SoftUartTransport`: don't (Gotcha 6). Use a hardware UART.

## 4. Device: RP2350 / Pico 2 W (rp2350_uart port)

Geometry: bootloader in the first 64 KiB of flash, app region = next
1 MiB (8192 × 128-byte protocol pages), UART0 at 115200 8N1
(`-DUPDATER_UART_BAUD` to change).

Build (pico-sdk 2.1.1, `PICO_SDK_PATH` set; no SDK submodules needed):

```sh
cd device/ports/rp2350_uart
cmake -S . -B build -G Ninja -DCMAKE_BUILD_TYPE=MinSizeRel
ninja -C build          # -> build/bootloader.uf2, build/blink.bin
```

Install the bootloader (once, or after bootloader changes) — either:

```sh
# zero-tool: hold BOOTSEL while plugging USB, then
cp build/bootloader.uf2 /run/media/$USER/RP2350/
# or picotool (install.toml target rp2350-bootsel):
updater-cli install --target rp2350-bootsel --image build/bootloader.uf2
```

**A silent USB bus is success.** This bootloader has no USB: the BOOTSEL
drive disappears and nothing re-enumerates. It is listening on UART0.
Recovery is always available via BOOTSEL.

Wiring (3V3 UART, e.g. Raspberry Pi Debug Probe's UART port):

| Probe wire | Signal | Pico 2 W |
|---|---|---|
| orange (probe TX) | → device RX | GPIO1, pin 2 |
| yellow (probe RX) | ← device TX | GPIO0, pin 1 |
| black | GND | pin 3 |

The Pico stays powered from its own USB. First cycle:

```sh
updater-cli --transport uart --dev /dev/ttyACM0 info
updater-cli --transport uart --dev /dev/ttyACM0 flash build/blink.bin
updater-cli --transport uart --dev /dev/ttyACM0 boot
```

Apps for this port are plain pico-sdk binaries linked at the app region;
the entire app-project contract is one line:

```cmake
pico_set_linker_script(<app> ${PORT_DIR}/memmap_app.ld)
```

Flash the **`.bin`** (never the `.uf2` — the ROM never sees these apps;
the bootloader enters them directly). The reference app is
`device/ports/rp2350_uart/app/blink.c` (heartbeat on UART0; sending `U`
re-enters the bootloader via the app stub).

## 5. Device: AVR64EA28 (avr_ea_twi / avr_ea_uart ports)

Geometry: 4 KiB boot section, app region 0x1000–0xFFFF (480 × 128-byte
pages). Build (avr-gcc):

```sh
make -C device/ports/avr_ea_twi    # -> build/firmware_0x10.hex, firmware_0x20.hex
make -C device/ports/avr_ea_uart   # -> build/firmware_115200.hex
make -C device/ports/avr_ea_uart UART_BAUD=57600   # 57600 variant
```

Install once per board via UPDI (Atmel-ICE recipe ships in
`install.toml`; run from the repo root):

```sh
updater-cli install --target avr64ea28-updi \
    --image device/ports/avr_ea_twi/build/firmware_0x10.hex
```

This single avrdude invocation programs **BOOTSIZE=8** (8 × 512 B = 4 KiB
boot section) and flashes the bootloader. The fuse write is mandatory
and one-time: with BOOTSIZE unprogrammed the whole flash is boot section
and the bootloader can never write the app region. After this, the
UPDI adapter is never needed again — updates ride the I2C/UART wire.

App projects link above the boot section:

```
-Wl,--section-start=.text=0x1000
```

Vectors land correctly because CPUINT IVSEL resets to 0 (vectors follow
the boot section). Bring-up premises worth one minute: the port assumes
FUSE.OSCCFG at the 20 MHz factory default (UART baud math and the ms
tick derive from it), and the TWI variant's polled-flag configuration is
audited but see `device/ports/avr_ea_twi/PORT_AUDIT.md` § bring-up.

## 6. Device: a new MCU

Implement the nine functions of `device/include/updater/port.h`, write
the main loop against the obligations checklist, ship a `PORT_AUDIT.md`.
The complete procedure, checklist and audit template:
[PORTING.md](PORTING.md).

## 7. App-side integration (any target)

Re-entry: include `device/include/updater/app_stub.h`, call
`updater_reboot_to_bootloader()` from a project-defined trigger —
typically a write to a reserved/OVERRIDE register in the project's I2C
register file (the motor_controller pattern: one of its reserved regs).
It never returns; the bootloader comes up resident (entry window
skipped) and the host can start a session immediately.

Constraints on the app:

- Image ≤ app region − 16 bytes. The last 16 bytes belong to the boot
  footer, which the **host** writes during flashing — the app never
  emits it, it just must not claim those bytes.
- AVR-EA: app and bootloader MUST be built for the same MCU — both
  derive the re-entry pair's SRAM address from their own
  `INTERNAL_SRAM_END`; a mismatch silently breaks re-entry.
- Don't call the stub with meaningful interrupts in flight: it resets
  the chip immediately (AVR variant masks interrupts itself first —
  the pair sits where stack pushes land).

## 8. Gotchas

Every sharp edge found while building and hardware-validating this SDK.

1. **ERASE_APP holds the bus for a long time, and the default poll
   budgets (except `flash`'s) will time out on it.** One command erases
   the whole region before replying. Measured/derived envelopes:
   AVR64EA28 ≈ 5 s (480 pages × ~10 ms); RP2350 1 MiB ≈ **12–100 s on
   dirty flash** (256 sector erases at ~45–400 ms each). The RP2350
   footgun: a **clean** region erases in ~3 s (observed on silicon —
   erase-verify exits early), so a budget "validated" on fresh flash
   dies on the first real re-flash. Arithmetic: poll attempts × delay ≥
   the target's worst-case erase envelope. The CLI's `flash` subcommand
   sizes the ERASE_APP budget for you from the reported geometry
   (`app_pages × 10 ms`; ~4.8 s on the AVR-EA, ~82 s on the RP2350 —
   for the pathological tail of the RP2350 envelope raise
   `--poll-attempts`, which wins when larger). Every other CLI exchange
   keeps 100 × 10 ms, and the updater-eh adapters default to ~1 s until
   you `with_poll` them — sizing those is on you.
2. **Raspberry Pi built-in I2C vs clock stretching.** The AVR TWI port's
   only flow control is stretching SCL — for seconds during ERASE_APP.
   The BCM283x-family I2C controller on Raspberry Pis has a
   long-standing clock-stretching erratum (it can sample data early
   after a stretch, corrupting reads). Prefer the UART transport on a
   Pi, or a USB adapter that genuinely supports stretching. If you must
   use Pi I2C, verify the full erase cycle on your exact Pi model
   before trusting it.
3. **USB-CDC serial adapters can re-enumerate mid-session** (observed
   live: a Debug Probe dropped and returned with a new device number
   mid-flash). The session holding the stale fd burns its full retry
   budget and fails. Recovery is trivial: re-run the command — protocol
   idempotence makes the retry safe at any interruption point. Related:
   keep exactly ONE reader on the tty (a second monitor process steals
   response bytes and produces phantom failures), and don't pipe CLI
   output through buffering filters during long operations.
4. **Boot-time UART line noise.** Plugging connectors and target
   power-up put junk bytes on the line. The stream framing eats them by
   design (sync hunt + CRC + silent re-hunt) — do not "fix" garbage
   observed at plug-in; only a valid frame does anything.
5. **Re-entry storage semantics: soft reset vs power-on.** The app-stub
   pair (AVR: last 4 SRAM bytes; RP2350: watchdog SCRATCH2/3) survives a
   soft reset and is *expected garbage* after power-on — that is why the
   pair is magic + one's complement, and why the bootloader clears it
   after reading. Consequences: re-entry cannot be requested across a
   power cycle by design, and nothing else on the AVR may use those 4
   bytes of SRAM (`.init3` captures them before the C runtime's first
   push; on RP2350 SCRATCH4–7 are avoided because the SDK/ROM own them).
6. **Bit-banged softuart is physics-limited.** One bit at 9600 baud is
   104 µs. On an ESP32 with WiFi up, radio interrupts stretch bits past
   recovery — use a hardware UART. On Linux, scheduler preemption does
   the same (the gpio backend busy-waits, but preemption mid-byte causes
   CRC drops → retries). The softuart exists for pin-budget emergencies,
   not throughput; prefer a real UART whenever one exists.
7. **The conformance sim's 255-byte cap is not a wire guarantee.** Real
   ports accept frames only up to `page_size + 8` bytes total (RX buffer
   `page_size + 8` bytes; the largest legal frame is `page_size + 5`
   bytes total) and drop longer frames at the wire (PROTOCOL.md §7).
   Size device buffers from geometry; never assume a device accepts
   codec-maximum frames.

## 9. Wire protocol

Normative frame layout, commands, status codes, footer, entry model,
transport bindings, golden vectors: [PROTOCOL.md](PROTOCOL.md).
