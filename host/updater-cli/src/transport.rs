//! Linux backends for `updater_core::Transport`, behind the CLI's
//! `--transport` flag.
//!
//! * `i2c`  — `/dev/i2c-*` via i2cdev: transactional write + fixed-length
//!   poll-read, the device padding the tail with `0xFF` idle bytes.
//! * `uart` — a serial device in raw termios mode (`VMIN=0`/`VTIME=1`, so
//!   an idle line reads as `Ok(0)` and becomes one poll attempt), driven
//!   by `updater_eh::UartTransport`.
//! * `spi`  — `/dev/spidev*` via the spidev crate, wrapped as an
//!   `embedded_hal::spi::SpiDevice` and driven by
//!   `updater_eh::SpiTransport`.
//! * `gpio` — two GPIO character-device lines via gpio-cdev, driven by
//!   `updater_eh::SoftUartTransport` (bit-banged 9600 8N1). Userspace
//!   scheduling makes the bit timing marginal; expect retries, prefer a
//!   real UART when one exists.
//!
//! The stream backends reuse the exact `updater-eh` adapters an embedded
//! consumer wires up — this file only implements the embedded-hal /
//! embedded-io traits over the Linux device nodes and erases the
//! per-backend error types behind [`DynTransport`].
//!
//! [`DynTransport`] also owns the poll-budget policy: every exchange runs
//! with the base budget from the CLI flags, except `ERASE_APP`, which may
//! run with a raised attempt count ([`DynTransport::set_erase_attempts`])
//! — the one exchange whose wait scales with the whole app region.

use std::fmt;
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Context};
use embedded_hal::delay::DelayNs;
use i2cdev::core::I2CDevice;
use i2cdev::linux::{LinuxI2CDevice, LinuxI2CError};
use updater_core::frame::CMD_ERASE_APP;
use updater_core::Transport;
use updater_eh::{SoftUartTransport, SpiTransport, UartTransport};

/// Response poll budget, straight from `--poll-attempts`/`--poll-delay-ms`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PollBudget {
    pub attempts: u32,
    pub delay_ms: u32,
}

/// Which wire to open, fully resolved from the CLI flags (pure data — the
/// arg-to-spec mapping is unit-tested without hardware).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportSpec {
    I2c { bus: String, addr: u16 },
    Uart { dev: String, baud: u32 },
    Spi { dev: String, speed_hz: u32 },
    Gpio { chip: String, pin_tx: u32, pin_rx: u32 },
}

/// Baud rates the raw-termios backend can program.
pub const SUPPORTED_BAUDS: &[u32] = &[9_600, 19_200, 38_400, 57_600, 115_200, 230_400];

/// Open the wire a spec describes; every exchange starts on the `poll`
/// budget (the ERASE_APP budget can be raised later, once geometry is
/// known — [`DynTransport::set_erase_attempts`]).
pub fn open(spec: &TransportSpec, poll: PollBudget) -> anyhow::Result<DynTransport> {
    let inner: Box<dyn PollTransport> = match spec {
        TransportSpec::I2c { bus, addr } => {
            let t = LinuxI2c::open(bus, *addr)
                .with_context(|| format!("opening {bus} (address {addr:#04x})"))?;
            Box::new(Erased(t))
        }
        TransportSpec::Uart { dev, baud } => {
            Box::new(Erased(UartTransport::new(serial::open(dev, *baud)?, SpinDelay)))
        }
        TransportSpec::Spi { dev, speed_hz } => {
            Box::new(Erased(SpiTransport::new(spi::open(dev, *speed_hz)?, SpinDelay)))
        }
        TransportSpec::Gpio { chip, pin_tx, pin_rx } => {
            let (tx, rx) = gpio::open(chip, *pin_tx, *pin_rx)?;
            Box::new(Erased(SoftUartTransport::new(tx, rx, SpinDelay)))
        }
    };
    let mut inner = inner;
    inner.set_poll_budget(poll);
    Ok(DynTransport { inner, base: poll, erase_attempts: poll.attempts })
}

// ---------------------------------------------------------------------------
// type erasure: one Session type over any backend
// ---------------------------------------------------------------------------

/// A [`Transport`] over whichever backend `--transport` selected.
///
/// Owns the poll-budget policy: the base budget applies to every exchange;
/// `ERASE_APP` requests alone run with the (possibly raised) erase attempt
/// count, then the base budget is restored — so a slow full-region erase
/// gets its long wait without making every lost frame elsewhere take as
/// long to diagnose.
pub struct DynTransport {
    inner: Box<dyn PollTransport>,
    base: PollBudget,
    erase_attempts: u32,
}

impl DynTransport {
    /// Raise the poll-attempt count used for `ERASE_APP` exchanges only
    /// (never lowered below the base budget). The CLI computes this from
    /// the geometry INFO reports, once it has it.
    pub fn set_erase_attempts(&mut self, attempts: u32) {
        self.erase_attempts = attempts.max(self.base.attempts);
    }
}

impl Transport for DynTransport {
    type Err = anyhow::Error;

    fn request(&mut self, req: &[u8], rsp: &mut [u8]) -> Result<usize, Self::Err> {
        // The command byte leads every request frame, so this match is
        // exact, not heuristic.
        let erase =
            req.first() == Some(&CMD_ERASE_APP) && self.erase_attempts > self.base.attempts;
        if erase {
            self.inner.set_poll_budget(PollBudget {
                attempts: self.erase_attempts,
                delay_ms: self.base.delay_ms,
            });
        }
        let out = self.inner.request(req, rsp);
        if erase {
            self.inner.set_poll_budget(self.base);
        }
        out
    }
}

/// The object-safe seam [`DynTransport`] drives: a transport whose poll
/// budget can be re-tuned between exchanges.
trait PollTransport: Transport<Err = anyhow::Error> {
    fn set_poll_budget(&mut self, poll: PollBudget);
}

/// How a concrete backend consumes a CLI poll budget
/// (`attempts` × `delay_ms`).
trait SetPoll {
    fn set_poll_budget(&mut self, poll: PollBudget);
}

impl SetPoll for LinuxI2c {
    fn set_poll_budget(&mut self, poll: PollBudget) {
        self.poll_attempts = poll.attempts.max(1);
        self.poll_interval = Duration::from_millis(u64::from(poll.delay_ms));
    }
}

impl<T, D> SetPoll for UartTransport<T, D> {
    fn set_poll_budget(&mut self, poll: PollBudget) {
        self.set_poll(poll.attempts, poll.delay_ms.saturating_mul(1_000_000));
    }
}

impl<S, D> SetPoll for SpiTransport<S, D> {
    fn set_poll_budget(&mut self, poll: PollBudget) {
        self.set_poll(poll.attempts, poll.delay_ms.saturating_mul(1_000_000));
    }
}

impl<Tx, Rx, D> SetPoll for SoftUartTransport<Tx, Rx, D> {
    /// The softuart poll interval is RX sampling granularity (it must stay
    /// far below half a bit time), so the CLI's budget is converted:
    /// attempts × delay = total wait, sampled at the adapter's default
    /// interval.
    fn set_poll_budget(&mut self, poll: PollBudget) {
        let total_ns = u64::from(poll.attempts.max(1))
            .saturating_mul(u64::from(poll.delay_ms))
            .saturating_mul(1_000_000);
        let interval = updater_eh::DEFAULT_SOFTUART_POLL_INTERVAL_NS;
        let samples = u32::try_from(total_ns / u64::from(interval.max(1))).unwrap_or(u32::MAX);
        self.set_poll(samples.max(1), interval);
    }
}

/// Maps a concrete transport's typed error into `anyhow` at the CLI
/// boundary; the message is the typed error's own `Display`.
struct Erased<T>(T);

impl<T: Transport> Transport for Erased<T>
where
    T::Err: fmt::Display,
{
    type Err = anyhow::Error;

    fn request(&mut self, req: &[u8], rsp: &mut [u8]) -> Result<usize, Self::Err> {
        self.0.request(req, rsp).map_err(|e| anyhow!("{e}"))
    }
}

impl<T> PollTransport for Erased<T>
where
    T: Transport + SetPoll,
    T::Err: fmt::Display,
{
    fn set_poll_budget(&mut self, poll: PollBudget) {
        self.0.set_poll_budget(poll);
    }
}

// ---------------------------------------------------------------------------
// delay: ns-accurate enough for bit-banging, polite for ms-scale polls
// ---------------------------------------------------------------------------

/// `DelayNs` for Linux userspace: sub-millisecond delays spin on
/// `Instant` (an OS sleep's wakeup jitter would wreck softuart bit
/// timing), millisecond-scale delays sleep.
struct SpinDelay;

impl DelayNs for SpinDelay {
    fn delay_ns(&mut self, ns: u32) {
        if ns >= 1_000_000 {
            thread::sleep(Duration::from_nanos(u64::from(ns)));
            return;
        }
        let dur = Duration::from_nanos(u64::from(ns));
        let start = std::time::Instant::now();
        while start.elapsed() < dur {
            std::hint::spin_loop();
        }
    }
}

// ---------------------------------------------------------------------------
// i2c backend (i2cdev)
// ---------------------------------------------------------------------------

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
                 (is it in the bootloader, at this address? if this was \
                 erase/flash, the device may still be erasing — raise \
                 --poll-attempts): {last}"
            ),
        }
    }
}

impl std::error::Error for LinuxI2cError {}

pub struct LinuxI2c {
    dev: LinuxI2CDevice,
    poll_attempts: u32,
    poll_interval: Duration,
}

impl LinuxI2c {
    /// Open the bus; the poll budget is applied afterwards through
    /// [`SetPoll`] (a fresh handle starts on a 1 × 10 ms placeholder).
    pub fn open(bus: &str, addr: u16) -> Result<Self, LinuxI2cError> {
        LinuxI2CDevice::new(bus, addr)
            .map(|dev| Self {
                dev,
                poll_attempts: 1,
                poll_interval: Duration::from_millis(10),
            })
            .map_err(LinuxI2cError::Open)
    }
}

impl Transport for LinuxI2c {
    type Err = LinuxI2cError;

    /// One request: plain write, then a fixed-length poll-read of all of
    /// `rsp` (i2c-dev cannot split a read transaction and the master must
    /// pick the size before the device says how long its answer is; the
    /// device pads with `0xFF`, the session trims via `decode_padded`).
    /// A busy device NACKs its address; the read is retried, bounded.
    fn request(&mut self, req: &[u8], rsp: &mut [u8]) -> Result<usize, Self::Err> {
        self.dev.write(req).map_err(LinuxI2cError::Write)?;
        let mut attempt: u32 = 0;
        loop {
            attempt += 1;
            match self.dev.read(rsp) {
                Ok(()) => return Ok(rsp.len()),
                Err(last) => {
                    if attempt >= self.poll_attempts {
                        return Err(LinuxI2cError::Exhausted { attempts: attempt, last });
                    }
                    thread::sleep(self.poll_interval);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// uart backend (raw termios via nix)
// ---------------------------------------------------------------------------

mod serial {
    use std::fs::{File, OpenOptions};
    use std::io::{Read as _, Write as _};

    use anyhow::Context;
    use nix::sys::termios::{
        cfmakeraw, cfsetspeed, tcflush, tcgetattr, tcsetattr, BaudRate, FlushArg, SetArg,
        SpecialCharacterIndices,
    };

    use super::SUPPORTED_BAUDS;

    /// A serial device in raw mode. `VMIN=0`/`VTIME=1`: a read blocks at
    /// most 100 ms and returns `Ok(0)` when nothing arrived — exactly the
    /// "one poll attempt" contract `updater_eh::UartTransport` expects.
    pub struct SerialPort {
        file: File,
    }

    fn baud_constant(baud: u32) -> Option<BaudRate> {
        Some(match baud {
            9_600 => BaudRate::B9600,
            19_200 => BaudRate::B19200,
            38_400 => BaudRate::B38400,
            57_600 => BaudRate::B57600,
            115_200 => BaudRate::B115200,
            230_400 => BaudRate::B230400,
            _ => return None,
        })
    }

    pub fn open(dev: &str, baud: u32) -> anyhow::Result<SerialPort> {
        let rate = baud_constant(baud).ok_or_else(|| {
            anyhow::anyhow!("unsupported baud rate {baud} (supported: {SUPPORTED_BAUDS:?})")
        })?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(dev)
            .with_context(|| format!("opening serial device {dev}"))?;
        let mut tio = tcgetattr(&file).with_context(|| format!("{dev}: not a terminal?"))?;
        cfmakeraw(&mut tio);
        cfsetspeed(&mut tio, rate).context("setting baud rate")?;
        tio.control_chars[SpecialCharacterIndices::VMIN as usize] = 0;
        tio.control_chars[SpecialCharacterIndices::VTIME as usize] = 1; // deciseconds
        tcsetattr(&file, SetArg::TCSANOW, &tio).context("applying termios settings")?;
        tcflush(&file, FlushArg::TCIOFLUSH).context("flushing stale bytes")?;
        Ok(SerialPort { file })
    }

    impl embedded_io::ErrorType for SerialPort {
        type Error = std::io::Error;
    }

    impl embedded_io::Read for SerialPort {
        fn read(&mut self, buf: &mut [u8]) -> Result<usize, std::io::Error> {
            loop {
                match self.file.read(buf) {
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    other => return other,
                }
            }
        }
    }

    impl embedded_io::Write for SerialPort {
        fn write(&mut self, buf: &[u8]) -> Result<usize, std::io::Error> {
            loop {
                match self.file.write(buf) {
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    other => return other,
                }
            }
        }

        fn flush(&mut self) -> Result<(), std::io::Error> {
            self.file.flush()
        }
    }
}

// ---------------------------------------------------------------------------
// spi backend (spidev)
// ---------------------------------------------------------------------------

mod spi {
    use std::fmt;

    use anyhow::Context;
    use embedded_hal::spi::{ErrorType, Operation, SpiDevice};
    use spidev::{SpiModeFlags, Spidev, SpidevOptions, SpidevTransfer};

    #[derive(Debug)]
    pub enum SpiError {
        Io(std::io::Error),
        Unsupported(&'static str),
    }

    impl fmt::Display for SpiError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::Io(e) => write!(f, "spidev transfer failed: {e}"),
                Self::Unsupported(what) => write!(f, "spidev backend cannot do this: {what}"),
            }
        }
    }

    impl std::error::Error for SpiError {}

    impl embedded_hal::spi::Error for SpiError {
        fn kind(&self) -> embedded_hal::spi::ErrorKind {
            embedded_hal::spi::ErrorKind::Other
        }
    }

    /// `/dev/spidev*` as an `SpiDevice`: the kernel owns the chip select,
    /// asserted across one `transfer_multiple` (= one transaction).
    pub struct SpidevDevice {
        dev: Spidev,
    }

    pub fn open(dev: &str, speed_hz: u32) -> anyhow::Result<SpidevDevice> {
        let mut spi = Spidev::open(dev).with_context(|| format!("opening SPI device {dev}"))?;
        spi.configure(
            &SpidevOptions::new()
                .bits_per_word(8)
                .max_speed_hz(speed_hz)
                .mode(SpiModeFlags::SPI_MODE_0)
                .build(),
        )
        .with_context(|| format!("configuring {dev} at {speed_hz} Hz"))?;
        Ok(SpidevDevice { dev: spi })
    }

    impl ErrorType for SpidevDevice {
        type Error = SpiError;
    }

    impl SpiDevice for SpidevDevice {
        fn transaction(
            &mut self,
            operations: &mut [Operation<'_, u8>],
        ) -> Result<(), SpiError> {
            // Screen for the two shapes spidev cannot express, and take
            // the TransferInPlace tx snapshots (they must outlive the
            // ioctl below).
            let mut copies: Vec<Vec<u8>> = Vec::new();
            for op in operations.iter() {
                match op {
                    Operation::DelayNs(_) => {
                        return Err(SpiError::Unsupported(
                            "a delay inside one chip-select assertion",
                        ))
                    }
                    Operation::Transfer(read, write) if read.len() != write.len() => {
                        return Err(SpiError::Unsupported(
                            "Transfer with different read/write lengths",
                        ))
                    }
                    Operation::TransferInPlace(buf) => copies.push(buf.to_vec()),
                    _ => {}
                }
            }
            let mut next_copy = copies.iter();
            let mut transfers: Vec<SpidevTransfer> = Vec::with_capacity(operations.len());
            for op in operations.iter_mut() {
                match op {
                    Operation::Read(buf) => transfers.push(SpidevTransfer::read(buf)),
                    Operation::Write(bytes) => transfers.push(SpidevTransfer::write(bytes)),
                    Operation::Transfer(read, write) => {
                        transfers.push(SpidevTransfer::read_write(write, read));
                    }
                    Operation::TransferInPlace(buf) => {
                        let tx = next_copy.next().map(Vec::as_slice).unwrap_or(&[]);
                        transfers.push(SpidevTransfer::read_write(tx, buf));
                    }
                    Operation::DelayNs(_) => unreachable!("screened above"),
                }
            }
            self.dev.transfer_multiple(&mut transfers).map_err(SpiError::Io)
        }
    }
}

// ---------------------------------------------------------------------------
// gpio backend (gpio-cdev character device)
// ---------------------------------------------------------------------------

mod gpio {
    use anyhow::Context;
    use embedded_hal::digital::{ErrorType, InputPin, OutputPin};
    use gpio_cdev::{Chip, LineHandle, LineRequestFlags};

    #[derive(Debug)]
    pub struct PinError(gpio_cdev::Error);

    impl std::fmt::Display for PinError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "gpio line access failed: {}", self.0)
        }
    }

    impl std::error::Error for PinError {}

    impl embedded_hal::digital::Error for PinError {
        fn kind(&self) -> embedded_hal::digital::ErrorKind {
            embedded_hal::digital::ErrorKind::Other
        }
    }

    pub struct OutPin(LineHandle);
    pub struct InPin(LineHandle);

    /// Claim the two lines: TX driven high (UART idle mark) from the
    /// start, RX as input.
    pub fn open(chip: &str, pin_tx: u32, pin_rx: u32) -> anyhow::Result<(OutPin, InPin)> {
        let mut chip =
            Chip::new(chip).with_context(|| format!("opening GPIO chip {chip}"))?;
        let tx = chip
            .get_line(pin_tx)
            .and_then(|l| l.request(LineRequestFlags::OUTPUT, 1, "updater-cli tx"))
            .with_context(|| format!("claiming TX line {pin_tx}"))?;
        let rx = chip
            .get_line(pin_rx)
            .and_then(|l| l.request(LineRequestFlags::INPUT, 0, "updater-cli rx"))
            .with_context(|| format!("claiming RX line {pin_rx}"))?;
        Ok((OutPin(tx), InPin(rx)))
    }

    impl ErrorType for OutPin {
        type Error = PinError;
    }

    impl OutputPin for OutPin {
        fn set_low(&mut self) -> Result<(), PinError> {
            self.0.set_value(0).map_err(PinError)
        }

        fn set_high(&mut self) -> Result<(), PinError> {
            self.0.set_value(1).map_err(PinError)
        }
    }

    impl ErrorType for InPin {
        type Error = PinError;
    }

    impl InputPin for InPin {
        fn is_high(&mut self) -> Result<bool, PinError> {
            self.0.get_value().map(|v| v != 0).map_err(PinError)
        }

        fn is_low(&mut self) -> Result<bool, PinError> {
            self.is_high().map(|h| !h)
        }
    }
}

// ---------------------------------------------------------------------------
// tests: the erase-budget seam (hardware-free)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;

    /// Records the attempt budget in force at each request.
    struct FakeBackend {
        seen: Rc<RefCell<Vec<u32>>>,
        attempts: u32,
    }

    impl Transport for FakeBackend {
        type Err = anyhow::Error;

        fn request(&mut self, _req: &[u8], _rsp: &mut [u8]) -> Result<usize, Self::Err> {
            self.seen.borrow_mut().push(self.attempts);
            Ok(0)
        }
    }

    impl PollTransport for FakeBackend {
        fn set_poll_budget(&mut self, poll: PollBudget) {
            self.attempts = poll.attempts.max(1);
        }
    }

    fn dyn_over_fake(base: PollBudget) -> (DynTransport, Rc<RefCell<Vec<u32>>>) {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let mut inner: Box<dyn PollTransport> =
            Box::new(FakeBackend { seen: Rc::clone(&seen), attempts: 0 });
        inner.set_poll_budget(base);
        (DynTransport { inner, base, erase_attempts: base.attempts }, seen)
    }

    #[test]
    fn only_erase_requests_run_with_the_raised_budget() {
        let (mut t, seen) = dyn_over_fake(PollBudget { attempts: 100, delay_ms: 10 });
        t.set_erase_attempts(480);
        let mut rsp = [0u8; 4];
        t.request(&[CMD_ERASE_APP, 0, 0], &mut rsp).unwrap();
        t.request(&[updater_core::frame::CMD_INFO, 0, 0], &mut rsp).unwrap();
        t.request(&[CMD_ERASE_APP, 0, 0], &mut rsp).unwrap();
        assert_eq!(
            *seen.borrow(),
            vec![480, 100, 480],
            "erase gets the raised budget; everything else keeps the base"
        );
    }

    #[test]
    fn erase_budget_never_drops_below_the_base() {
        let (mut t, seen) = dyn_over_fake(PollBudget { attempts: 1000, delay_ms: 10 });
        t.set_erase_attempts(5); // geometry asked for less than the user did
        let mut rsp = [0u8; 4];
        t.request(&[CMD_ERASE_APP, 0, 0], &mut rsp).unwrap();
        assert_eq!(*seen.borrow(), vec![1000], "--poll-attempts wins when larger");
    }
}
