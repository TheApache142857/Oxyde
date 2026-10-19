//! Keeping the protocol channel clean. MCP's stdio transport carries JSON-RPC messages on
//! the standard output and nothing else, but OpenCASCADE prints to it (the STEP writer's
//! transfer statistics, through C++'s `std::cout`), which a client reads as a broken
//! message. So the server keeps a duplicate of the standard output for the protocol and
//! points the process's own standard output, the C runtime's file descriptor 1, at the
//! standard error, where such prints are harmless.

use std::fs::File;

/// The protocol's channel, once descriptor 1 has been pointed at the standard error;
/// `None` (nothing changed) when the standard output cannot be duplicated.
pub(crate) fn protocol_channel() -> Option<File> {
    let saved = duplicate_stdout()?;
    if redirect_stdout_to_stderr() {
        Some(saved)
    } else {
        None
    }
}

#[cfg(windows)]
fn duplicate_stdout() -> Option<File> {
    use std::os::windows::io::AsHandle;
    let handle = std::io::stdout().as_handle().try_clone_to_owned().ok()?;
    Some(File::from(handle))
}

#[cfg(unix)]
fn duplicate_stdout() -> Option<File> {
    use std::os::fd::AsFd;
    let fd = std::io::stdout().as_fd().try_clone_to_owned().ok()?;
    Some(File::from(fd))
}

#[cfg(not(any(windows, unix)))]
fn duplicate_stdout() -> Option<File> {
    None
}

#[cfg(windows)]
fn redirect_stdout_to_stderr() -> bool {
    unsafe extern "C" {
        // The C runtime's `_dup2` (io.h), shared with OpenCASCADE's DLLs through the
        // dynamic runtime.
        fn _dup2(from: i32, to: i32) -> i32;
    }
    // SAFETY: `_dup2` takes two plain file descriptors and only changes the C runtime's
    // descriptor table; 1 and 2 always exist in a console process.
    unsafe { _dup2(2, 1) == 0 }
}

#[cfg(unix)]
fn redirect_stdout_to_stderr() -> bool {
    unsafe extern "C" {
        fn dup2(from: i32, to: i32) -> i32;
    }
    // SAFETY: `dup2` takes two plain file descriptors; 1 and 2 always exist.
    unsafe { dup2(2, 1) == 1 }
}

#[cfg(not(any(windows, unix)))]
fn redirect_stdout_to_stderr() -> bool {
    false
}
