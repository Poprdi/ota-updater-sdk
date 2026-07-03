//! `updater-cli` — drive the updater bootloader from a Linux master.
//!
//! `anyhow` lives only here, at the binary layer; everything below speaks
//! typed errors.

mod ihex;
mod transport;

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use updater_core::image::Image;
use updater_core::Session;

use transport::LinuxI2c;

/// Big enough for the largest exchange of any legal geometry:
/// worst request (page 250 + 2 + overhead 3) + worst response (12 + 3).
const FRAME_BUF_LEN: usize = 512;

#[derive(Parser)]
#[command(
    name = "updater-cli",
    version,
    about = "Flash and manage devices running the updater bootloader over I2C"
)]
struct Cli {
    /// I2C bus device node
    #[arg(long, global = true, default_value = "/dev/i2c-1")]
    bus: String,

    /// 7-bit device address (decimal or 0x-prefixed hex)
    #[arg(long, global = true, default_value = "0x20", value_parser = parse_addr)]
    addr: u16,

    #[command(subcommand)]
    cmd: Cmd,
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
    Flash {
        /// Image file; parsed as Intel HEX iff the extension is .hex
        image: PathBuf,
    },
    /// Ask the device to boot the application
    Boot,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let transport = LinuxI2c::open(&cli.bus, cli.addr)
        .with_context(|| format!("opening {} (address {:#04x})", cli.bus, cli.addr))?;
    let mut frame_buf = [0u8; FRAME_BUF_LEN];
    let mut session = Session::new(transport, &mut frame_buf);

    match cli.cmd {
        Cmd::Info => {
            let info = session.info().context("INFO failed")?;
            let region = u32::from(info.page_size) * u32::from(info.app_pages);
            println!("protocol:   {}", info.proto);
            println!("bootloader: v{}", info.bl_version);
            println!(
                "device id:  {:02X} {:02X} {:02X} {:02X}",
                info.device_id[0], info.device_id[1], info.device_id[2], info.device_id[3]
            );
            println!("page size:  {} bytes", info.page_size);
            println!("app pages:  {}", info.app_pages);
            println!("app region: {} bytes ({} usable)", region, region.saturating_sub(16));
            println!("app valid:  {}", if info.app_valid { "yes" } else { "no" });
        }
        Cmd::Echo { data } => {
            let bytes = parse_hex_bytes(&data).context("--data must be hex digits")?;
            if bytes.is_empty() || bytes.len() > 16 {
                bail!("--data must be 1..=16 bytes ({} given)", bytes.len());
            }
            session.echo(&bytes).context("ECHO failed")?;
            println!("echo OK ({} bytes round-tripped)", bytes.len());
        }
        Cmd::Flash { image } => {
            let binary = load_image(&image)?;
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
            eprintln!(
                "flashing {} bytes (crc32 {:08X}) to {} @ {:#04x}",
                img.len(),
                img.crc32(),
                cli.bus,
                cli.addr
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
        Cmd::Boot => {
            session.boot().context("BOOT refused")?;
            println!("boot: OK");
        }
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

    #[test]
    fn cli_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn help_names_every_surface() {
        let help = Cli::command().render_long_help().to_string();
        for needle in ["info", "echo", "flash", "boot", "--bus", "--addr", "/dev/i2c-1", "0x20"] {
            assert!(help.contains(needle), "--help lost {needle:?}:\n{help}");
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
}
