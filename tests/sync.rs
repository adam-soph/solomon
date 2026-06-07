//! Atomics + mutex tests (`lib/sync.hc`).
//!
//! Concurrency is impure, so these are **property** tests. Threads hammer a shared
//! counter, once via `AtomicAdd` and once under a `Mutex`. The final total is
//! deterministic (`threads × iterations`) only if the synchronization actually works.
//! The interpreter (synchronous threads, no contention) and the native backends (real
//! threads plus hardware atomics) must agree, so they double as a conformance check.
//! Direct `AtomicSwap`/`AtomicCas`/`AtomicLoad` semantics are checked too.

use std::process::Command;

use solomon::codegen::Codegen;
use solomon::interp::run_to_string;
use solomon::parser::parse_with;
use solomon::sema::check_program;
use solomon::{Arm64Darwin, Arm64Linux, X64Linux};

// The portable program, which runs on all four targets. It exercises a *single* shared
// atomic counter under real threads, the mutex on its *uncontended* fast path
// (single-threaded, no kernel block), a fence, and the full width matrix.
const PROGRAM: &str = r#"
    #include <sync.hc>
    #include <thread.hc>
    I64 acount = 0;
    Mutex mu;
    I64 mcount = 0;
    I64 AWorker(I64 n) { I64 i; for (i = 0; i < 2000; i++) AtomicAdd(&acount, 1); return 0; }
    U0 Main() {
      I64 h[4];
      I64 i;
      for (i = 0; i < 4; i++) h[i] = Thread(&AWorker, i);
      for (i = 0; i < 4; i++) Join(h[i]);
      AtomicFence();
      MutexInit(&mu);
      for (i = 0; i < 8000; i++) { MutexLock(&mu); mcount++; MutexUnlock(&mu); }
      "acount=%d mcount=%d\n", acount, mcount;
      // Direct atomic semantics (I64).
      I64 x = 5;
      I64 sw = AtomicSwap(&x, 42);
      I64 c1 = AtomicCas(&x, 0, 7);
      I64 c2 = AtomicCas(&x, 42, 7);
      "x=%d sw=%d c1=%d c2=%d load=%d\n", x, sw, c1, c2, AtomicLoad(&x);
      // Width-directed atomics: U32 wraparound, U8 truncation, I16 sign-extension.
      U32 w = 0xFFFFFFFF; I64 nw = AtomicAdd(&w, 2);
      U8 b = 250; I64 nb = AtomicAdd(&b, 10);
      I16 s = -1; I64 cs = AtomicCas(&s, -1, -50);
      "w=%u nw=%d b=%d nb=%d cs=%d s=%d\n", w, nw, b, nb, cs, s;
    }
    Main;
"#;

// acount: 4 threads × 2000 = 8000. mcount: 8000 single-threaded mutex round-trips.
// Direct I64 line: swap returns old 5 (x=42); the first CAS fails (returns 42); the
// second swaps 42→7 (returns 42); load reads 7. Widths: U32 0xFFFFFFFF+2 wraps to 1;
// U8 250+10 truncates to 4; I16 CAS witnesses -1 and sets -50.
const EXPECTED: &str =
    "acount=8000 mcount=8000\nx=7 sw=5 c1=42 c2=42 load=7\nw=1 nw=1 b=4 nb=4 cs=-1 s=-50\n";

// The *blocking* mutex under real contention: 4 threads increment a shared counter in a
// critical section, so the futex wait/wake path runs. Verified on the native runners —
// arm64 Darwin here, and the freestanding Linux path on a native linux/aarch64 or
// linux/x86_64 host (e.g. CI).
const CONTENDED: &str = r#"
    #include <sync.hc>
    #include <thread.hc>
    Mutex mu;
    I64 mcount = 0;
    I64 MWorker(I64 n) { I64 i; for (i = 0; i < 2000; i++) { MutexLock(&mu); mcount++; MutexUnlock(&mu); } return 0; }
    U0 Main() {
      MutexInit(&mu);
      I64 h[4]; I64 i;
      for (i = 0; i < 4; i++) h[i] = Thread(&MWorker, i);
      for (i = 0; i < 4; i++) Join(h[i]);
      "mcount=%d\n", mcount;
    }
    Main;
"#;
const CONTENDED_EXPECTED: &str = "mcount=8000\n";

// Condition variable: 4 workers block in `CondWait` until the main thread sets the
// predicate and `CondBroadcast`s, then each bumps `done`. The synchronous interpreter
// can't model a consumer that waits for a later producer, so this is native-only.
const CONDVAR: &str = r#"
    #include <sync.hc>
    #include <time.hc>
    #include <thread.hc>
    Mutex mu;
    Cond cv;
    I64 go = 0;
    I64 done = 0;
    I64 W(I64 n) {
      MutexLock(&mu);
      while (!go) CondWait(&cv, &mu);
      MutexUnlock(&mu);
      AtomicAdd(&done, 1);
      return 0;
    }
    U0 Main() {
      MutexInit(&mu); CondInit(&cv);
      I64 h[4]; I64 i;
      for (i = 0; i < 4; i++) h[i] = Thread(&W, i);
      Sleep(50000000);  // 50ms: let the workers reach CondWait
      MutexLock(&mu); go = 1; CondBroadcast(&cv); MutexUnlock(&mu);
      for (i = 0; i < 4; i++) Join(h[i]);
      "done=%d\n", done;
    }
    Main;
"#;
const CONDVAR_EXPECTED: &str = "done=4\n";

// Reader/writer lock: 4 threads each do 1000 iterations of (write-locked increment,
// read-locked read). The writes are mutually exclusive, so the counter is exactly 4000.
// This is interleaving-independent, so the synchronous interpreter handles it too.
const RWLOCK: &str = r#"
    #include <sync.hc>
    #include <thread.hc>
    RwLock rw;
    I64 counter = 0;
    I64 W(I64 n) {
      I64 i;
      for (i = 0; i < 1000; i++) {
        RwLockWLock(&rw); counter++; RwLockWUnlock(&rw);
        RwLockRLock(&rw); I64 t = counter; RwLockRUnlock(&rw);
      }
      return 0;
    }
    U0 Main() {
      RwLockInit(&rw);
      I64 h[4]; I64 i;
      for (i = 0; i < 4; i++) h[i] = Thread(&W, i);
      for (i = 0; i < 4; i++) Join(h[i]);
      "counter=%d\n", counter;
    }
    Main;
"#;
const RWLOCK_EXPECTED: &str = "counter=4000\n";

fn lib_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lib")
}

fn compile(src: &str) -> solomon::Program {
    let program = parse_with(src, std::path::Path::new("."), &[lib_dir()])
        .unwrap_or_else(|e| panic!("parse failed: {e}"));
    assert!(check_program(&program).is_empty(), "sema errors");
    program
}

#[test]
fn interp_sync() {
    let out = run_to_string(&compile(PROGRAM)).unwrap_or_else(|e| panic!("interp error: {e}"));
    assert_eq!(out, EXPECTED);
}

fn darwin_toolchain() -> bool {
    cfg!(all(target_arch = "aarch64", target_os = "macos"))
        && Command::new("cc")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
}

/// Atomics + mutex through the **native arm64 Darwin** backend (`ldaxr`/`stlxr` loops
/// and `ldar`/`stlr`), with real pthreads. Self-skips off an Apple-silicon host.
#[test]
fn native_arm64_sync() {
    if !darwin_toolchain() {
        eprintln!("skipping: native sync test needs aarch64-apple-darwin + cc");
        return;
    }
    let bin = std::env::temp_dir().join(format!("solomon-sync-{}", std::process::id()));
    Arm64Darwin::new(&bin)
        .run(&compile(PROGRAM))
        .unwrap_or_else(|e| panic!("arm64 build failed: {e}"));
    let output = Command::new(&bin)
        .output()
        .unwrap_or_else(|e| panic!("could not run produced binary: {e}"));
    let _ = std::fs::remove_file(&bin);
    assert_eq!(String::from_utf8_lossy(&output.stdout), EXPECTED);
}

/// Build `src` with `backend` to a temp ELF and run it **natively**. Only called on a
/// matching Linux host. The atomics/futex ops hit the real kernel. Returns stdout.
fn freestanding_sync_stdout(out: &std::path::Path, mut backend: impl Codegen, src: &str) -> String {
    backend
        .run(&compile(src))
        .unwrap_or_else(|e| panic!("freestanding build failed: {e}"));
    let output = Command::new(out)
        .output()
        .unwrap_or_else(|e| panic!("could not run produced ELF: {e}"));
    let _ = std::fs::remove_file(out);
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Atomics + mutex through the **freestanding x86-64** backend (`lock`-prefixed
/// `xadd`/`xchg`/`cmpxchg`), with real `clone(2)` threads. Runs only on a linux/x86_64
/// host (CI); self-skips elsewhere.
#[test]
fn native_x86_64_freestanding_sync() {
    if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        eprintln!("skipping: freestanding x86-64 sync test needs a linux/x86_64 host");
        return;
    }
    let out = std::env::temp_dir().join(format!("solomon-x64-sync-{}", std::process::id()));
    let got = freestanding_sync_stdout(&out, X64Linux::new(&out), PROGRAM);
    assert_eq!(got, EXPECTED, "x86_64 freestanding");
}

/// Atomics + mutex through the **freestanding aarch64** backend (`ldaxr`/`stlxr`).
/// Runs only on a linux/aarch64 host; self-skips elsewhere.
#[test]
fn native_arm64_freestanding_sync() {
    if !cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        eprintln!("skipping: freestanding aarch64 sync test needs a linux/aarch64 host");
        return;
    }
    let out = std::env::temp_dir().join(format!("solomon-arm-sync-{}", std::process::id()));
    let got = freestanding_sync_stdout(&out, Arm64Linux::new(&out), PROGRAM);
    assert_eq!(got, EXPECTED, "arm64 freestanding");
}

/// The **blocking futex mutex under real contention** through native arm64 Darwin
/// (`__ulock_wait`/`__ulock_wake`). Self-skips off an Apple-silicon host.
#[test]
fn native_arm64_contended_mutex() {
    if !darwin_toolchain() {
        eprintln!("skipping: contended mutex test needs aarch64-apple-darwin + cc");
        return;
    }
    let bin = std::env::temp_dir().join(format!("solomon-cmu-{}", std::process::id()));
    Arm64Darwin::new(&bin)
        .run(&compile(CONTENDED))
        .unwrap_or_else(|e| panic!("arm64 build failed: {e}"));
    let output = Command::new(&bin)
        .output()
        .unwrap_or_else(|e| panic!("could not run produced binary: {e}"));
    let _ = std::fs::remove_file(&bin);
    assert_eq!(String::from_utf8_lossy(&output.stdout), CONTENDED_EXPECTED);
}

/// The blocking futex mutex under real contention through the **freestanding aarch64**
/// backend (the Linux `futex(2)` wait/wake path). Runs only on a linux/aarch64 host;
/// self-skips elsewhere. The Darwin path above covers the arm64 logic on the Mac.
#[test]
fn native_arm64_freestanding_contended_mutex() {
    if !cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        eprintln!("skipping: freestanding aarch64 contended mutex test needs a linux/aarch64 host");
        return;
    }
    let out = std::env::temp_dir().join(format!("solomon-arm-cmu-{}", std::process::id()));
    let got = freestanding_sync_stdout(&out, Arm64Linux::new(&out), CONTENDED);
    assert_eq!(
        got, CONTENDED_EXPECTED,
        "arm64 freestanding contended mutex"
    );
}

/// Build `src` with the arm64 Darwin backend, run the binary, and return its stdout.
fn arm64_darwin_stdout(src: &str) -> String {
    let bin = std::env::temp_dir().join(format!("solomon-sd-{}", std::process::id()));
    Arm64Darwin::new(&bin)
        .run(&compile(src))
        .unwrap_or_else(|e| panic!("arm64 build failed: {e}"));
    let output = Command::new(&bin)
        .output()
        .unwrap_or_else(|e| panic!("could not run produced binary: {e}"));
    let _ = std::fs::remove_file(&bin);
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The reader/writer lock on the **interpreter**. It is interleaving-independent, so
/// the synchronous threads still give the exact count.
#[test]
fn interp_rwlock() {
    let out = run_to_string(&compile(RWLOCK)).unwrap_or_else(|e| panic!("interp error: {e}"));
    assert_eq!(out, RWLOCK_EXPECTED);
}

/// Condition variable + reader/writer lock under real contention on the native arm64
/// runners (Darwin `__ulock`, freestanding `futex`). Native-only: the producer/consumer
/// condvar can't run on the synchronous interpreter.
#[test]
fn native_arm64_condvar_rwlock() {
    if darwin_toolchain() {
        assert_eq!(
            arm64_darwin_stdout(CONDVAR),
            CONDVAR_EXPECTED,
            "Darwin condvar"
        );
        assert_eq!(
            arm64_darwin_stdout(RWLOCK),
            RWLOCK_EXPECTED,
            "Darwin rwlock"
        );
    } else {
        eprintln!("skipping: native condvar/rwlock test needs aarch64-apple-darwin + cc");
    }
}

#[test]
fn native_arm64_freestanding_condvar_rwlock() {
    if !cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        eprintln!("skipping: freestanding aarch64 condvar/rwlock test needs a linux/aarch64 host");
        return;
    }
    let out = std::env::temp_dir().join(format!("solomon-arm-cr-{}", std::process::id()));
    assert_eq!(
        freestanding_sync_stdout(&out, Arm64Linux::new(&out), CONDVAR),
        CONDVAR_EXPECTED,
        "arm64 freestanding condvar"
    );
    assert_eq!(
        freestanding_sync_stdout(&out, Arm64Linux::new(&out), RWLOCK),
        RWLOCK_EXPECTED,
        "arm64 freestanding rwlock"
    );
}
