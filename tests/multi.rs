//! The thread-per-core runtime: workers, cross-thread wake-ups, shutdown.

use std::collections::HashSet;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use giong::net::TcpListener;
use giong::sync::mpsc;
use giong::time::{sleep, timeout};

#[test]
fn every_worker_runs_its_main_future_on_its_own_thread() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s = seen.clone();
    giong::Builder::new()
        .workers(4)
        .build()
        .unwrap()
        .run(move |index| {
            let s = s.clone();
            async move {
                assert_eq!(giong::worker_id(), index);
                sleep(Duration::from_millis(5)).await;
                s.lock().unwrap().push((index, std::thread::current().id()));
            }
        })
        .unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(
        seen.iter().map(|(i, _)| *i).collect::<HashSet<_>>(),
        (0..4).collect()
    );
    assert_eq!(
        seen.iter().map(|(_, t)| *t).collect::<HashSet<_>>().len(),
        4
    );
}

#[test]
fn shutdown_from_outside_wakes_every_worker() {
    let rt = giong::Builder::new().workers(3).build().unwrap();
    let handle = rt.handle();
    let stopper = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        handle.shutdown();
    });
    let start = Instant::now();
    rt.run(|_| async {
        giong::shutdown_signal().await;
    })
    .unwrap();
    stopper.join().unwrap();
    assert!(start.elapsed() >= Duration::from_millis(50));
    assert!(start.elapsed() < Duration::from_secs(5));
}

#[test]
fn until_shutdown_interrupts_a_pending_future() {
    let rt = giong::Builder::new().workers(2).build().unwrap();
    rt.run(|index| async move {
        if index == 0 {
            sleep(Duration::from_millis(20)).await;
            giong::handle().shutdown();
        }
        let res = giong::until_shutdown(sleep(Duration::from_secs(3600))).await;
        assert!(res.is_none());
        assert!(giong::handle().is_shutdown());
        // Once shut down, it resolves immediately.
        assert!(giong::until_shutdown(sleep(Duration::from_secs(3600)))
            .await
            .is_none());
    })
    .unwrap();
}

#[test]
fn spawn_on_runs_work_on_the_chosen_worker() {
    const WORKERS: usize = 4;
    let rt = giong::Builder::new().workers(WORKERS).build().unwrap();
    let handle = rt.handle();
    let hits = Arc::new(AtomicUsize::new(0));
    let h = hits.clone();
    rt.run(move |index| {
        let handle = handle.clone();
        let hits = h.clone();
        async move {
            if index == 0 {
                // Ask every worker (including ourselves) to run a job.
                for target in 0..WORKERS {
                    let hits = hits.clone();
                    let sent = handle.spawn_on(target, move || async move {
                        assert_eq!(giong::worker_id(), target);
                        sleep(Duration::from_millis(1)).await;
                        if hits.fetch_add(1, Ordering::SeqCst) + 1 == WORKERS {
                            giong::handle().shutdown();
                        }
                    });
                    assert!(sent);
                }
                assert!(!handle.spawn_on(WORKERS, || async {}));
            }
            giong::shutdown_signal().await;
        }
    })
    .unwrap();
    assert_eq!(hits.load(Ordering::SeqCst), WORKERS);
}

#[test]
fn channel_delivers_across_workers_and_from_plain_threads() {
    const PER_SENDER: usize = 10_000;
    let (tx, rx) = mpsc::channel::<usize>();
    let rx = Arc::new(Mutex::new(Some(rx)));
    // One sender for each of workers 1 and 2, handed out by index.
    let senders = Arc::new(Mutex::new(vec![None, Some(tx.clone()), Some(tx.clone())]));
    let total = Arc::new(AtomicUsize::new(0));
    let t = total.clone();

    // A sender that is not a runtime thread at all.
    let outside = std::thread::spawn(move || {
        for i in 0..PER_SENDER {
            tx.send(i).unwrap();
            if i % 1000 == 0 {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    });

    giong::Builder::new()
        .workers(3)
        .build()
        .unwrap()
        .run(move |index| {
            let (rx, senders, total) = (rx.clone(), senders.clone(), t.clone());
            async move {
                if index == 0 {
                    // Receiver: sleeps in io_uring between messages and must be
                    // woken by other threads through the eventfd.
                    let mut rx = rx.lock().unwrap().take().unwrap();
                    let mut sum = 0;
                    while let Some(v) = rx.recv().await {
                        sum += v;
                    }
                    total.store(sum, Ordering::SeqCst);
                } else {
                    let tx = senders.lock().unwrap()[index].take().unwrap();
                    for i in 0..PER_SENDER {
                        tx.send(i).unwrap();
                        if i % 500 == 0 {
                            sleep(Duration::from_micros(200)).await;
                        }
                    }
                }
            }
        })
        .unwrap();
    outside.join().unwrap();
    let per_sender: usize = (0..PER_SENDER).sum();
    assert_eq!(total.load(Ordering::SeqCst), 3 * per_sender);
}

#[test]
fn reuseport_listeners_on_every_worker_serve_connections() {
    const WORKERS: usize = 3;
    const CLIENTS: usize = 60;
    // Reserve a port.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let rt = giong::Builder::new().workers(WORKERS).build().unwrap();
    let handle = rt.handle();
    let ready = Arc::new(AtomicUsize::new(0));
    let served = Arc::new(Mutex::new(vec![0usize; WORKERS]));

    let (r, s) = (ready.clone(), served.clone());
    let server = std::thread::spawn(move || {
        rt.run(move |index| {
            let (ready, served) = (r.clone(), s.clone());
            async move {
                let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
                ready.fetch_add(1, Ordering::SeqCst);
                while let Some(conn) = giong::until_shutdown(listener.accept()).await {
                    let stream = conn.unwrap();
                    let served = served.clone();
                    giong::spawn(async move {
                        let data = stream.recv().await.unwrap();
                        let reply = data.to_vec();
                        drop(data);
                        stream.write_all(reply).await.0.unwrap();
                        served.lock().unwrap()[index] += 1;
                    });
                }
            }
        })
        .unwrap();
    });

    while ready.load(Ordering::SeqCst) < WORKERS {
        std::thread::sleep(Duration::from_millis(1));
    }
    for i in 0..CLIENTS {
        let mut c = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        let msg = format!("client-{i}");
        c.write_all(msg.as_bytes()).unwrap();
        let mut buf = vec![0u8; msg.len()];
        c.read_exact(&mut buf).unwrap();
        assert_eq!(buf, msg.as_bytes());
    }
    handle.shutdown();
    server.join().unwrap();
    let served = served.lock().unwrap();
    assert_eq!(served.iter().sum::<usize>(), CLIENTS);
}

#[test]
fn a_panicking_worker_stops_the_runtime_and_propagates() {
    let result = std::panic::catch_unwind(|| {
        giong::Builder::new()
            .workers(3)
            .build()
            .unwrap()
            .run(|index| async move {
                if index == 1 {
                    sleep(Duration::from_millis(10)).await;
                    panic!("worker one is down");
                }
                giong::shutdown_signal().await;
            })
            .unwrap();
    });
    let payload = result.unwrap_err();
    assert_eq!(
        *payload.downcast_ref::<&str>().unwrap(),
        "worker one is down"
    );
}

#[test]
fn sigterm_triggers_a_graceful_shutdown() {
    // Runs in a child process so the signal cannot disturb the other tests.
    if std::env::var_os("GIONG_SIGNAL_CHILD").is_some() {
        giong::Builder::new()
            .workers(2)
            .shutdown_on_signals(true)
            .build()
            .unwrap()
            .run(|index| async move {
                if index == 0 {
                    println!("ready");
                }
                giong::shutdown_signal().await;
            })
            .unwrap();
        println!("clean exit");
        return;
    }
    use std::io::BufRead;
    use std::process::{Command, Stdio};
    let exe = std::env::current_exe().unwrap();
    let mut child = Command::new(exe)
        .args([
            "--exact",
            "sigterm_triggers_a_graceful_shutdown",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("GIONG_SIGNAL_CHILD", "1")
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = std::io::BufReader::new(child.stdout.take().unwrap()).lines();
    assert!(
        lines.by_ref().any(|l| l.unwrap().contains("ready")),
        "child never became ready"
    );
    unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
    let rest: Vec<String> = lines.map(|l| l.unwrap()).collect();
    assert!(child.wait().unwrap().success());
    assert!(
        rest.iter().any(|l| l.contains("clean exit")),
        "child output: {rest:?}"
    );
}

#[test]
fn a_sleeping_worker_is_woken_promptly_by_another_thread() {
    // Latency of the eventfd wake-up path.
    let (tx, mut rx) = mpsc::channel::<Instant>();
    let sender = std::thread::spawn(move || {
        for _ in 0..200 {
            std::thread::sleep(Duration::from_millis(1));
            tx.send(Instant::now()).unwrap();
        }
    });
    let worst = giong::block_on(async move {
        let mut worst = Duration::ZERO;
        while let Some(sent) = timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("lost wake-up")
        {
            worst = worst.max(sent.elapsed());
        }
        worst
    });
    sender.join().unwrap();
    assert!(
        worst < Duration::from_millis(100),
        "slow cross-thread wake-up: {worst:?}"
    );
}

#[test]
fn a_panicking_spawn_on_closure_does_not_kill_the_worker() {
    let rt = giong::Builder::new().workers(2).build().unwrap();
    let handle = rt.handle();
    let done = Arc::new(AtomicUsize::new(0));
    let d = done.clone();
    rt.run(move |index| {
        let (handle, done) = (handle.clone(), d.clone());
        async move {
            if index == 0 {
                // The closure that builds the future blows up on worker 1 ...
                assert!(
                    handle.spawn_on(1, || -> std::future::Ready<()> { panic!("factory failed") })
                );
                sleep(Duration::from_millis(20)).await;
                // ... which must still be alive to run the next job.
                assert!(handle.spawn_on(1, move || async move {
                    done.store(giong::worker_id() + 10, Ordering::SeqCst);
                    giong::handle().shutdown();
                }));
            }
            giong::shutdown_signal().await;
        }
    })
    .unwrap();
    assert_eq!(done.load(Ordering::SeqCst), 11);
}
