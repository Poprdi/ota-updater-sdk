// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Adrian Erlacher

//! `updater-cli` — drive the updater bootloader from a Linux master.
//!
//! `anyhow` lives only here, at the binary layer; everything below speaks
//! typed errors.

mod ihex;
mod install;
mod transport;

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use updater_core::image::Image;
use updater_core::Session;

use transport::{PollBudget, TransportSpec};

/// One buffer for every `Session` exchange, sized per the Session buffer
/// rule (see the `updater_core::session` module docs: largest request plus
/// largest response; 320 covers every legal geometry).
const FRAME_BUF_LEN: usize = 512;

/// Response-poll defaults, sized for the short commands (info/echo/boot
/// and per-page writes: the device answers within milliseconds).
const DEFAULT_POLL_ATTEMPTS: u32 = 100;
const DEFAULT_POLL_DELAY_MS: u32 = 10;

/// Worst-case per-protocol-page erase time, used to scale the `ERASE_APP`
/// poll budget from the geometry INFO reports (the device erases the whole
/// region inside that one exchange, holding the wire). 10 ms/page covers
/// the supported ports: the AVR-EA erases its 480-page region in ~5 s,
/// one long stretch (avr_ea_twi/PORT_AUDIT.md, bring-up note 3), and the
/// RP2350's 8192 pages yield an ~82 s budget against its 12-100 s
/// dirty-flash envelope (rp2350_uart/PORT_AUDIT.md, bring-up note 2) —
/// for the pathological tail of that envelope, raise --poll-attempts.
const PER_PAGE_ERASE_WORST_MS: u32 = 10;

/// The wire flags are global, so every subcommand's --help inherits them;
/// one labeled section keeps `install` (which never opens the wire) from
/// interleaving them with its own flags.
const TRANSPORT_HEADING: &str = "Transport (info/echo/flash/boot)";

#[derive(Parser)]
#[command(
    name = "updater-cli",
    version,
    about = "Flash and manage devices running the updater bootloader",
    long_about = "Flash and manage devices running the updater bootloader.\n\
                  Pick the wire with --transport; each transport reads only its own\n\
                  flags (i2c: --bus/--addr; uart: --dev/--baud; spi: --dev/--speed-hz;\n\
                  gpio: --chip/--pin-tx/--pin-rx)."
)]
struct Cli {
    /// Wire to the device
    #[arg(long, global = true, help_heading = TRANSPORT_HEADING, value_enum, default_value_t = TransportKind::I2c)]
    transport: TransportKind,

    /// I2C bus device node (i2c)
    #[arg(long, global = true, help_heading = TRANSPORT_HEADING, default_value = "/dev/i2c-1")]
    bus: String,

    /// 7-bit device address, decimal or 0x-prefixed hex (i2c)
    #[arg(long, global = true, help_heading = TRANSPORT_HEADING, default_value = "0x20", value_parser = parse_addr)]
    addr: u16,

    /// Serial or SPI device node [default: /dev/ttyUSB0 (uart),
    /// /dev/spidev0.0 (spi)]
    #[arg(long, global = true, help_heading = TRANSPORT_HEADING)]
    dev: Option<String>,

    /// Baud rate (uart)
    #[arg(long, global = true, help_heading = TRANSPORT_HEADING, default_value_t = 115_200)]
    baud: u32,

    /// SPI clock in Hz (spi); conservative default, the device polls its
    /// bus between your clock edges
    #[arg(long, global = true, help_heading = TRANSPORT_HEADING, default_value_t = 100_000)]
    speed_hz: u32,

    /// GPIO character device (gpio)
    #[arg(long, global = true, help_heading = TRANSPORT_HEADING, default_value = "/dev/gpiochip0")]
    chip: String,

    /// GPIO line offset we transmit on, wired to the device's RX (gpio)
    #[arg(long, global = true, help_heading = TRANSPORT_HEADING)]
    pin_tx: Option<u32>,

    /// GPIO line offset we listen on, wired to the device's TX (gpio)
    #[arg(long, global = true, help_heading = TRANSPORT_HEADING)]
    pin_rx: Option<u32>,

    /// How many times to poll for a response before giving up (flash
    /// raises the ERASE_APP exchange's budget from the device geometry
    /// when this is smaller — see flash --help)
    #[arg(long, global = true, help_heading = TRANSPORT_HEADING, default_value_t = DEFAULT_POLL_ATTEMPTS)]
    poll_attempts: u32,

    /// Delay between response polls, in milliseconds
    #[arg(long, global = true, help_heading = TRANSPORT_HEADING, default_value_t = DEFAULT_POLL_DELAY_MS)]
    poll_delay_ms: u32,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
enum TransportKind {
    /// I2C via /dev/i2c-* (default)
    I2c,
    /// Serial port in raw mode
    Uart,
    /// SPI via /dev/spidev*
    Spi,
    /// Bit-banged 9600-baud UART on two GPIO lines
    Gpio,
}

#[derive(Subcommand)]
enum Cmd {
    /// Query bootloader identity, geometry and app validity
    Info,
    /// Link smoke test: send up to 16 bytes and require them echoed back
    Echo {
        /// Payload as hex digits, e.g. DEADBEEF (max 16 bytes)
        #[arg(long)]
        data: String,
    },
    /// Erase, write and verify an application image (.hex or raw .bin)
    ///
    /// The ERASE_APP exchange's poll budget is raised automatically once
    /// the device reports its geometry — enough attempts to cover
    /// app_pages x 10 ms of worst-case per-page erase — because the device
    /// erases the whole region inside that one exchange; a larger
    /// --poll-attempts wins. Every other exchange keeps the base budget.
    Flash {
        /// Image file; parsed as Intel HEX iff the extension is .hex
        image: PathBuf,
    },
    /// Ask the device to boot the application
    Boot,
    /// Install the bootloader itself through a debug adapter (avrdude,
    /// probe-rs, openocd, ...) using a per-target template from install.toml
    #[command(long_about = "Install the bootloader itself through a debug adapter \
                            (UPDI/SWD/JTAG) by running the target's programmer with \
                            arguments from install.toml.\n\
                            Config search order: --config <path> if given, else \
                            ./install.toml. Template args may use {image} and {port}; \
                            commands are spawned argv-exact, never through a shell.")]
    Install {
        /// Target entry in install.toml, e.g. avr64ea28-updi
        #[arg(long)]
        target: String,
        /// Bootloader image handed to the tool via the {image} placeholder
        #[arg(long)]
        image: PathBuf,
        /// Programmer port, e.g. /dev/ttyACM0; fills {port} or is appended
        /// via the target's port_arg
        #[arg(long)]
        port: Option<String>,
        /// Explicit install.toml (default: ./install.toml)
        #[arg(long)]
        config: Option<PathBuf>,
    },
}

/// Resolve the flags into a transport spec — pure data in, pure data out,
/// so the mapping is testable without hardware.
fn build_spec(cli: &Cli) -> Result<TransportSpec> {
    Ok(match cli.transport {
        TransportKind::I2c => TransportSpec::I2c { bus: cli.bus.clone(), addr: cli.addr },
        TransportKind::Uart => {
            if !transport::SUPPORTED_BAUDS.contains(&cli.baud) {
                bail!(
                    "unsupported baud rate {} (supported: {})",
                    cli.baud,
                    transport::SUPPORTED_BAUDS
                        .iter()
                        .map(u32::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            TransportSpec::Uart {
                dev: cli.dev.clone().unwrap_or_else(|| "/dev/ttyUSB0".into()),
                baud: cli.baud,
            }
        }
        TransportKind::Spi => TransportSpec::Spi {
            dev: cli.dev.clone().unwrap_or_else(|| "/dev/spidev0.0".into()),
            speed_hz: cli.speed_hz,
        },
        TransportKind::Gpio => {
            let (Some(pin_tx), Some(pin_rx)) = (cli.pin_tx, cli.pin_rx) else {
                bail!(
                    "--transport gpio needs both --pin-tx and --pin-rx \
                     (GPIO line offsets on {})",
                    cli.chip
                );
            };
            if pin_tx == pin_rx {
                bail!("--pin-tx and --pin-rx must be different lines (both are {pin_tx})");
            }
            TransportSpec::Gpio { chip: cli.chip.clone(), pin_tx, pin_rx }
        }
    })
}

/// `ERASE_APP` poll attempts: enough for `app_pages` ×
/// [`PER_PAGE_ERASE_WORST_MS`] of erase at the configured poll delay,
/// unless --poll-attempts already asks for more (the user's value wins
/// when larger). Applied to the erase exchange only
/// (`DynTransport::set_erase_attempts`).
fn erase_poll_attempts(app_pages: u16, poll: PollBudget) -> u32 {
    let needed = u32::from(app_pages)
        .saturating_mul(PER_PAGE_ERASE_WORST_MS)
        .div_ceil(poll.delay_ms.max(1));
    poll.attempts.max(needed)
}

/// `install` drives a debug adapter, never the updater wire: a transport
/// flag on it means the user muddled the two paths — refuse loudly
/// instead of silently ignoring the flag.
fn reject_transport_flags(matches: &clap::ArgMatches) -> Result<()> {
    use clap::parser::ValueSource;
    const TRANSPORT_FLAGS: &[(&str, &str)] = &[
        ("transport", "--transport"),
        ("bus", "--bus"),
        ("addr", "--addr"),
        ("dev", "--dev"),
        ("baud", "--baud"),
        ("speed_hz", "--speed-hz"),
        ("chip", "--chip"),
        ("pin_tx", "--pin-tx"),
        ("pin_rx", "--pin-rx"),
        ("poll_attempts", "--poll-attempts"),
        ("poll_delay_ms", "--poll-delay-ms"),
    ];
    let sub = matches.subcommand_matches("install");
    let from_cli = |m: &clap::ArgMatches, id: &str| {
        m.value_source(id) == Some(ValueSource::CommandLine)
    };
    let offenders: Vec<&str> = TRANSPORT_FLAGS
        .iter()
        .filter(|(id, _)| {
            from_cli(matches, id) || sub.is_some_and(|m| from_cli(m, id))
        })
        .map(|&(_, flag)| flag)
        .collect();
    if offenders.is_empty() {
        Ok(())
    } else {
        bail!(
            "install talks to a debug adapter (avrdude/probe-rs/...), never the \
             updater wire — drop {}",
            offenders.join(", ")
        );
    }
}

/// One line saying where we are talking to, for progress messages.
fn describe(spec: &TransportSpec) -> String {
    match spec {
        TransportSpec::I2c { bus, addr } => format!("{bus} @ {addr:#04x}"),
        TransportSpec::Uart { dev, baud } => format!("{dev} @ {baud} baud"),
        TransportSpec::Spi { dev, speed_hz } => format!("{dev} @ {speed_hz} Hz"),
        TransportSpec::Gpio { chip, pin_tx, pin_rx } => {
            format!("{chip} tx={pin_tx} rx={pin_rx} @ 9600 baud")
        }
    }
}

fn main() -> Result<()> {
    use clap::{CommandFactory as _, FromArgMatches as _};
    // Parse through ArgMatches so explicitly-passed flags stay observable
    // (reject_transport_flags needs the value sources, not just values).
    let matches = Cli::command().get_matches();
    let cli = Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());
    // `install` talks to a debug adapter, not the updater link — bail out
    // before any transport device node is opened, and refuse transport
    // flags outright rather than silently ignoring them.
    if let Cmd::Install { target, image, port, config } = &cli.cmd {
        reject_transport_flags(&matches)?;
        return install::run_cli(target, image, port.as_deref(), config.as_deref());
    }
    // Validate purely-local inputs BEFORE opening the transport: a typo in
    // an image path or hex string must never touch the wire.
    let flash_input = match &cli.cmd {
        Cmd::Flash { image } => Some(load_image(image)?),
        _ => None,
    };
    let echo_input = match &cli.cmd {
        Cmd::Echo { data } => {
            let bytes = parse_hex_bytes(data).context("--data must be hex digits")?;
            if bytes.is_empty() || bytes.len() > 16 {
                bail!("--data must be 1..=16 bytes ({} given)", bytes.len());
            }
            Some(bytes)
        }
        _ => None,
    };
    let spec = build_spec(&cli)?;
    let poll = PollBudget { attempts: cli.poll_attempts, delay_ms: cli.poll_delay_ms };
    let transport = transport::open(&spec, poll)?;
    let mut frame_buf = [0u8; FRAME_BUF_LEN];
    let mut session = Session::new(transport, &mut frame_buf);

    match cli.cmd {
        Cmd::Info => {
            let info = session.info().context("INFO failed")?;
            let region = u32::from(info.page_size) * u32::from(info.app_pages);
            println!("protocol:           {}", info.proto);
            println!("bootloader:         v{}", info.bl_version);
            println!(
                "device id:          {:02X} {:02X} {:02X} {:02X}",
                info.device_id[0], info.device_id[1], info.device_id[2], info.device_id[3]
            );
            println!("protocol page size: {} bytes", info.page_size);
            println!("app pages:          {}", info.app_pages);
            println!(
                "app region:         {} bytes ({} usable)",
                region,
                region.saturating_sub(16)
            );
            println!("app valid:          {}", if info.app_valid { "yes" } else { "no" });
        }
        Cmd::Echo { .. } => {
            let bytes = echo_input.unwrap_or_default(); // filled above, pre-transport
            session.echo(&bytes).context("ECHO failed")?;
            println!("echo OK ({} bytes round-tripped)", bytes.len());
        }
        Cmd::Flash { .. } => {
            let binary = flash_input.unwrap_or_default(); // loaded above, pre-transport
            let info = session.info().context("INFO failed")?;
            let img =
                Image::from_bin(&binary, info.page_size, info.app_pages).with_context(|| {
                    format!(
                        "image does not fit the device ({} bytes into {} pages of {} bytes, \
                         16 reserved for the footer)",
                        binary.len(),
                        info.app_pages,
                        info.page_size
                    )
                })?;
            // ERASE_APP answers only after the device erased the whole
            // region while holding the wire: now that the geometry is
            // known, raise that one exchange's poll budget to its worst
            // case (--poll-attempts wins when larger).
            let mut transport = session.into_transport();
            transport.set_erase_attempts(erase_poll_attempts(info.app_pages, poll));
            let mut session = Session::new(transport, &mut frame_buf);
            eprintln!(
                "flashing {} bytes (crc32 {:08X}) to {}",
                img.len(),
                img.crc32(),
                describe(&spec)
            );
            session
                .flash(&img, &mut |done, total| {
                    eprint!("\r  page {done}/{total}");
                    let _ = std::io::stderr().flush();
                })
                .context("flash failed")?;
            eprintln!();
            println!("flash OK: {} bytes written and verified", img.len());
        }
        Cmd::Boot => match session.boot() {
            Ok(()) => println!("boot: OK"),
            // The final ACK is inherently lossy (Session::boot docs): wire
            // silence here does not prove the boot failed — but it is not
            // proof of success either, so the exit code stays nonzero.
            Err(updater_core::Error::Transport(e)) => bail!(
                "no BOOT ACK — the device may have booted anyway; check \
                 whether the app came up ({e})"
            ),
            // Any device-status refusal (ST_NO_APP: boot gate rejected the
            // image) really is a refusal.
            Err(e) => return Err(e).context("BOOT refused"),
        },
        // Dispatched before the transport was opened; a session-borne arm
        // here would imply install needs the updater link, which it never does.
        Cmd::Install { .. } => unreachable!("install returns before transport setup"),
    }
    Ok(())
}

/// Load `.hex` (by extension, case-insensitive) as Intel HEX; anything
/// else is passed through as a raw binary.
fn load_image(path: &Path) -> Result<Vec<u8>> {
    let is_hex = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("hex"));
    if is_hex {
        let text =
            fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        ihex::parse(&text).with_context(|| format!("parsing {} as Intel HEX", path.display()))
    } else {
        fs::read(path).with_context(|| format!("reading {}", path.display()))
    }
}

fn parse_addr(s: &str) -> Result<u16, String> {
    let value = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .map_or_else(|| s.parse::<u16>(), |hex| u16::from_str_radix(hex, 16))
        .map_err(|_| format!("not a number: {s:?}"))?;
    if value > 0x7F {
        return Err(format!("{value:#04x} is not a 7-bit I2C address"));
    }
    Ok(value)
}

fn parse_hex_bytes(s: &str) -> Result<Vec<u8>> {
    let clean: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if clean.len() % 2 != 0 {
        bail!("odd number of hex digits");
    }
    clean
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let text = std::str::from_utf8(pair)?;
            u8::from_str_radix(text, 16).with_context(|| format!("invalid hex byte {text:?}"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).expect("flags must parse")
    }

    #[test]
    fn cli_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn help_names_every_surface() {
        let help = Cli::command().render_long_help().to_string();
        for needle in [
            "info", "echo", "flash", "boot", "--transport", "--bus", "--addr", "/dev/i2c-1",
            "0x20", "--dev", "--baud", "--speed-hz", "--chip", "--pin-tx", "--pin-rx",
            "--poll-attempts", "--poll-delay-ms", "i2c", "uart", "spi", "gpio", "install",
            // declared defaults for --dev (no clap default: build_spec applies them)
            "/dev/ttyUSB0", "/dev/spidev0.0",
        ] {
            assert!(help.contains(needle), "--help lost {needle:?}:\n{help}");
        }
    }

    #[test]
    fn install_help_documents_flags_and_search_order() {
        let mut cmd = Cli::command();
        // Globals reach subcommands only once the command is built; without
        // this the rendered help is not what the binary prints.
        cmd.build();
        let install = cmd.find_subcommand_mut("install").expect("install must exist");
        let help = install.render_long_help().to_string();
        for needle in [
            "--target", "--image", "--port", "--config", "install.toml", "{image}", "{port}",
            "argv-exact",
        ] {
            assert!(help.contains(needle), "install --help lost {needle:?}:\n{help}");
        }
        // The inherited transport globals must sit in their own labeled
        // section, after install's own flags — install never opens the wire.
        let heading = help.find("Transport (info/echo/flash/boot)").expect("heading missing");
        for own in ["--target", "--image", "--config"] {
            assert!(
                help.find(own).unwrap() < heading,
                "{own} must be listed before the transport section:\n{help}"
            );
        }
    }

    #[test]
    fn addr_parser_accepts_hex_and_decimal_rejects_wide() {
        assert_eq!(parse_addr("0x20").unwrap(), 0x20);
        assert_eq!(parse_addr("32").unwrap(), 32);
        assert!(parse_addr("0x80").is_err());
        assert!(parse_addr("nope").is_err());
    }

    #[test]
    fn hex_bytes_parser() {
        assert_eq!(parse_hex_bytes("DEad be ef").unwrap(), vec![0xDE, 0xAD, 0xBE, 0xEF]);
        assert!(parse_hex_bytes("ABC").is_err());
        assert!(parse_hex_bytes("ZZ").is_err());
    }

    // -- arg -> spec mapping (the part that must work without hardware) --

    #[test]
    fn default_spec_is_i2c_with_bus_and_addr() {
        let cli = parse(&["updater-cli", "info"]);
        assert_eq!(
            build_spec(&cli).unwrap(),
            TransportSpec::I2c { bus: "/dev/i2c-1".into(), addr: 0x20 }
        );
        assert_eq!((cli.poll_attempts, cli.poll_delay_ms), (100, 10));
    }

    // -- poll budget resolution (finding B1: erase budget) -----------------

    #[test]
    fn erase_attempts_scale_with_the_device_geometry() {
        let base = PollBudget { attempts: 100, delay_ms: 10 };
        // AVR64EA28: 480 pages x 10 ms / 10 ms poll = 480 attempts (~5 s).
        assert_eq!(erase_poll_attempts(480, base), 480);
        // RP2350: 8192 pages -> 8192 attempts x 10 ms ~= 82 s.
        assert_eq!(erase_poll_attempts(8192, base), 8192);
        // Rounding is upward: 480 pages x 10 ms at 7 ms polls = ceil(685.7).
        assert_eq!(erase_poll_attempts(480, PollBudget { attempts: 1, delay_ms: 7 }), 686);
        // A zero delay must not divide by zero.
        assert_eq!(erase_poll_attempts(480, PollBudget { attempts: 1, delay_ms: 0 }), 4800);
    }

    #[test]
    fn user_poll_attempts_win_when_larger() {
        // rp2350 PORT_AUDIT note 2 suggests 12000 x 10 ms for worst-case
        // dirty flash — the explicit flag must not be scaled down.
        let base = PollBudget { attempts: 12_000, delay_ms: 10 };
        assert_eq!(erase_poll_attempts(8192, base), 12_000);
    }

    #[test]
    fn flash_help_names_the_erase_budget_rule() {
        let mut cmd = Cli::command();
        cmd.build();
        let flash = cmd.find_subcommand_mut("flash").expect("flash must exist");
        let help = flash.render_long_help().to_string();
        assert!(help.contains("ERASE_APP"), "must name the exchange: {help}");
        assert!(help.contains("10 ms"), "must state the per-page constant: {help}");
        assert!(help.contains("--poll-attempts"), "must name the override: {help}");
    }

    // -- install x transport flags -----------------------------------------

    #[test]
    fn install_rejects_transport_flags_wherever_they_sit() {
        let before: &[&str] = &[
            "updater-cli", "--transport", "uart", "install", "--target", "t", "--image", "x",
        ];
        let after: &[&str] =
            &["updater-cli", "install", "--bus", "/dev/i2c-9", "--target", "t", "--image", "x"];
        for (args, flag) in [(before, "--transport"), (after, "--bus")] {
            let m = Cli::command().try_get_matches_from(args).expect("must parse");
            let err = reject_transport_flags(&m).unwrap_err().to_string();
            assert!(err.contains(flag), "must name the offender {flag}: {err}");
            assert!(err.contains("debug adapter"), "must explain why: {err}");
        }
    }

    #[test]
    fn install_without_transport_flags_passes_the_gate() {
        let m = Cli::command()
            .try_get_matches_from(["updater-cli", "install", "--target", "t", "--image", "x"])
            .expect("must parse");
        assert!(reject_transport_flags(&m).is_ok());
    }

    #[test]
    fn i2c_spec_honors_bus_and_addr_flags() {
        let cli = parse(&["updater-cli", "--bus", "/dev/i2c-7", "--addr", "0x11", "info"]);
        assert_eq!(
            build_spec(&cli).unwrap(),
            TransportSpec::I2c { bus: "/dev/i2c-7".into(), addr: 0x11 }
        );
    }

    #[test]
    fn uart_spec_defaults_dev_and_baud() {
        let cli = parse(&["updater-cli", "--transport", "uart", "info"]);
        assert_eq!(
            build_spec(&cli).unwrap(),
            TransportSpec::Uart { dev: "/dev/ttyUSB0".into(), baud: 115_200 }
        );
    }

    #[test]
    fn uart_spec_honors_dev_and_baud() {
        let cli = parse(&[
            "updater-cli", "--transport", "uart", "--dev", "/dev/ttyACM3", "--baud", "57600",
            "info",
        ]);
        assert_eq!(
            build_spec(&cli).unwrap(),
            TransportSpec::Uart { dev: "/dev/ttyACM3".into(), baud: 57_600 }
        );
    }

    #[test]
    fn uart_spec_rejects_unsupported_baud_naming_the_supported_set() {
        let cli = parse(&["updater-cli", "--transport", "uart", "--baud", "12345", "info"]);
        let err = build_spec(&cli).unwrap_err().to_string();
        assert!(err.contains("12345"), "message must name the offender: {err}");
        assert!(err.contains("115200"), "message must list supported rates: {err}");
    }

    #[test]
    fn spi_spec_defaults_dev_and_speed() {
        let cli = parse(&["updater-cli", "--transport", "spi", "info"]);
        assert_eq!(
            build_spec(&cli).unwrap(),
            TransportSpec::Spi { dev: "/dev/spidev0.0".into(), speed_hz: 100_000 }
        );
    }

    #[test]
    fn spi_spec_honors_dev_and_speed() {
        let cli = parse(&[
            "updater-cli", "--transport", "spi", "--dev", "/dev/spidev1.2", "--speed-hz",
            "250000", "info",
        ]);
        assert_eq!(
            build_spec(&cli).unwrap(),
            TransportSpec::Spi { dev: "/dev/spidev1.2".into(), speed_hz: 250_000 }
        );
    }

    #[test]
    fn gpio_spec_requires_both_pins() {
        let cli = parse(&["updater-cli", "--transport", "gpio", "info"]);
        let err = build_spec(&cli).unwrap_err().to_string();
        assert!(err.contains("--pin-tx"), "must tell the user what is missing: {err}");

        let cli = parse(&["updater-cli", "--transport", "gpio", "--pin-tx", "4", "info"]);
        let err = build_spec(&cli).unwrap_err().to_string();
        assert!(err.contains("--pin-rx"), "must name the missing pin: {err}");
    }

    #[test]
    fn gpio_spec_rejects_equal_pins() {
        let cli = parse(&[
            "updater-cli", "--transport", "gpio", "--pin-tx", "4", "--pin-rx", "4", "info",
        ]);
        assert!(build_spec(&cli).is_err(), "tx and rx on one line cannot work");
    }

    #[test]
    fn gpio_spec_builds_with_chip_and_pins() {
        let cli = parse(&[
            "updater-cli", "--transport", "gpio", "--chip", "/dev/gpiochip2", "--pin-tx", "17",
            "--pin-rx", "27", "info",
        ]);
        assert_eq!(
            build_spec(&cli).unwrap(),
            TransportSpec::Gpio { chip: "/dev/gpiochip2".into(), pin_tx: 17, pin_rx: 27 }
        );
    }

    #[test]
    fn describe_names_the_wire() {
        assert_eq!(
            describe(&TransportSpec::I2c { bus: "/dev/i2c-1".into(), addr: 0x20 }),
            "/dev/i2c-1 @ 0x20"
        );
        assert!(describe(&TransportSpec::Uart { dev: "/dev/ttyUSB0".into(), baud: 115_200 })
            .contains("115200"));
    }
}
