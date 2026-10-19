//! linuxtest: an ordinary Rust program (no æther-specific code) that checks
//! the parts of std the Craft apps rely on: threads, locks, channels, time,
//! files in the app's private folder, big allocations and unwinding.
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// A raw system call, to check what the sandbox refuses.
fn syscall6(nr: u64, a: u64, b: u64, c: u64, d: u64, e: u64, f: u64) -> i64 {
    let r: i64;
    unsafe {
        std::arch::asm!("syscall", inlateout("rax") nr as i64 => r, in("rdi") a, in("rsi") b, in("rdx") c,
                        in("r10") d, in("r8") e, in("r9") f, lateout("rcx") _, lateout("r11") _, options(nostack));
    }
    r
}

fn sandbox_checks(failures: &mut u32) {
    const PROT_RW: u64 = 3;
    const PROT_RWX: u64 = 7;
    const PROT_RX: u64 = 5;
    const ANON: u64 = 0x22; // MAP_PRIVATE | MAP_ANONYMOUS
    let neg = |r: i64| r < 0 && r > -4096;
    check("sandbox: no writable+executable memory", neg(syscall6(9, 0, 4096, PROT_RWX, ANON, u64::MAX, 0)), failures);
    check("sandbox: no new executable memory", neg(syscall6(9, 0, 4096, PROT_RX, ANON, u64::MAX, 0)), failures);
    let page = syscall6(9, 0, 4096, PROT_RW, ANON, u64::MAX, 0);
    check("sandbox: can't make data executable", page > 0 && neg(syscall6(10, page as u64, 4096, PROT_RX, 0, 0, 0)), failures);
    check("sandbox: no fork", syscall6(57, 0, 0, 0, 0, 0, 0) == -38, failures);
    check("sandbox: no network", neg(syscall6(41, 2, 1, 0, 0, 0, 0)), failures);
    check("sandbox: can't signal other apps", syscall6(62, 1, 9, 0, 0, 0, 0) == -1, failures);
    check("sandbox: can't map over the kernel", neg(syscall6(9, 0xFFFF_8000_0000_0000, 4096, PROT_RW, ANON | 0x10, u64::MAX, 0)), failures);
    check("sandbox: TLS can't point at the kernel", neg(syscall6(158, 0x1002, 0xFFFF_8000_0000_0000, 0, 0, 0, 0)), failures);
    // A thread whose TLS isn't a user address: refused up front (it used to
    // be loaded into FS_BASE at the next switch and crash the kernel).
    // CLONE_VM | CLONE_SIGHAND | CLONE_THREAD | CLONE_SETTLS; the stack is never used.
    check("sandbox: thread TLS can't point at the kernel",
          syscall6(56, 0x100 | 0x800 | 0x10000 | 0x80000, page as u64 + 4096, 0, 0, 0x8000_0000_0000_0000, 0) == -1, failures);
    check("sandbox: no system log access (native call)", syscall6(0x1000 + 10, page as u64, 64, 0, 0, 0, 0) == -1, failures);
    check("sandbox: kernel pointers rejected", syscall6(1, 1, 0xFFFF_FFFF_8010_0000, 16, 0, 0, 0) == -14, failures);
}

fn check(name: &str, ok: bool, failures: &mut u32) {
    println!("{} {}", if ok { "ok  " } else { "FAIL" }, name);
    if !ok {
        *failures += 1;
    }
}

fn main() {
    let mut failures = 0;
    println!("linuxtest: starting (pid {})", std::process::id());

    check("environment: HOME=/data", std::env::var("HOME").as_deref() == Ok("/data"), &mut failures);
    check("working directory", std::env::current_dir().map(|p| p == std::path::Path::new("/data")).unwrap_or(false), &mut failures);

    // Threads, a mutex and a channel.
    let counter = Arc::new(Mutex::new(0u64));
    let (tx, rx) = mpsc::channel();
    let handles: Vec<_> = (0..4)
        .map(|i| {
            let counter = Arc::clone(&counter);
            let tx = tx.clone();
            thread::spawn(move || {
                for _ in 0..10_000 {
                    *counter.lock().unwrap() += 1;
                }
                tx.send(i).unwrap();
                i * 2
            })
        })
        .collect();
    drop(tx);
    let joined: u64 = handles.into_iter().map(|h| h.join().unwrap()).sum();
    let received: Vec<u64> = rx.iter().collect();
    check("4 threads, mutex", *counter.lock().unwrap() == 40_000 && joined == 12, &mut failures);
    check("channel", received.len() == 4, &mut failures);

    // A condition variable with a timeout, and a scoped thread.
    let pair = Arc::new((Mutex::new(false), Condvar::new()));
    let p2 = Arc::clone(&pair);
    let t = thread::spawn(move || {
        thread::sleep(Duration::from_millis(30));
        *p2.0.lock().unwrap() = true;
        p2.1.notify_one();
    });
    let (lock, cv) = &*pair;
    let (ready, _) = cv.wait_timeout_while(lock.lock().unwrap(), Duration::from_secs(5), |r| !*r).unwrap();
    check("condition variable", *ready, &mut failures);
    drop(ready);
    t.join().unwrap();
    let mut data = vec![1u32; 1000];
    thread::scope(|s| {
        for chunk in data.chunks_mut(250) {
            s.spawn(move || chunk.iter_mut().for_each(|x| *x *= 3));
        }
    });
    check("scoped threads", data.iter().all(|&x| x == 3), &mut failures);

    thread_local!(static TLS: std::cell::Cell<u32> = const { std::cell::Cell::new(7) });
    TLS.with(|v| v.set(8));
    let other = thread::spawn(|| TLS.with(|v| v.get())).join().unwrap();
    check("thread-local storage", other == 7 && TLS.with(|v| v.get()) == 8, &mut failures);

    // Time.
    let start = Instant::now();
    thread::sleep(Duration::from_millis(50));
    let el = start.elapsed();
    check("sleep 50 ms", el >= Duration::from_millis(49) && el < Duration::from_millis(500), &mut failures);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    check("wall clock after 2024", now > 1_704_067_200, &mut failures);

    // Files in the private folder.
    let r = (|| -> std::io::Result<bool> {
        fs::create_dir_all("/data/linuxtest")?;
        let mut f = fs::File::create("/data/linuxtest/hello.txt")?;
        f.write_all(b"hello from Rust on aether\n")?;
        drop(f);
        let mut s = String::new();
        fs::File::open("linuxtest/hello.txt")?.read_to_string(&mut s)?;
        let mut f = fs::OpenOptions::new().read(true).write(true).open("/data/linuxtest/hello.txt")?;
        f.seek(SeekFrom::Start(6))?;
        f.write_all(b"FROM")?;
        drop(f);
        let s2 = fs::read_to_string("/data/linuxtest/hello.txt")?;
        let len = fs::metadata("/data/linuxtest/hello.txt")?.len();
        fs::rename("/data/linuxtest/hello.txt", "/data/linuxtest/renamed.txt")?;
        let names: Vec<String> = fs::read_dir("/data/linuxtest")?
            .map(|e| e.map(|e| e.file_name().to_string_lossy().into_owned()))
            .collect::<Result<_, _>>()?;
        fs::remove_file("/data/linuxtest/renamed.txt")?;
        fs::remove_dir("/data/linuxtest")?;
        Ok(s == "hello from Rust on aether\n" && s2 == "hello FROM Rust on aether\n" && len == 26 && names == ["renamed.txt"])
    })();
    match r {
        Ok(ok) => check("files: create, read, seek, rename, list, remove", ok, &mut failures),
        Err(e) => {
            println!("     error: {e}");
            check("files", false, &mut failures)
        }
    }
    check("no access outside /data", fs::read("/etc/passwd").is_err() && fs::read_dir("/").is_err(), &mut failures);

    // Memory: a big allocation, touched, freed, and a growing map.
    let mut big = vec![0u8; 64 << 20];
    for i in (0..big.len()).step_by(4096) {
        big[i] = (i >> 12) as u8;
    }
    let sum: u64 = big.iter().step_by(4096).map(|&b| b as u64).sum();
    drop(big);
    check("64 MiB allocation", sum > 0, &mut failures);
    let mut m = HashMap::new();
    for i in 0..200_000u32 {
        m.insert(i, i.wrapping_mul(2654435761));
    }
    let mut v: Vec<u8> = Vec::new();
    for i in 0..(8 << 20) {
        v.push(i as u8); // grows by reallocating (mremap)
    }
    check("hash map + growing vector", m.len() == 200_000 && v.len() == 8 << 20 && v[12345] == (12345 % 256) as u8, &mut failures);

    // Unwinding.
    let caught = std::panic::catch_unwind(|| panic!("expected panic")).is_err();
    check("panic caught by catch_unwind", caught, &mut failures);
    check("available parallelism", thread::available_parallelism().map(|n| n.get() >= 1).unwrap_or(false), &mut failures);

    sandbox_checks(&mut failures);

    if failures == 0 {
        println!("linuxtest: all checks passed");
    } else {
        println!("linuxtest: {failures} checks FAILED");
    }
    std::process::exit(if failures == 0 { 0 } else { 1 });
}
