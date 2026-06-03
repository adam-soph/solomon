//! Tests for the shared `printf` conversion-spec parsing and rendering
//! (`solomon::fmt`), used by both the interpreter and the native backends so their
//! formatted output agrees.

use solomon::fmt::{Spec, parse, render_exp, render_g, render_int, render_str, to_c_format};

fn spec(fmt: &str) -> Spec {
    let mut it = fmt.chars().peekable();
    assert_eq!(it.next(), Some('%'));
    parse(&mut it)
}

#[test]
fn parses_flags_width_precision_length() {
    let s = spec("%-+08.3lld");
    assert!(s.minus && s.plus && s.zero);
    assert_eq!(s.width, Some(8));
    assert!(s.has_precision);
    assert_eq!(s.precision, 3);
    assert_eq!(s.conv, 'd');
}

#[test]
fn native_injects_ll_for_integers() {
    assert_eq!(to_c_format(&spec("%d")), "%lld");
    assert_eq!(to_c_format(&spec("%05x")), "%05llx");
    assert_eq!(to_c_format(&spec("%-8u")), "%-8llu");
    assert_eq!(to_c_format(&spec("%llu")), "%llu");
    assert_eq!(to_c_format(&spec("%#X")), "%#llX");
    assert_eq!(to_c_format(&spec("%f")), "%f");
    assert_eq!(to_c_format(&spec("%.2f")), "%.2f");
    assert_eq!(to_c_format(&spec("%%")), "%%");
}

#[test]
fn renders_int_padding() {
    // %5d of 42
    assert_eq!(
        render_int(&spec("%5d"), Some(5), None, "", "", "42"),
        "   42"
    );
    // %-5d of 42
    assert_eq!(
        render_int(&spec("%-5d"), Some(5), None, "", "", "42"),
        "42   "
    );
    // %05x of 255 -> "000ff"
    assert_eq!(
        render_int(&spec("%05x"), Some(5), None, "", "", "ff"),
        "000ff"
    );
    // %+d of 7
    assert_eq!(render_int(&spec("%+d"), None, None, "+", "", "7"), "+7");
    // %#x of 255 -> 0xff
    assert_eq!(render_int(&spec("%#x"), None, None, "", "0x", "ff"), "0xff");
    // %.3d of 5 -> 005
    assert_eq!(render_int(&spec("%.3d"), None, Some(3), "", "", "5"), "005");
    // zero flag ignored when precision given
    assert_eq!(
        render_int(&spec("%08.2d"), Some(8), Some(2), "", "", "5"),
        "      05"
    );
}

#[test]
fn renders_scientific() {
    assert_eq!(render_exp(1.5, 6, false), "1.500000e+00");
    assert_eq!(render_exp(1234.5, 6, true), "1.234500E+03");
    assert_eq!(render_exp(0.0, 6, false), "0.000000e+00");
    assert_eq!(render_exp(9.9999996, 6, false), "1.000000e+01"); // rounding carry
    assert_eq!(render_exp(1.0e300, 6, false), "1.000000e+300"); // 3-digit exp
    assert_eq!(render_exp(9.6, 0, false), "1e+01");
}

#[test]
fn renders_general() {
    assert_eq!(render_g(1.5, 6, false, false), "1.5");
    assert_eq!(render_g(100000.0, 6, false, false), "100000");
    assert_eq!(render_g(1000000.0, 6, false, false), "1e+06");
    assert_eq!(render_g(0.0001, 6, false, false), "0.0001");
    assert_eq!(render_g(0.00001, 6, false, false), "1e-05");
    assert_eq!(render_g(0.0, 6, false, false), "0");
    assert_eq!(render_g(1.5, 6, false, true), "1.50000"); // # keeps zeros
    assert_eq!(render_g(1234567.0, 6, false, false), "1.23457e+06");
}

#[test]
fn renders_str_padding() {
    assert_eq!(
        render_str(&spec("%10s"), Some(10), None, "hi"),
        "        hi"
    );
    assert_eq!(
        render_str(&spec("%-10s"), Some(10), None, "hi"),
        "hi        "
    );
    assert_eq!(render_str(&spec("%.2s"), None, Some(2), "hello"), "he");
}
