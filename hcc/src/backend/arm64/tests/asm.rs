//! Tests for the AArch64 logical (bitmask) immediate encoder. The encoder is the fiddly
//! `processLogicalImmediate` bit math; this validates it exhaustively against an independent
//! `DecodeBitMasks` so a bad encoding can't slip through (the integration suite would also
//! catch it, as `native != interp` on any `&`/`|`/`^`-by-constant, but this localizes it).

use crate::backend::arm64::asm::encode_logical_imm;

fn mask_bits(esize: u32) -> u64 {
    if esize >= 64 {
        u64::MAX
    } else {
        (1u64 << esize) - 1
    }
}

fn ror_esize(v: u64, r: u32, esize: u32) -> u64 {
    let v = v & mask_bits(esize);
    if r == 0 {
        v
    } else {
        ((v >> r) | (v << (esize - r))) & mask_bits(esize)
    }
}

/// The ARM ARM `DecodeBitMasks` for the 64-bit logical-immediate `wmask`, returning `None`
/// for the reserved encodings the assembler must never emit.
fn decode_logical_imm(n: u32, immr: u32, imms: u32) -> Option<u64> {
    let combined = ((n & 1) << 6) | ((!imms) & 0x3f); // immN : NOT(imms), 7 bits
    if combined == 0 {
        return None;
    }
    let len = 31 - combined.leading_zeros(); // highest set bit index, 0..=6
    if len == 0 {
        return None;
    }
    let esize = 1u32 << len;
    let levels = esize - 1;
    let s = imms & levels;
    let r = immr & levels;
    if s == levels {
        return None; // reserved (run length would be the whole element)
    }
    let welem = (1u64 << (s + 1)) - 1;
    let elem = ror_esize(welem, r, esize);
    // Replicate the esize-bit element across all 64 bits (esize divides 64).
    let mut v = 0u64;
    let mut shift = 0;
    while shift < 64 {
        v |= elem << shift;
        shift += esize;
    }
    Some(v)
}

#[test]
fn logical_imm_roundtrips_every_valid_encoding() {
    // For every value reachable by a valid encoding, the encoder must produce a (canonical)
    // encoding that decodes back to the same value.
    for n in 0..=1u32 {
        for imms in 0..64u32 {
            for immr in 0..64u32 {
                let Some(v) = decode_logical_imm(n, immr, imms) else {
                    continue;
                };
                let enc =
                    encode_logical_imm(v).unwrap_or_else(|| panic!("encode failed for {v:#018x}"));
                let (n2, immr2, imms2) = enc;
                let v2 = decode_logical_imm(n2, immr2, imms2)
                    .unwrap_or_else(|| panic!("encoded a reserved triple for {v:#018x}"));
                assert_eq!(v, v2, "re-encode mismatch for {v:#018x}");
            }
        }
    }
}

#[test]
fn logical_imm_known_and_rejected() {
    assert_eq!(encode_logical_imm(0x7FFF_FFFF), Some((1, 0, 30))); // strhash mask
    assert_eq!(encode_logical_imm(1), Some((1, 0, 0)));
    assert_eq!(encode_logical_imm(3), Some((1, 0, 1)));
    // Not encodable: empty, full, and a value with two separate runs.
    assert_eq!(encode_logical_imm(0), None);
    assert_eq!(encode_logical_imm(u64::MAX), None);
    assert_eq!(encode_logical_imm(0x101), None);
}

#[test]
fn shifted_register_encodings() {
    // Golden words assembled by `/usr/bin/as -arch arm64`; the shifted-register operand
    // fusion (try_shift_fuse) relies on these exact encodings.
    use super::{Asm, SH_ASR, SH_LSL, SH_LSR};
    let cases: &[(&str, fn(&mut Asm), u32)] = &[
        // eor x0, x1, x2, lsl #13   (xorshift: x ^ (x << 13))
        (
            "eor.lsl13",
            |a| a.eor_shifted(0, 1, 2, SH_LSL, 13),
            0xCA02_3420,
        ),
        // add x0, x1, x1, lsl #5    (strhash: (h << 5) + h)
        (
            "add.lsl5",
            |a| a.add_shifted(0, 1, 1, SH_LSL, 5),
            0x8B01_1420,
        ),
        (
            "add.lsl1",
            |a| a.add_shifted(3, 4, 5, SH_LSL, 1),
            0x8B05_0483,
        ),
        (
            "sub.asr3",
            |a| a.sub_shifted(0, 1, 2, SH_ASR, 3),
            0xCB82_0C20,
        ),
        (
            "and.lsr7",
            |a| a.and_shifted(0, 1, 2, SH_LSR, 7),
            0x8A42_1C20,
        ),
        (
            "orr.lsl0",
            |a| a.orr_shifted(0, 1, 2, SH_LSL, 0),
            0xAA02_0020,
        ),
    ];
    for (name, emit, want) in cases {
        let mut a = Asm::new();
        emit(&mut a);
        assert_eq!(a.words[0], *want, "{name}: got {:#010x}", a.words[0]);
    }
}
