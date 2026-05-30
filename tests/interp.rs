//! Tests for the tree-walking interpreter backend.

use solomon::backend::interp::{run_to_string, Interpreter};
use solomon::backend::Backend;
use solomon::parser::parse;
use solomon::sema::check_program;

/// Parse, semantically check, then interpret `src`, returning captured output.
fn run(src: &str) -> String {
    let program = parse(src).unwrap_or_else(|e| panic!("parse failed: {e}"));
    let errs = check_program(&program);
    assert!(errs.is_empty(), "semantic errors: {errs:?}");
    run_to_string(&program).unwrap_or_else(|e| panic!("runtime error: {e}"))
}

#[test]
fn implicit_string_print() {
    assert_eq!(run(r#""Hello, World!\n";"#), "Hello, World!\n");
}

#[test]
fn formatted_print() {
    let src = r#"I64 x = 42; "x=%d\n", x;"#;
    assert_eq!(run(src), "x=42\n");
}

#[test]
fn format_specifiers() {
    let src = r#""%d %x %c %s %f\n", 255, 255, 65, "hi", 1.5;"#;
    assert_eq!(run(src), "255 ff A hi 1.5\n");
}

#[test]
fn arithmetic_and_precedence() {
    assert_eq!(run(r#""%d\n", 1 + 2 * 3 - 4;"#), "3\n");
}

#[test]
fn float_arithmetic() {
    assert_eq!(run(r#""%f\n", 3.0 / 2.0;"#), "1.5\n");
}

#[test]
fn integer_division_and_modulo() {
    assert_eq!(run(r#""%d %d\n", 17 / 5, 17 % 5;"#), "3 2\n");
}

#[test]
fn local_variables_and_assignment() {
    let src = r#"
        U0 Main() {
            I64 a = 10;
            a += 5;
            a *= 2;
            "%d\n", a;
        }
        Main;
    "#;
    assert_eq!(run(src), "30\n");
}

#[test]
fn recursion_factorial() {
    let src = r#"
        I64 Fact(I64 n) {
            if (n <= 1) return 1;
            return n * Fact(n - 1);
        }
        "%d\n", Fact(5);
    "#;
    assert_eq!(run(src), "120\n");
}

#[test]
fn fibonacci_loop() {
    let src = r#"
        I64 Fib(I64 n) {
            if (n < 2) return n;
            return Fib(n - 1) + Fib(n - 2);
        }
        U0 Main() {
            I64 i;
            for (i = 0; i < 8; i++)
                "%d ", Fib(i);
        }
        Main;
    "#;
    assert_eq!(run(src), "0 1 1 2 3 5 8 13 ");
}

#[test]
fn while_loop_accumulates() {
    let src = r#"
        I64 sum = 0, k = 1;
        while (k <= 5) {
            sum += k * k;
            k++;
        }
        "%d\n", sum;
    "#;
    assert_eq!(run(src), "55\n"); // 1+4+9+16+25
}

#[test]
fn do_while_runs_at_least_once() {
    let src = r#"
        I64 i = 10;
        do {
            "%d ", i;
            i++;
        } while (i < 10);
    "#;
    assert_eq!(run(src), "10 ");
}

#[test]
fn ternary_and_logical_short_circuit() {
    assert_eq!(run(r#""%d\n", (3 > 2 && 1) ? 7 : 9;"#), "7\n");
}

#[test]
fn bitwise_operations() {
    assert_eq!(run(r#""%d %d %d\n", 12 & 10, 12 | 3, 1 << 4;"#), "8 15 16\n");
}

#[test]
fn switch_with_fallthrough_and_range() {
    let src = r#"
        U0 Classify(I64 v) {
            switch (v) {
                case 0:
                    "zero ";
                case 1 ... 3:
                    "small\n";
                    break;
                default:
                    "other\n";
            }
        }
        Classify(0);
        Classify(2);
        Classify(9);
    "#;
    // v=0 falls through into the 1..3 case; v=2 hits the range; v=9 default.
    assert_eq!(run(src), "zero small\nsmall\nother\n");
}

#[test]
fn structs_and_pointers() {
    let src = r#"
        class Point { I64 x; I64 y; }
        U0 Main() {
            Point p;
            p.x = 3;
            p.y = 4;
            Point *pp = &p;
            pp->x = pp->x + 10;
            "%d %d\n", p.x, p.y;
        }
        Main;
    "#;
    // The pointer write is visible through `p`.
    assert_eq!(run(src), "13 4\n");
}

#[test]
fn arrays_index_and_mutate() {
    let src = r#"
        U0 Main() {
            I64 a[3];
            a[0] = 5;
            a[1] = a[0] * 2;
            a[2] = a[0] + a[1];
            "%d %d %d\n", a[0], a[1], a[2];
        }
        Main;
    "#;
    assert_eq!(run(src), "5 10 15\n");
}

#[test]
fn default_arguments() {
    let src = r#"
        I64 AddN(I64 a, I64 b = 100) { return a + b; }
        "%d %d\n", AddN(1), AddN(1, 2);
    "#;
    assert_eq!(run(src), "101 3\n");
}

#[test]
fn goto_and_labels() {
    let src = r#"
        U0 Main() {
            I64 i = 0;
        top:
            "%d ", i;
            i++;
            if (i < 3) goto top;
        }
        Main;
    "#;
    assert_eq!(run(src), "0 1 2 ");
}

#[test]
fn cast_truncates_float() {
    assert_eq!(run(r#""%d\n", (I64)3.9;"#), "3\n");
}

#[test]
fn sizeof_type() {
    assert_eq!(run(r#""%d %d %d\n", sizeof(I64), sizeof(U8), sizeof(F64);"#), "8 1 8\n");
}

#[test]
fn sizeof_class_uses_layout() {
    // Point is two I64s => 16 bytes; the layout pass feeds sizeof.
    let src = r#"
        class Point { I64 x; I64 y; }
        "%d\n", sizeof(Point);
    "#;
    assert_eq!(run(src), "16\n");
}

#[test]
fn sizeof_expression_uses_inferred_type() {
    // sizeof of an expression resolves via its statically inferred type:
    //   x       => I64   (8)
    //   1.5     => F64   (8)
    //   x > 0   => I64   (8, a comparison)
    //   p.y     => I32   (4, a struct field)
    let src = r#"
        class Rec { U8 tag; I32 y; }
        U0 Main() {
            I64 x = 0;
            Rec p;
            "%d %d %d %d\n", sizeof(x), sizeof(1.5), sizeof(x > 0), sizeof(p.y);
        }
        Main;
    "#;
    assert_eq!(run(src), "8 8 8 4\n");
}

#[test]
fn sizeof_array_variable_does_not_decay() {
    // `sizeof` of an array variable is the whole array (no pointer decay),
    // while an element is the element size.
    let src = r#"
        U0 Main() {
            I64 a[4];
            "%d %d\n", sizeof(a), sizeof(a[0]);
        }
        Main;
    "#;
    assert_eq!(run(src), "32 8\n");
}

#[test]
fn print_builtin() {
    assert_eq!(run(r#"Print("n=%d\n", 7);"#), "n=7\n");
}

#[test]
fn pointer_arithmetic_indexing_and_comparison() {
    let src = r#"
        U0 Main() {
            I64 a[5];
            I64 *p = &a[0];
            *p = 10;
            *(p + 1) = 20;
            p[2] = 30;
            I64 *stop = &a[5];
            I64 sum = 0;
            I64 *q = &a[0];
            while (q < stop) {
                sum += *q;
                q++;
            }
            "%d %d %d\n", a[0], a[1], a[2];
            "diff=%d sum=%d\n", stop - &a[0], sum;
        }
        Main;
    "#;
    assert_eq!(run(src), "10 20 30\ndiff=5 sum=60\n");
}

#[test]
fn pointer_equality_with_null() {
    let src = r#"
        U0 Main() {
            I64 x = 7;
            I64 *p = NULL;
            if (p == NULL) "is null\n";
            p = &x;
            if (p != NULL) "now set: %d\n", *p;
        }
        Main;
    "#;
    assert_eq!(run(src), "is null\nnow set: 7\n");
}

#[test]
fn cast_truncates_to_width() {
    // (U8)300 -> 44, (I8)200 -> -56, (U16)70000 -> 4464
    assert_eq!(
        run(r#""%d %d %d\n", (U8)300, (I8)200, (U16)70000;"#),
        "44 -56 4464\n"
    );
}

#[test]
fn sizeof_variable_length_array_matches_allocation() {
    // The array dimension is a runtime value; sizeof agrees with what was
    // allocated (4 elements * 8 bytes).
    let src = r#"
        U0 Main() {
            I64 n = 4;
            I64 a[n];
            a[3] = 99;
            "%d %d\n", sizeof(a), a[3];
        }
        Main;
    "#;
    assert_eq!(run(src), "32 99\n");
}

#[test]
fn goto_to_label_in_enclosing_block() {
    // The label is at the function-body level; the goto fires from inside a
    // nested block and resumes at the enclosing label.
    let src = r#"
        U0 Main() {
            I64 i = 0;
        loop:
            {
                "%d ", i;
                i++;
                if (i < 3) goto loop;
            }
        }
        Main;
    "#;
    assert_eq!(run(src), "0 1 2 ");
}

// ---- error reporting ----

#[test]
fn division_by_zero_is_a_runtime_error() {
    let program = parse("I64 x = 1 / 0;").unwrap();
    let mut interp = Interpreter::new(Vec::<u8>::new());
    let err = interp.run(&program).unwrap_err();
    assert!(err.message.contains("division by zero"), "got: {err}");
}

#[test]
fn null_dereference_is_a_runtime_error() {
    let src = "U0 F() { I64 *p = NULL; *p = 1; } F;";
    let program = parse(src).unwrap();
    let mut interp = Interpreter::new(Vec::<u8>::new());
    let err = interp.run(&program).unwrap_err();
    assert!(err.message.contains("null pointer"), "got: {err}");
}

#[test]
fn backend_name() {
    let interp = Interpreter::new(Vec::<u8>::new());
    assert_eq!(interp.name(), "interp");
}

#[test]
fn run_to_string_reports_semantic_errors() {
    // run_to_string type-checks first, so a semantic error surfaces instead of a
    // confusing runtime fault.
    let program = parse("U0 Main() { Frobnicate(1); } Main;").unwrap();
    let err = run_to_string(&program).unwrap_err();
    assert!(err.message.contains("Frobnicate"), "got: {err}");
}
