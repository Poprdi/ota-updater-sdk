//! `updater_core::Transport` over Linux `/dev/i2c-*`.
//!
//! One request is a plain write followed by a fixed-length poll-read:
//! i2c-dev cannot split a single read transaction, and the master must
//! pick the read size before the device has said how long its response is
//! (error responses are shorter than success responses). So the whole
//! `rsp` window is read in ONE transaction and the device pads the tail
//! with `0xFF` idle bytes — the session trims via
//! `updater_core::frame::decode_padded`. While the device is erasing or
//! burning a page it NACKs its address; the read is retried with a short
//! sleep, bounded in total.

use std::fmt;
use std::thread;
use std::time::Duration;

use i2cdev::core::I2CDevice;
use i2cdev::linux::{LinuxI2CDevice, LinuxI2CError};
use updater_core::Transport;

/// Poll budget: 100 attempts x 10 ms = a one-second window, enough for a
/// full-region erase on AVR-class flash with headroom.
const POLL_ATTEMPTS: u32 = 100;
const POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Debug)]
pub enum LinuxI2cError {
    Open(LinuxI2CError),
    Write(LinuxI2CError),
    Exhausted { attempts: u32, last: LinuxI2CError },
}

impl fmt::Display for LinuxI2cError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Open(e) => write!(f, "cannot open i2c device: {e}"),
            Self::Write(e) => write!(f, "i2c write failed: {e}"),
            Self::Exhausted { attempts, last } => write!(
                f,
                "device did not answer within {attempts} poll attempts \
                 (is it in the bootloader, at this address?): {last}"
            ),
        }
    }
}

impl std::error::Error for LinuxI2cError {}

pub struct LinuxI2c {
    dev: LinuxI2CDevice,
}

impl LinuxI2c {
    pub fn open(bus: &str, addr: u16) -> Result<Self, LinuxI2cError> {
        LinuxI2CDevice::new(bus, addr)
            .map(|dev| Self { dev })
            .map_err(LinuxI2cError::Open)
    }
}

impl Transport for LinuxI2c {
    type Err = LinuxI2cError;

    fn request(&mut self, req: &[u8], rsp: &mut [u8]) -> Result<usize, Self::Err> {
        self.dev.write(req).map_err(LinuxI2cError::Write)?;
        let mut attempt: u32 = 0;
        loop {
            attempt += 1;
            match self.dev.read(rsp) {
                Ok(()) => return Ok(rsp.len()),
                Err(last) => {
                    if attempt >= POLL_ATTEMPTS {
                        return Err(LinuxI2cError::Exhausted { attempts: attempt, last });
                    }
                    thread::sleep(POLL_INTERVAL);
                }
            }
        }
    }
}
