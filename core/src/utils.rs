use hkdf::hmac::digest::Output;
use sha2::{Digest, Sha256, Sha256VarCore};
use std::io::Error;
use tokio::select;
#[cfg(not(target_os = "windows"))]
use tokio::signal::unix::{signal, SignalKind};
#[cfg(target_os = "windows")]
use tokio::signal::windows::{ctrl_break, ctrl_c, ctrl_close, ctrl_logoff, ctrl_shutdown};

pub fn hash_256(input: impl AsRef<[u8]>) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(input);
    let mut buf = [0u8; 32];
    hasher.finalize_into(<&mut Output<Sha256VarCore>>::from(&mut buf));
    buf
}

#[cfg(not(target_os = "windows"))]
pub async fn await_termination() -> Result<(), Error> {
    let mut term_signal = signal(SignalKind::terminate())?;
    let mut int_signal = signal(SignalKind::interrupt())?;
    let mut quit_signal = signal(SignalKind::quit())?;
    let mut alarm_signal = signal(SignalKind::alarm())?;
    let mut hup_signal = signal(SignalKind::hangup())?;
    select! {
        _ = term_signal.recv() => (),
        _ = int_signal.recv() => (),
        _ = quit_signal.recv() => (),
        _ = alarm_signal.recv() => (),
        _ = hup_signal.recv() => ()
    }
    Ok(())
}

#[cfg(target_os = "windows")]
pub async fn await_termination() -> Result<(), Error> {
    let mut ctrl_break_signal = ctrl_break()?;
    let mut ctrl_c_signal = ctrl_c()?;
    let mut ctrl_close_signal = ctrl_close()?;
    let mut ctrl_logoff_signal = ctrl_logoff()?;
    let mut ctrl_shutdown_signal = ctrl_shutdown()?;
    select! {
        _ = ctrl_break_signal.recv() => (),
        _ = ctrl_c_signal.recv() => (),
        _ = ctrl_close_signal.recv() => (),
        _ = ctrl_logoff_signal.recv() => (),
        _ = ctrl_shutdown_signal.recv() => ()
    }
    Ok(())
}
