// SPDX-License-Identifier: Apache-2.0
use super::*;

#[test]
fn dedicated_arrows_and_keypad_enter_decode_after_e0() {
    let mut keyboard = Keyboard::new();
    for (code, key) in [
        (0x48, Key::Up),
        (0x50, Key::Down),
        (0x4b, Key::Left),
        (0x4d, Key::Right),
        (0x1c, Key::Enter),
    ] {
        assert!(keyboard.push(0xe0).is_none());
        assert_eq!(keyboard.push(code).unwrap().key, key);
        assert!(keyboard.push(0xe0).is_none());
        assert!(keyboard.push(code | 0x80).is_none());
    }
}

#[test]
fn each_modifier_side_remains_held_until_both_are_released() {
    let mut keyboard = Keyboard::new();
    for byte in [0x1d, 0xe0, 0x1d, 0x9d] {
        assert!(keyboard.push(byte).is_none());
    }
    assert!(keyboard.push(0x39).unwrap().modifiers.ctrl);
    keyboard.push(0xe0);
    keyboard.push(0x9d);
    assert!(!keyboard.push(0x39).unwrap().modifiers.ctrl);
    for byte in [0x2a, 0x36, 0xaa] {
        keyboard.push(byte);
    }
    assert!(keyboard.modifiers().shift);
    keyboard.push(0xb6);
    assert!(!keyboard.modifiers().shift);
}

#[test]
fn extended_alt_and_super_are_captured() {
    let mut keyboard = Keyboard::new();
    for byte in [0xe0, 0x38, 0xe0, 0x5b] {
        keyboard.push(byte);
    }
    let event = keyboard.push(0x39).unwrap();
    assert!(event.modifiers.alt && event.modifiers.super_key);
    for byte in [0xe0, 0xb8, 0xe0, 0xdb] {
        keyboard.push(byte);
    }
    assert_eq!(keyboard.modifiers(), Modifiers::default());
}

#[test]
fn pause_and_print_screen_do_not_leave_fake_modifiers_held() {
    let mut keyboard = Keyboard::new();
    for byte in [
        0xe1, 0x1d, 0x45, 0xe1, 0x9d, 0xc5, 0xe0, 0x2a, 0xe0, 0x37, 0xe0, 0xb7, 0xe0, 0xaa,
    ] {
        assert!(keyboard.push(byte).is_none());
    }
    assert_eq!(keyboard.modifiers(), Modifiers::default());
    assert_eq!(keyboard.push(0x44).unwrap().key, Key::Function(10));
    assert_eq!(keyboard.push(0x53).unwrap().key, Key::Delete);
}

#[test]
fn modifier_snapshot_is_not_changed_by_later_releases() {
    let mut keyboard = Keyboard::new();
    keyboard.push(0x1d);
    let event = keyboard.push(0x0f).unwrap();
    keyboard.push(0x9d);
    assert!(event.modifiers.ctrl);
    assert!(!keyboard.modifiers().ctrl);
}

fn packet(decoder: &mut MouseDecoder, bytes: [u8; 3]) -> MousePacket {
    assert!(decoder.push(bytes[0]).is_none());
    assert!(decoder.push(bytes[1]).is_none());
    decoder.push(bytes[2]).unwrap()
}

#[test]
fn mouse_uses_nine_bit_signs_not_i8_casts() {
    let mut decoder = MouseDecoder::new();
    let positive = packet(&mut decoder, [0x08, 200, 255]);
    assert_eq!((positive.dx, positive.dy), (200, 255));
    let negative = packet(&mut decoder, [0x38, 56, 0]);
    assert_eq!((negative.dx, negative.dy), (-200, -256));
}

#[test]
fn mouse_overflow_preserves_buttons_and_the_other_axis() {
    let mut decoder = MouseDecoder::new();
    let event = packet(&mut decoder, [0x49, 127, 3]);
    assert_eq!(
        event,
        MousePacket {
            dx: 0,
            dy: 3,
            buttons: 1
        }
    );
    let event = packet(&mut decoder, [0xc8, 127, 127]);
    assert_eq!(
        event,
        MousePacket {
            dx: 0,
            dy: 0,
            buttons: 0
        }
    );
}

#[test]
fn stray_mouse_bytes_and_partial_packet_reset_are_bounded() {
    let mut decoder = MouseDecoder::new();
    assert!(decoder.push(0).is_none());
    decoder.push(0x08);
    decoder.reset();
    assert_eq!(packet(&mut decoder, [0x08, 12, 2]).dx, 12);
}

#[test]
fn fifo_retains_quick_press_and_release_between_polls() {
    let mut queue = EventQueue::<MousePacket, 2>::new();
    let mut decoder = MouseDecoder::new();
    queue.push(packet(&mut decoder, [0x09, 0, 0]));
    queue.push(packet(&mut decoder, [0x08, 0, 0]));
    assert_eq!(queue.pop().unwrap().buttons, 1);
    assert_eq!(queue.pop().unwrap().buttons, 0);
    assert!(queue.pop().is_none());
}

#[test]
fn fifo_overflow_is_counted_and_wraparound_preserves_order() {
    let mut queue = EventQueue::<u8, 2>::new();
    assert!(queue.push(1));
    assert!(queue.push(2));
    assert!(!queue.push(3));
    assert_eq!(queue.pop(), Some(1));
    assert!(queue.push(4));
    assert_eq!(queue.pop(), Some(2));
    assert_eq!(queue.pop(), Some(4));
    assert_eq!(queue.received(), 4);
    assert_eq!(queue.dropped(), 1);
    let mut empty = EventQueue::<u8, 0>::new();
    assert!(!empty.push(1));
    assert_eq!(empty.pop(), None);
}

#[test]
fn arbitrary_byte_streams_do_not_panic() {
    let mut keyboard = Keyboard::new();
    let mut mouse = MouseDecoder::new();
    for _ in 0..8 {
        for byte in 0..=255 {
            let _ = keyboard.push(byte);
            let _ = mouse.push(byte);
        }
    }
}
