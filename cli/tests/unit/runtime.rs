use super::run_with_runtime;
use std::sync::mpsc;
use std::time::Duration;

#[test]
fn shutdown_returns_with_a_running_blocking_job() {
    let (release, blocked) = mpsc::channel();
    let (finished, completion) = mpsc::channel();
    let runner = std::thread::spawn(move || {
        let result = run_with_runtime(
            async move {
                let (started, ready) = tokio::sync::oneshot::channel();
                tokio::task::spawn_blocking(move || {
                    started.send(()).unwrap();
                    blocked.recv().unwrap();
                });
                ready.await.unwrap();
                Err(std::io::Error::other("preserved command error"))
            },
            Duration::from_millis(20),
        );
        finished.send(result).unwrap();
    });
    let result = completion.recv_timeout(Duration::from_secs(5));
    release.send(()).unwrap();
    runner.join().unwrap();
    assert_eq!(
        result
            .expect("runtime waited for the blocking job")
            .unwrap_err()
            .to_string(),
        "preserved command error"
    );
}
