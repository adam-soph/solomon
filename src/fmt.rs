//! Shared C-`printf` conversion-spec parsing, used by both backends so their
//! formatted output agrees.
//!
//! A spec is `%[flags][width][.precision][length]conv`. The native backend
//! reconstructs a libc format string from the parsed spec (injecting the `ll`
//! length so 64-bit values print correctly); the interpreter renders the value
//! itself, applying the same flags/width/precision rules libc would.

use std::iter::Peekable;
use std::str::Chars;

/// A parsed conversion specifier (the text between `%` and the conversion char).
#[derive(Debug, Default, Clone)]
pub struct Spec {
    pub minus: bool, // `-`  left-justify
    pub plus: bool,  // `+`  always show a sign
    pub space: bool, // ` `  space before a non-negative
    pub zero: bool,  // `0`  pad with zeros
    pub hash: bool,  // `#`  alternate form (0x.., leading 0)
    /// Width is `*` (taken from an argument).
    pub width_star: bool,
    pub width: Option<usize>,
    /// A `.` was present (precision applies even if the value is 0).
    pub has_precision: bool,
    /// Precision is `*` (taken from an argument).
    pub prec_star: bool,
    pub precision: usize,
    pub conv: char,
}

/// Parse a spec from `chars`, which must be positioned just after the `%`.
/// Advances past the conversion character.
pub fn parse(chars: &mut Peekable<Chars>) -> Spec {
    let mut s = Spec::default();
    loop {
        match chars.peek() {
            Some('-') => s.minus = true,
            Some('+') => s.plus = true,
            Some(' ') => s.space = true,
            Some('0') => s.zero = true,
            Some('#') => s.hash = true,
            _ => break,
        }
        chars.next();
    }
    if chars.peek() == Some(&'*') {
        s.width_star = true;
        chars.next();
    } else {
        let mut w = String::new();
        while let Some(c) = chars.peek().filter(|c| c.is_ascii_digit()) {
            w.push(*c);
            chars.next();
        }
        if !w.is_empty() {
            s.width = w.parse().ok();
        }
    }
    if chars.peek() == Some(&'.') {
        chars.next();
        s.has_precision = true;
        if chars.peek() == Some(&'*') {
            s.prec_star = true;
            chars.next();
        } else {
            let mut p = String::new();
            while let Some(c) = chars.peek().filter(|c| c.is_ascii_digit()) {
                p.push(*c);
                chars.next();
            }
            s.precision = p.parse().unwrap_or(0);
        }
    }
    // Length modifiers are parsed and discarded — solomon values are 64-bit.
    while matches!(chars.peek(), Some('l' | 'h' | 'L' | 'z' | 'j' | 't')) {
        chars.next();
    }
    s.conv = chars.next().unwrap_or('\0');
    // Clamp the literal width/precision so the native backends' fixed scratch
    // buffers (sized to these caps) can't overflow. `*` width/precision is rejected
    // by the freestanding backends, and the interpreter/libc paths are unbounded, so
    // only the literal forms need clamping. The caps are far beyond any real use, and
    // `MAX_PRECISION` keeps `%f`'s digit count within the 48-limb bignum.
    if let Some(w) = s.width {
        s.width = Some(w.min(MAX_WIDTH));
    }
    s.precision = s.precision.min(MAX_PRECISION);
    s
}

/// Caps on literal field width / precision (see [`parse`]). The native formatters
/// size their digit/field buffers to hold these.
pub const MAX_WIDTH: usize = 1024;
pub const MAX_PRECISION: usize = 512;

/// Reconstruct a libc format string from `s`, injecting the `ll` length modifier
/// on integer conversions so a 64-bit argument is read in full.
pub fn to_c_format(s: &Spec) -> String {
    if s.conv == '%' {
        return "%%".to_string();
    }
    let mut out = String::from("%");
    if s.minus {
        out.push('-');
    }
    if s.plus {
        out.push('+');
    }
    if s.space {
        out.push(' ');
    }
    if s.zero {
        out.push('0');
    }
    if s.hash {
        out.push('#');
    }
    if s.width_star {
        out.push('*');
    } else if let Some(w) = s.width {
        out.push_str(&w.to_string());
    }
    if s.has_precision {
        out.push('.');
        if s.prec_star {
            out.push('*');
        } else {
            out.push_str(&s.precision.to_string());
        }
    }
    match s.conv {
        'd' | 'i' | 'u' | 'x' | 'X' | 'o' => {
            out.push_str("ll");
            out.push(s.conv);
        }
        '\0' => {} // a trailing `%` with no conversion
        c => out.push(c),
    }
    out
}

/// Lay out an integer conversion: apply `precision` (minimum digits), then the
/// width/flags. `sign` is "", "-", "+", or " "; `alt` is an alternate-form prefix
/// such as "0x". Matches libc: with a precision the `0` flag is ignored, and a
/// zero value with precision 0 produces no digits.
pub fn render_int(
    s: &Spec,
    width: Option<usize>,
    precision: Option<usize>,
    sign: &str,
    alt: &str,
    digits: &str,
) -> String {
    let mut digits = digits.to_string();
    if let Some(p) = precision {
        if digits == "0" && p == 0 {
            digits.clear();
        }
        while digits.len() < p {
            digits.insert(0, '0');
        }
    }
    let body_len = sign.len() + alt.len() + digits.len();
    let w = width.unwrap_or(0);
    if w <= body_len {
        format!("{sign}{alt}{digits}")
    } else if s.minus {
        format!("{sign}{alt}{digits}{}", " ".repeat(w - body_len))
    } else if s.zero && precision.is_none() {
        format!("{sign}{alt}{}{digits}", "0".repeat(w - body_len))
    } else {
        format!("{}{sign}{alt}{digits}", " ".repeat(w - body_len))
    }
}

/// Render `mag` (non-negative) in C `%e`/`%E` form: a single leading digit, a
/// `precision`-digit fraction, then `e`/`E`, a sign, and a >=2-digit exponent
/// (e.g. `1.500000e+00`). Rust's `{:e}` does the correctly-rounded mantissa; we
/// only restyle the exponent to match libc.
pub fn render_exp(mag: f64, precision: usize, upper: bool) -> String {
    let s = format!("{:.*e}", precision, mag);
    let (mant, exp) = s.split_once('e').unwrap_or((s.as_str(), "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let e = if upper { 'E' } else { 'e' };
    let sign = if exp < 0 { '-' } else { '+' };
    format!("{mant}{e}{sign}{:02}", exp.unsigned_abs())
}

/// Render `mag` (non-negative) in C `%g`/`%G` form: `precision` significant
/// digits, choosing `%e` or `%f` by the (post-rounding) exponent, and trimming
/// trailing zeros unless `alt` (the `#` flag) is set.
pub fn render_g(mag: f64, precision: usize, upper: bool, alt: bool) -> String {
    let p = precision.max(1);
    // Format as %e at p-1 fractional digits to learn the rounded exponent X.
    let es = format!("{:.*e}", p - 1, mag);
    let (mant, exp) = es.split_once('e').unwrap_or((es.as_str(), "0"));
    let x: i32 = exp.parse().unwrap_or(0);
    let mut body = if x >= -4 && (x as i64) < p as i64 {
        // %f style with precision p-1-X.
        let fp = (p as i32 - 1 - x).max(0) as usize;
        format!("{:.*}", fp, mag)
    } else {
        let e = if upper { 'E' } else { 'e' };
        let sign = if x < 0 { '-' } else { '+' };
        format!("{mant}{e}{sign}{:02}", x.unsigned_abs())
    };
    if !alt {
        // Trim trailing zeros (and a bare `.`) from the mantissa, not the exponent.
        let (m, e) = match body.find(['e', 'E']) {
            Some(i) => (body[..i].to_string(), body[i..].to_string()),
            None => (body.clone(), String::new()),
        };
        if m.contains('.') {
            body = format!("{}{e}", m.trim_end_matches('0').trim_end_matches('.'));
        }
    }
    body
}

/// Lay out a string/char conversion: truncate to `precision` chars, then pad to
/// `width` (left-justified with `-`).
pub fn render_str(s: &Spec, width: Option<usize>, precision: Option<usize>, body: &str) -> String {
    // C `%.Ns` truncates and `%Ns` pads by **bytes** (as the native backends do).
    // Truncate to ≤ `p` bytes, flooring to a char boundary so the result stays valid
    // UTF-8 (identical to the native byte truncation except when a precision splits a
    // multibyte char — vanishingly rare, and only for non-ASCII).
    let body: &str = match precision {
        Some(p) => {
            let mut end = p.min(body.len());
            while end > 0 && !body.is_char_boundary(end) {
                end -= 1;
            }
            &body[..end]
        }
        None => body,
    };
    let len = body.len(); // byte length, so width padding matches the native backend
    let w = width.unwrap_or(0);
    if len >= w {
        body.to_string()
    } else if s.minus {
        format!("{body}{}", " ".repeat(w - len))
    } else {
        format!("{}{body}", " ".repeat(w - len))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
