//! Minimal Intel-HEX parser: `:llaaaatt[dd..]cc`, record types 00 (data)
//! and 01 (EOF) only, checksum verified, extended-address records rejected
//! with the offending line number.
//!
//! Deliberately not a general ihex library: the updater's app region lives
//! at offset 0 of a 16-bit address space (images are < 64 KiB by
//! construction), so anything needing extended addressing is a wrong file,
//! and saying so precisely beats relocating it silently. Gaps between data
//! records are filled with `0xFF` — the erased-flash value, which the
//! image layer then skips page-wise.

use anyhow::{anyhow, bail, Result};

/// Parse Intel-HEX `text` into a raw binary starting at address 0.
pub fn parse(text: &str) -> Result<Vec<u8>> {
    let mut out: Vec<u8> = Vec::new();
    let mut saw_eof = false;

    for (idx, raw_line) in text.lines().enumerate() {
        let lineno = idx + 1;
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if saw_eof {
            bail!("line {lineno}: record after EOF record");
        }
        let Some(hex) = line.strip_prefix(':') else {
            bail!("line {lineno}: record does not start with ':'");
        };
        let bytes = decode_hex(hex).map_err(|e| anyhow!("line {lineno}: {e}"))?;

        // ll aaaa tt [dd..] cc
        let [ll, addr_hi, addr_lo, rtype, rest @ ..] = &bytes[..] else {
            bail!("line {lineno}: record too short ({} bytes, need at least 5)", bytes.len());
        };
        let Some((_cc, data)) = rest.split_last() else {
            bail!("line {lineno}: record too short (missing checksum)");
        };
        if data.len() != usize::from(*ll) {
            bail!(
                "line {lineno}: length field says {ll} data byte(s) but record carries {}",
                data.len()
            );
        }
        let sum = bytes.iter().fold(0u8, |s, &b| s.wrapping_add(b));
        if sum != 0 {
            bail!("line {lineno}: checksum mismatch");
        }

        let addr = usize::from(u16::from_be_bytes([*addr_hi, *addr_lo]));
        match rtype {
            0x00 => {
                let end = addr + data.len(); // <= 0xFFFF + 0xFF
                if out.len() < end {
                    out.resize(end, 0xFF); // gap filler = erased-flash value
                }
                out[addr..end].copy_from_slice(data);
            }
            0x01 => saw_eof = true,
            0x02 | 0x04 => bail!(
                "line {lineno}: extended address record (type {rtype:#04x}) is not supported \
                 — the updater image must fit 16-bit addresses starting at 0"
            ),
            other => bail!("line {lineno}: unsupported record type {other:#04x}"),
        }
    }

    if !saw_eof {
        bail!("missing EOF record (:00000001FF)");
    }
    Ok(out)
}

fn decode_hex(s: &str) -> Result<Vec<u8>> {
    if s.len() % 2 != 0 {
        bail!("odd number of hex digits");
    }
    s.as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let text = std::str::from_utf8(pair)?; // chunk boundaries are ASCII-checked next
            u8::from_str_radix(text, 16).map_err(|_| anyhow!("invalid hex digits {text:?}"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::parse;

    const GOLDEN: &str = ":04000000DEADBEEFC4\n:00000001FF\n";

    #[test]
    fn golden_tiny_hex() {
        assert_eq!(parse(GOLDEN).unwrap(), vec![0xDE, 0xAD, 0xBE, 0xEF]);
    }

    #[test]
    fn gaps_are_filled_with_ff_and_records_may_be_unordered() {
        // 2 bytes at 0x0004, then 2 bytes at 0x0000.
        let text = ":020004001122C7\n:02000000334487\n:00000001FF\n";
        let out = parse(text).unwrap();
        assert_eq!(out, vec![0x33, 0x44, 0xFF, 0xFF, 0x11, 0x22]);
    }

    #[test]
    fn bad_checksum_names_the_line() {
        let text = ":04000000DEADBEEFC5\n:00000001FF\n";
        let err = format!("{:#}", parse(text).unwrap_err());
        assert!(err.contains("checksum"), "{err}");
        assert!(err.contains("line 1"), "{err}");
    }

    #[test]
    fn extended_address_records_are_rejected_with_line_number() {
        for (rec, ty) in [(":020000021000EC", "0x02"), (":020000040800F2", "0x04")] {
            let text = format!(":04000000DEADBEEFC4\n{rec}\n:00000001FF\n");
            let err = format!("{:#}", parse(&text).unwrap_err());
            assert!(err.contains("extended address"), "{err}");
            assert!(err.contains("line 2"), "{err}");
            assert!(err.contains(ty), "{err}");
        }
    }

    #[test]
    fn other_record_types_are_rejected() {
        // Type 03 (start segment address), checksum-correct.
        let text = ":0400000300003800C1\n:00000001FF\n";
        let err = format!("{:#}", parse(text).unwrap_err());
        assert!(err.contains("line 1"), "{err}");
        assert!(err.contains("0x03"), "{err}");
    }

    #[test]
    fn structural_defects_are_rejected() {
        // Missing EOF record.
        assert!(parse(":04000000DEADBEEFC4\n").is_err());
        // Data after EOF.
        assert!(parse(":00000001FF\n:04000000DEADBEEFC4\n").is_err());
        // Missing colon.
        assert!(parse("04000000DEADBEEFC4\n:00000001FF\n").is_err());
        // Odd number of hex digits.
        assert!(parse(":04000000DEADBEEFC\n:00000001FF\n").is_err());
        // Non-hex characters.
        assert!(parse(":04000000DEADBEEFZZ\n:00000001FF\n").is_err());
        // Record shorter than ll aaaa tt cc.
        assert!(parse(":0400\n:00000001FF\n").is_err());
        // Length byte inconsistent with record size.
        assert!(parse(":05000000DEADBEEFC3\n:00000001FF\n").is_err());
        // Empty input.
        assert!(parse("").is_err());
    }

    #[test]
    fn blank_lines_and_crlf_are_tolerated() {
        let text = ":04000000DEADBEEFC4\r\n\r\n:00000001FF\r\n";
        assert_eq!(parse(text).unwrap(), vec![0xDE, 0xAD, 0xBE, 0xEF]);
    }
}
