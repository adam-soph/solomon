use crate::backend::arm64::isel::*;
use std::sync::atomic::{AtomicU64, Ordering};

fn ir_native_output(src: &str) -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let prog = crate::parser::parse(src).expect("parse");
    // Sema annotates `e.ty()`, which the type-directed lowering needs (the CLI runs
    // it before codegen).
    assert!(crate::sema::check_program(&prog).is_empty(), "sema: {src}");
    let ir = crate::backend::lower_to_machine_ir(&prog)
        .unwrap_or_else(|e| panic!("lowering failed for {src:?}: {e}"));
    let obj = compile_ir(&ir, &crate::backend::arm64::darwin::Darwin)
        .unwrap_or_else(|e| panic!("compile_ir failed for {src:?}: {e}"));
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir();
    let obj_path = dir.join(format!("hcc-ir-{}-{n}.o", std::process::id()));
    let exe_path = dir.join(format!("hcc-ir-{}-{n}", std::process::id()));
    std::fs::write(&obj_path, &obj).expect("write obj");
    crate::backend::arm64::darwin::Darwin
        .link(&obj_path, &exe_path)
        .expect("link");
    let out = std::process::Command::new(&exe_path).output().expect("run");
    let _ = std::fs::remove_file(&obj_path);
    let _ = std::fs::remove_file(&exe_path);
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn ir_backend_matches_oracle() {
    let sources = [
        // strings / control flow / globals
        "\"Hello, world!\\n\";",
        "I64 i; for (i = 0; i < 5; i++) if (i & 1) \"odd\\n\"; else \"even\\n\";",
        // calls, recursion, struct-by-value (sret)
        "I64 fib(I64 n) { if (n < 2) return n; return fib(n-1) + fib(n-2); } \
        U0 M() { if (fib(10) == 55) \"fib ok\\n\"; } M;",
        "class P { I64 x; I64 y; } P mk(I64 a, I64 b) { P p; p.x=a; p.y=b; return p; } \
        U0 M() { P q = mk(3, 4); \"%d\\n\", q.x + q.y; } M;",
        // the printf path: integers, hex/unsigned, width, and floats
        "\"%d + %d = %d\\n\", 2, 3, 2 + 3;",
        "\"%x %u %d\\n\", 255, 7, -7;",
        "\"[%5d][%-5d][%05d]\\n\", 42, 42, 42;",
        "\"pi=%f e=%g\\n\", 3.14159, 2.71828;",
        "I64 i; for (i = 1; i <= 5; i++) \"i=%d sq=%d\\n\", i, i * i;",
        // exceptions: catch a local throw, a throw that unwinds out of a callee, and
        // a nested bare re-raise (the full ExcFrame push/pop + longjmp unwind path).
        "try { throw(42); \"unreached\\n\"; } catch { \"caught %d\\n\", Fs->except_ch; }",
        "I64 Boom(I64 n) { if (n > 3) throw(n * 10); return n; } \
        U0 M() { try { Boom(9); \"no\\n\"; } catch { \"got %d\\n\", Fs->except_ch; } } M;",
        "try { try { throw(7); } catch { \"inner %d\\n\", Fs->except_ch; throw; } } \
        catch { \"outer %d flag=%d\\n\", Fs->except_ch, Fs->catch_except; } \
        \"flag now %d\\n\", Fs->catch_except;",
        // the command line (run with no args ⇒ argc == 1, matching the oracle's
        // default argv) and an fd primitive with errno conversion.
        "\"argc=%d\\n\", argc; if (argc >= 1) \"have prog name\\n\";",
        "#include <fcntl.hh>\n\
        I64 fd = Open(\"/no_such_hcc_4c1f_file\", O_RDONLY, 0); \
        \"missing=%d\\n\", fd < 0;",
    ];
    for src in sources {
        // Every case prints, so pull in <stdio.hh> (no auto-include anymore); a
        // duplicate include in the source is a guard-deduped no-op.
        let src = format!("#include <stdio.hh>\n{src}");
        let prog = crate::parser::parse(&src).expect("parse");
        let oracle = crate::oracle::run_to_string(&prog).expect("oracle");
        assert_eq!(
            ir_native_output(&src),
            oracle,
            "IR backend differs for:\n{src}"
        );
    }
}
