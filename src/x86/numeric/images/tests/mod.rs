use super::*;

mod fx;
mod legacy;
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
mod native_xsave;

struct RestoredFixture {
    x87: X87State,
    xmm: [[u8; 16]; 16],
    mxcsr: u32,
}

fn restored_fixture(
    operation: ImageOp,
    bytes: &[u8; 512],
    x87: &X87State,
    xmm: &[[u8; 16]; 16],
    mxcsr: u32,
) -> Result<RestoredFixture, u32> {
    let mut result = RestoredFixture {
        x87: *x87,
        xmm: *xmm,
        mxcsr,
    };
    restore(
        operation,
        bytes,
        &mut result.x87,
        &mut result.xmm,
        &mut result.mxcsr,
    )?;
    Ok(result)
}

fn state() -> X87State {
    let mut state = X87State {
        control_word: 0x37b,
        status_word: 0xa884,
        tag_word: 0xfffc,
        opcode: 0x123,
        instruction_pointer: 0x1234_5678_fedc_ba98,
        data_pointer: 0x8877_6655_1122_3344,
        ..X87State::default()
    };
    for index in 1..8 {
        state.set_raw(
            index,
            ((0x4000 + index as u128) << 64) | (1 << 63) | index as u128,
        );
    }
    state
}

fn xmm() -> [[u8; 16]; 16] {
    std::array::from_fn(|bank| std::array::from_fn(|byte| (bank * 16 + byte) as u8))
}

fn bytes(hex: &str) -> Vec<u8> {
    hex.as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

#[test]
fn image_descriptors_reject_unselected_kind_layout_pairs_and_unused_bits() {
    let mut admitted = 0;
    for encoded in 0..256 {
        if let Some(operation) = ImageOp::try_decode(encoded) {
            admitted += 1;
            assert_eq!(operation.encode(), encoded);
            assert_eq!(
                operation.is_waiting(),
                matches!(operation.kind, Kind::LoadEnvironment | Kind::Restore)
            );
        }
    }
    assert_eq!(admitted, 28);
    for value in [u32::MAX, 1 << 16, 1 << 31] {
        assert!(ImageOp::try_decode(value).is_none());
    }
}

#[test]
fn standard_components_select_only_defined_slots_even_with_poisoned_padding() {
    use super::extended::ExtendedImage;
    for format in [Format::FxLegacy, Format::Fx64, Format::Fx64Wide] {
        for kind in [Kind::SaveX, Kind::RestoreX] {
            let op = ImageOp { kind, format };
            for request in [0, 1, 2, 3, 512, 513, 514, 515] {
                for present in [0, 1, 2, 3, 512, 513, 514, 515] {
                    let mut image = ExtendedImage::new(op, 515, request | 0x1_0000_0000, u64::MAX);
                    image.bytes[512..520].copy_from_slice(&u64::to_le_bytes(present));
                    assert!(image.accept_header(515));
                    let mut actual = [false; 584];
                    for (start, len) in image.fields() {
                        actual[start as usize..(start + len) as usize].fill(true);
                    }
                    let save = kind == Kind::SaveX;
                    let selected = if save { request } else { request & present };
                    let wide = format == Format::Fx64Wide;
                    let expected: [bool; 584] = std::array::from_fn(|n| match n {
                        0..=4 | 6..=11 | 16..=19 => selected & 1 != 0,
                        12..=13 | 20..=21 => selected & 1 != 0 && (wide || save),
                        14..=15 | 22..=23 => selected & 1 != 0 && wide,
                        24..=27 => request & 2 != 0,
                        28..=31 => request & 2 != 0 && save,
                        32..=159 => selected & 1 != 0 && (n - 32) % 16 < 10,
                        160..=287 => selected & 2 != 0,
                        288..=415 => selected & 2 != 0 && format != Format::FxLegacy,
                        512..=519 => save,
                        576..=579 => selected & 512 != 0,
                        _ => false,
                    });
                    assert_eq!(actual, expected, "{op:?} {request:#x} {present:#x}");
                }
            }
        }
    }
}

#[test]
fn standard_header_checks_enabled_bits_and_preserves_unrequested_bits() {
    use super::extended::ExtendedImage;
    let mut op = ImageOp {
        kind: Kind::RestoreX,
        format: Format::Fx64Wide,
    };
    let mut image = ExtendedImage::new(op, 1, 0, 0);
    for offset in 512..576 {
        image.bytes[512..576].fill(0);
        image.bytes[offset] = 0x80;
        assert_eq!(image.accept_header(1), offset >= 536, "{offset}");
    }
    op.kind = Kind::SaveX;
    let mut image = ExtendedImage::new(op, 515, 2, 0);
    let old = 0xfe00_0001_0200_0041u64;
    image.bytes[512..520].copy_from_slice(&old.to_le_bytes());
    image.bytes[520..576].fill(0xa5);
    assert!(image.accept_header(515));
    assert_eq!(&image.bytes[512..520], &(old | 2).to_le_bytes());
    assert_eq!(&image.bytes[520..576], &[0xa5; 56]);
}

#[test]
fn standard_init_clears_x87_payloads_only_visible_xmm_and_uses_the_memory_mxcsr() {
    use super::extended::ExtendedImage;
    let op = ImageOp {
        kind: Kind::RestoreX,
        format: Format::FxLegacy,
    };
    let mut image = ExtendedImage::new(op, 515, 515, 0);
    assert!(image.accept_header(515));
    image.bytes[24..32].copy_from_slice(&[0x81, 0x1f, 0, 0, 0xff, 0xff, 0xff, 0xff]);
    assert_eq!(image.mxcsr(), Ok(0x1f81));
    let (mut x87, mut regs, mut mxcsr, mut pkru) = (state(), xmm(), 0xffff, u32::MAX);
    image.restore(&mut x87, &mut regs, &mut mxcsr, &mut pkru);
    assert_eq!((x87, mxcsr, pkru), (X87State::default(), 0x1f81, 0));
    assert_eq!(&regs[..8], &[[0; 16]; 8]);
    assert_eq!(&regs[8..], &xmm()[8..]);
    image.bytes[27] = 0x80;
    assert_eq!(image.mxcsr(), Err(0x80001f81));
}
