//! Host pipe I/O must be cancellable independently of child process lifetime.
//! A descendant can escape the process group and retain either pipe forever.
use std::{
    io::{self, Read, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

pub fn supported() -> bool {
    cfg!(unix)
}

pub struct Pipe<T> {
    inner: T,
    stopped: Arc<AtomicBool>,
}

#[cfg(unix)]
impl<T: std::os::fd::AsRawFd> Pipe<T> {
    pub fn new(inner: T, stopped: Arc<AtomicBool>) -> io::Result<Self> {
        let fd = inner.as_raw_fd();
        // SAFETY: fd belongs to inner and remains live throughout both calls.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { inner, stopped })
    }
}

#[cfg(not(unix))]
impl<T> Pipe<T> {
    pub fn new(_: T, _: Arc<AtomicBool>) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Cancellable provider pipes unavailable",
        ))
    }
}

impl<T> Pipe<T> {
    fn check(&self) -> io::Result<()> {
        if self.stopped.load(Ordering::Acquire) {
            Err(io::Error::other("Provider pipe cancelled"))
        } else {
            Ok(())
        }
    }

    fn retry(&self, error: &io::Error) -> bool {
        match error.kind() {
            io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(5));
                true
            }
            io::ErrorKind::Interrupted => true,
            _ => false,
        }
    }
}

impl<T: Read> Read for Pipe<T> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        loop {
            self.check()?;
            match self.inner.read(buffer) {
                Err(error) if self.retry(&error) => (),
                result => return result,
            }
        }
    }
}

impl<T: Write> Write for Pipe<T> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        loop {
            self.check()?;
            match self.inner.write(buffer) {
                Err(error) if self.retry(&error) => (),
                result => return result,
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.check()
    }
}
