//! Byte-exact tests for the register-offset (SIB) memory encoders. x86 execution is CI-only,
//! so these pin the emitted machine code against hand-assembled references — the surest local
//! check that addressing-mode fusion (T3) encodes `[rcx + rsi*scale]` correctly.

use crate::backend::x86_64::asm::Asm;

#[test]
fn sib_load_store_encodings() {
    // mov rax, [rcx + rsi*8]
    let mut a = Asm::new();
    a.load_sib(8, false, 3);
    assert_eq!(a.code, vec![0x48, 0x8B, 0x04, 0xF1]);

    // mov [rcx + rsi*8], rax
    let mut a = Asm::new();
    a.store_sib(8, 3);
    assert_eq!(a.code, vec![0x48, 0x89, 0x04, 0xF1]);

    // movzx rax, byte [rcx + rsi*1]
    let mut a = Asm::new();
    a.load_sib(1, false, 0);
    assert_eq!(a.code, vec![0x48, 0x0F, 0xB6, 0x04, 0x31]);

    // movsxd rax, dword [rcx + rsi*4]
    let mut a = Asm::new();
    a.load_sib(4, true, 2);
    assert_eq!(a.code, vec![0x48, 0x63, 0x04, 0xB1]);

    // mov eax, [rcx + rsi*4]  (zero-extends to rax; no REX.W)
    let mut a = Asm::new();
    a.load_sib(4, false, 2);
    assert_eq!(a.code, vec![0x8B, 0x04, 0xB1]);

    // movsd xmm0, [rcx + rsi*8]
    let mut a = Asm::new();
    a.movsd_load_sib(3);
    assert_eq!(a.code, vec![0xF2, 0x0F, 0x10, 0x04, 0xF1]);

    // movsd [rcx + rsi*8], xmm0
    let mut a = Asm::new();
    a.movsd_store_sib(3);
    assert_eq!(a.code, vec![0xF2, 0x0F, 0x11, 0x04, 0xF1]);
}
