//! Pure wire protocol for the Ulanzi AU05 Vibe Key vendor HID interface.
//!
//! The vendor collection carries 64-byte reports with report ID `0x55`. The
//! plaintext is a 4-byte command header, then the payload, right-padded with
//! zeros to 64 bytes. The buffer is TEA-encrypted in 8-byte blocks and only
//! the first 63 ciphertext bytes travel on the wire; responses use the same
//! encoding. Everything in this module is platform independent so the wire
//! format is testable on every build host; the Windows HID adapter owns the
//! device I/O.
#![cfg_attr(not(windows), allow(dead_code))]

use vibdock_input::hotkey::{Hotkey, Key, Modifier};

/// USB vendor ID of the AU05 vendor collection.
pub const VENDOR_ID: u16 = 0xfff1;
/// USB product ID of the AU05 vendor collection.
pub const PRODUCT_ID: u16 = 0x00dd;
/// HID report ID of every vendor frame.
pub const REPORT_ID: u8 = 0x55;
/// Full report length in bytes, report ID included.
pub const REPORT_LEN: usize = 64;
/// Plaintext bytes available to a payload after the 4-byte header.
const PAYLOAD_LEN: usize = REPORT_LEN - 4;
/// Ciphertext bytes carried on the wire after the report ID.
const CIPHER_LEN: usize = REPORT_LEN - 1;

/// Header of "read shortcut slot".
pub const GET_SHORTCUT_HEADER: [u8; 4] = [0x01, 0x06, 0x50, 0x01];
/// Header of "write shortcut slot".
pub const SET_SHORTCUT_HEADER: [u8; 4] = [0x01, 0x06, 0x50, 0x04];
/// Header of "read fixed function".
pub const GET_FIXED_FUNCTION_HEADER: [u8; 4] = [0x01, 0x06, 0x10, 0x01];
/// Header of "write fixed function".
pub const SET_FIXED_FUNCTION_HEADER: [u8; 4] = [0x01, 0x06, 0x10, 0x04];
/// Header of "read battery state".
///
/// Part of the vendor command surface; no caller reads the battery yet.
#[allow(dead_code)]
pub const GET_BATTERY_HEADER: [u8; 4] = [0x01, 0x01, 0x02, 0x01];
/// Header of the keep-alive heartbeat.
pub const HEARTBEAT_HEADER: [u8; 4] = [0x06, 0x01, 0x23, 0x00];

/// One of the three round buttons the vendor protocol manages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Button(u8);

impl Button {
    /// The button at a device slot index. Only slots 0..=2 are managed.
    pub fn from_index(index: u8) -> Option<Self> {
        (index < 3).then_some(Button(index))
    }

    /// The device slot index of this button.
    pub fn index(self) -> u8 {
        self.0
    }

    /// A human-readable name for diagnostics.
    pub fn label(self) -> &'static str {
        match self.0 {
            0 => "Button 1",
            1 => "Button 2",
            _ => "Button 3",
        }
    }

    /// All managed buttons, in slot order.
    pub fn all() -> [Button; 3] {
        [Button(0), Button(1), Button(2)]
    }
}

/// One shortcut entry: a HID page, its usage value, and the left/right sign
/// bit the wire format carries in the page byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyEntry {
    /// HID usage page: 2 is the keyboard/consumer page, 3 is the modifier map.
    pub page: u8,
    /// HID usage ID on that page, or the modifier bit value.
    pub value: u8,
    /// 1 for a left-side modifier, 0 otherwise.
    pub sign: u8,
}

/// The entries a hotkey turns into, modifiers first, then the key.
///
/// The device understands a closed key set; anything outside it is rejected
/// with an error naming the key rather than silently guessed, because a
/// shortcut that reaches the device as some other key would fire the wrong
/// action with no way to notice.
pub fn hotkey_entries(hotkey: &Hotkey) -> Result<Vec<KeyEntry>, String> {
    let mut entries = Vec::with_capacity(hotkey.modifiers().len() + 1);
    for modifier in hotkey.modifiers() {
        entries.push(modifier_entry(*modifier));
    }
    entries.push(key_entry(hotkey.key(), hotkey.modifiers())?);
    Ok(entries)
}

/// A payload that writes one shortcut slot: `[index, 0x01, count, entries…]`.
#[cfg_attr(not(test), allow(dead_code))]
pub fn shortcut_payload(button: Button, hotkey: &Hotkey) -> Result<Vec<u8>, String> {
    let entries = hotkey_entries(hotkey)?;
    Ok(shortcut_payload_entries(button, &entries))
}

/// A payload that writes one shortcut slot from already-derived entries.
pub fn shortcut_payload_entries(button: Button, entries: &[KeyEntry]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(3 + entries.len() * 2);
    payload.extend_from_slice(&[button.index(), 0x01, entries.len() as u8]);
    for entry in entries {
        payload.extend_from_slice(&shortcut_entry_wire_bytes(*entry));
    }
    payload
}

/// Parses a chord for the direct-firmware path.
///
/// Unlike a registered trigger, a direct chord may be a bare key such as
/// `Enter` or `Esc`: the firmware emits it as a normal key press and no host
/// hotkey is involved. A bare Windows key is refused because it would invoke
/// the shell start menu rather than a user action.
pub fn direct_entries(text: &str) -> Result<Vec<KeyEntry>, String> {
    if let Ok(hotkey) = Hotkey::parse(text) {
        return hotkey_entries(&hotkey);
    }
    let key = Key::parse(text)
        .ok_or_else(|| format!("{text:?} is not a supported chord or key for an AU05 button"))?;
    if matches!(key, Key::Win) {
        return Err("Win alone is not a usable direct chord".to_string());
    }
    key_entry(key, &[]).map(|entry| vec![entry])
}

/// The canonical display form of a direct chord.
pub fn direct_canonical(text: &str) -> Result<String, String> {
    if let Ok(hotkey) = Hotkey::parse(text) {
        return Ok(hotkey.as_string());
    }
    let key = Key::parse(text)
        .ok_or_else(|| format!("{text:?} is not a supported chord or key for an AU05 button"))?;
    if matches!(key, Key::Win) {
        return Err("Win alone is not a usable direct chord".to_string());
    }
    Ok(key.as_string())
}

/// A payload that clears one shortcut slot.
pub fn clear_shortcut_payload(button: Button) -> [u8; 3] {
    [button.index(), 0x01, 0x00]
}

/// A payload that clears the fixed function bound to one button.
pub fn fixed_function_clear_payload(button: Button) -> [u8; 3] {
    [0x00, button.index(), 0x00]
}

/// A payload that reads the fixed function bound to one button.
pub fn fixed_function_get_payload(button: Button) -> [u8; 2] {
    [0x00, button.index()]
}

/// Encodes one report: `[0x55]` followed by the encrypted plaintext prefix.
///
/// The header occupies plaintext bytes 0..4, the payload follows, and the
/// rest is zero. A payload longer than 60 bytes is truncated; the final
/// ciphertext byte never reaches the wire.
pub fn encode_frame(header: [u8; 4], payload: &[u8]) -> [u8; REPORT_LEN] {
    let mut plain = [0u8; REPORT_LEN];
    plain[..4].copy_from_slice(&header);
    let length = payload.len().min(PAYLOAD_LEN);
    plain[4..4 + length].copy_from_slice(&payload[..length]);
    tea_encrypt(&mut plain);
    let mut report = [0u8; REPORT_LEN];
    report[0] = REPORT_ID;
    report[1..].copy_from_slice(&plain[..CIPHER_LEN]);
    report
}

/// Decodes one received report into its plaintext buffer.
///
/// The report ID is stripped, the remaining ciphertext is decrypted in
/// 8-byte blocks, and the result is zero-padded into a full-size buffer.
/// Returns `None` when the bytes are not a report for this report ID.
pub fn decode_report(report: &[u8]) -> Option<[u8; REPORT_LEN]> {
    if report.len() < 2 || report[0] != REPORT_ID {
        return None;
    }
    let cipher = &report[1..];
    let length = cipher.len().min(REPORT_LEN);
    let mut plain = [0u8; REPORT_LEN];
    plain[..length].copy_from_slice(&cipher[..length]);
    tea_decrypt(&mut plain[..length]);
    Some(plain)
}

/// Whether a plaintext reply answers a request with the given header.
///
/// The device sets the high bit of the first byte and echoes `b1`/`b2`; `b3`
/// carries the reply direction and must not be compared.
pub fn response_matches(plain: &[u8; REPORT_LEN], header: [u8; 4]) -> bool {
    plain[0] & 0x80 != 0 && plain[1] == header[1] && plain[2] == header[2]
}

/// A decoded shortcut-slot readback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShortcutReadback {
    /// Device slot index the reply describes.
    pub button: u8,
    /// Stored entries in device order.
    pub entries: Vec<KeyEntry>,
}

/// Parses a shortcut-slot readback reply.
///
/// The reply data is `[button_index, 0x01, count, (pageSign, value) * count]`.
/// Malformed lengths and replies to a different command are rejected.
pub fn parse_shortcut_readback(plain: &[u8; REPORT_LEN]) -> Option<ShortcutReadback> {
    if !response_matches(plain, GET_SHORTCUT_HEADER) || plain[5] != 0x01 {
        return None;
    }
    let count = usize::from(plain[6]);
    if 7 + count * 2 > REPORT_LEN {
        return None;
    }
    let entries = (0..count)
        .map(|index| entry_from_wire(plain[7 + 2 * index], plain[8 + 2 * index]))
        .collect();
    Some(ShortcutReadback {
        button: plain[4],
        entries,
    })
}

/// Whether a stored shortcut readback holds exactly the hotkey's entries.
///
/// Order and the left/right modifier signs are part of the comparison: a
/// right-side chord stored on the device is not the same binding as the
/// left-side chord a recorded hotkey describes.
#[cfg_attr(not(test), allow(dead_code))]
pub fn shortcut_readback_matches(
    plain: &[u8; REPORT_LEN],
    button: Button,
    hotkey: &Hotkey,
) -> bool {
    let Ok(expected) = hotkey_entries(hotkey) else {
        return false;
    };
    shortcut_readback_matches_entries(plain, button, &expected)
}

/// Whether a stored readback holds exactly the given entries, in order.
pub fn shortcut_readback_matches_entries(
    plain: &[u8; REPORT_LEN],
    button: Button,
    expected: &[KeyEntry],
) -> bool {
    let Some(readback) = parse_shortcut_readback(plain) else {
        return false;
    };
    readback.button == button.index() && readback.entries == expected
}

/// Parses a fixed-function readback reply into `(button_index, func_index)`.
///
/// The reply data is `[0x00, button_index, func_index]`; the fixed function
/// is a separate storage slot from the shortcut, and 0 means "none".
pub fn parse_fixed_function_readback(plain: &[u8; REPORT_LEN]) -> Option<(u8, u8)> {
    if !response_matches(plain, GET_FIXED_FUNCTION_HEADER) || plain[4] != 0x00 {
        return None;
    }
    Some((plain[5], plain[6]))
}

/// Maps one modifier onto its modifier-page entry. Left side only.
///
/// The AU05 modifier page carries the same bits a HID keyboard uses: bit 0x08
/// is the Windows/GUI key. Writing `Ctrl + Win` as direct firmware bytes was
/// verified on a physical device: the host received it as a real Ctrl+Win key
/// chord (`83 01 83 08` read back identically). This is why Win is accepted
/// here but still refused as a *registered* VibDock trigger: firmware emission
/// is a physical-key path, while `RegisterHotKey` never receives MOD_WIN.
fn modifier_entry(modifier: Modifier) -> KeyEntry {
    let value = match modifier {
        Modifier::Control => 0x01,
        Modifier::Alt => 0x04,
        Modifier::Shift => 0x02,
        Modifier::Win => 0x08,
    };
    KeyEntry {
        page: 3,
        value,
        sign: 1,
    }
}

/// Maps one non-modifier key onto its keyboard-page entry.
///
/// `+` needs the Shift modifier already present: without it the device would
/// emit `=` for the same usage.
fn key_entry(key: Key, modifiers: &[Modifier]) -> Result<KeyEntry, String> {
    let value = match key {
        Key::Function(number) => match number {
            1..=12 => 0x39 + number,
            _ => {
                return Err(format!(
                    "{} is not representable by the AU05 (only F1-F12 are)",
                    key.as_string()
                ))
            }
        },
        Key::Character('+') if !modifiers.contains(&Modifier::Shift) => {
            return Err("'+' requires Shift; without it the AU05 would send '='".to_string());
        }
        Key::Character('+') => 0x2e,
        Key::Character(character) => character_value(character)?,
        Key::Escape => 0x29,
        Key::Delete => 0x4c,
        Key::Insert => 0x49,
        Key::Home => 0x4a,
        Key::End => 0x4d,
        Key::PageUp => 0x4b,
        Key::PageDown => 0x4e,
        Key::Up => 0x52,
        Key::Down => 0x51,
        Key::Left => 0x50,
        Key::Right => 0x4f,
        Key::Enter => 0x28,
        Key::Tab => 0x2b,
        Key::Backspace => 0x2a,
        // The Windows key as the key half of `Ctrl + Win`: the modifier page
        // value 0x08, left side. This is a firmware-emission path only; it is
        // never a registered trigger.
        Key::Win => {
            return Ok(KeyEntry {
                page: 3,
                value: 0x08,
                sign: 1,
            })
        }
    };
    Ok(KeyEntry {
        page: 2,
        value,
        sign: 0,
    })
}

/// Maps a single printable character onto its keyboard-page usage ID.
fn character_value(character: char) -> Result<u8, String> {
    let upper = character.to_ascii_uppercase();
    let value = match upper {
        'A'..='Z' => 0x04 + (upper as u8 - b'A'),
        '1'..='9' => 0x1e + (upper as u8 - b'1'),
        '0' => 0x27,
        ' ' => 0x2c,
        '-' => 0x2d,
        '=' => 0x2e,
        '[' => 0x2f,
        ']' => 0x30,
        '\\' => 0x31,
        ';' => 0x33,
        '\'' => 0x34,
        '`' => 0x35,
        ',' => 0x36,
        '.' => 0x37,
        '/' => 0x38,
        _ => return Err(format!("{character:?} is not a key the AU05 can send")),
    };
    Ok(value)
}

/// The wire form of one entry in a SET payload.
///
/// The second byte is the page (`2` keyboard, `3` modifier); the high bit of
/// the first byte marks a left-side modifier. A left Ctrl entry is therefore
/// `[0x83, 0x01]`, which the device reads back unchanged. This symmetry was
/// confirmed on a physical AU05 firmware 4.4.0 on 2026-10-19: writing
/// `83 01 83 04 83 02 02 42` read back as `83 01 83 04 83 02 02 42`, while the
/// reference implementation's oracle vectors show the same `page | sign`
/// encoding. Readback and write byte layouts therefore match.
fn shortcut_entry_wire_bytes(entry: KeyEntry) -> [u8; 2] {
    let sign = u8::from(entry.sign != 0) << 7;
    [(entry.page & 0x7f) | sign, entry.value]
}

/// Decodes one readback entry: the sign bit sits on top of the page byte.
fn entry_from_wire(page_sign: u8, value: u8) -> KeyEntry {
    KeyEntry {
        page: page_sign & 0x7f,
        value,
        sign: u8::from(page_sign & 0x80 != 0),
    }
}

/// TEA round constant.
const TEA_DELTA: u32 = 0x9e37_79b9;
/// TEA rounds per block.
const TEA_ROUNDS: u32 = 32;
/// The device key as four little-endian 32-bit words.
const TEA_KEY: [u32; 4] = [0xcaa5_baca, 0xbc2a_8a6d, 0xca5a_9eba, 0x9bb8_8bca];

/// Encrypts every complete 8-byte block in place; trailing bytes stay as-is.
fn tea_encrypt(bytes: &mut [u8]) {
    for block in bytes.as_chunks_mut::<8>().0 {
        let (v0, v1) = encrypt_block(u32_at(block, 0), u32_at(block, 4));
        block[..4].copy_from_slice(&v0.to_le_bytes());
        block[4..].copy_from_slice(&v1.to_le_bytes());
    }
}

/// Decrypts every complete 8-byte block in place; trailing bytes stay as-is.
fn tea_decrypt(bytes: &mut [u8]) {
    for block in bytes.as_chunks_mut::<8>().0 {
        let (v0, v1) = decrypt_block(u32_at(block, 0), u32_at(block, 4));
        block[..4].copy_from_slice(&v0.to_le_bytes());
        block[4..].copy_from_slice(&v1.to_le_bytes());
    }
}

fn encrypt_block(mut v0: u32, mut v1: u32) -> (u32, u32) {
    let mut sum = 0u32;
    for _ in 0..TEA_ROUNDS {
        sum = sum.wrapping_add(TEA_DELTA);
        v0 = v0.wrapping_add(
            (v1 << 4).wrapping_add(TEA_KEY[0])
                ^ v1.wrapping_add(sum)
                ^ (v1 >> 5).wrapping_add(TEA_KEY[1]),
        );
        v1 = v1.wrapping_add(
            (v0 << 4).wrapping_add(TEA_KEY[2])
                ^ v0.wrapping_add(sum)
                ^ (v0 >> 5).wrapping_add(TEA_KEY[3]),
        );
    }
    (v0, v1)
}

fn decrypt_block(mut v0: u32, mut v1: u32) -> (u32, u32) {
    let mut sum = TEA_DELTA.wrapping_mul(TEA_ROUNDS);
    for _ in 0..TEA_ROUNDS {
        v1 = v1.wrapping_sub(
            (v0 << 4).wrapping_add(TEA_KEY[2])
                ^ v0.wrapping_add(sum)
                ^ (v0 >> 5).wrapping_add(TEA_KEY[3]),
        );
        v0 = v0.wrapping_sub(
            (v1 << 4).wrapping_add(TEA_KEY[0])
                ^ v1.wrapping_add(sum)
                ^ (v1 >> 5).wrapping_add(TEA_KEY[1]),
        );
        sum = sum.wrapping_sub(TEA_DELTA);
    }
    (v0, v1)
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real captured vendor reply, report ID included.
    const CAPTURED_RESPONSE: &str = concat!(
        "55 2E 40 A8 A9 9C 0F 8D 5F E6 CD 35 44 D7 49 D8 ",
        "B3 52 E5 A8 22 E2 F1 7C 2D 38 90 C4 99 A3 60 AA ",
        "AD 38 90 C4 99 A3 60 AA AD 38 90 C4 99 A3 60 AA ",
        "AD 38 90 C4 99 A3 60 AA AD 75 22 13 38 9A 61 C3",
    );

    /// The frame the device expects for a four-entry shortcut on button 1.
    ///
    /// Payload `00 01 04 83 01 83 04 83 02 02 42` (left Ctrl, Alt, Shift, F9)
    /// was written to and read back identically from a physical AU05 on
    /// 2026-10-19.
    const EXPECTED_SET_SHORTCUT_FRAME: &str = concat!(
        "55 F2 F5 AA 0A 2A 21 94 D1 7D 54 CC 50 26 62 03 ",
        "88 38 90 C4 99 A3 60 AA AD 38 90 C4 99 A3 60 AA ",
        "AD 38 90 C4 99 A3 60 AA AD 38 90 C4 99 A3 60 AA ",
        "AD 38 90 C4 99 A3 60 AA AD 38 90 C4 99 A3 60 AA",
    );

    /// A real shortcut-slot readback plaintext for button 0 holding a
    /// right-side Ctrl+Alt+Shift+F9 chord. The last ciphertext block is only
    /// partially present on the wire, so its tail is not decrypted and the
    /// buffer is zero-extended to 64 bytes.
    const RIGHT_SIDE_READBACK: &str = concat!(
        "81 06 50 11 00 01 04 03 01 03 04 03 02 02 42 00 ",
        "00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 ",
        "00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 ",
        "00 00 00 00 00 00 00 00 75 22 13 38 9A 61 C3 00",
    );

    fn hex_bytes(text: &str) -> Vec<u8> {
        text.split_whitespace()
            .map(|token| u8::from_str_radix(token, 16).expect("fixture is hexadecimal"))
            .collect()
    }

    fn array_from_hex(text: &str) -> [u8; REPORT_LEN] {
        let bytes = hex_bytes(text);
        assert!(bytes.len() <= REPORT_LEN, "fixture must fit one report");
        let mut array = [0u8; REPORT_LEN];
        array[..bytes.len()].copy_from_slice(&bytes);
        array
    }

    fn response(header: [u8; 4], data: &[u8]) -> [u8; REPORT_LEN] {
        let mut plain = [0u8; REPORT_LEN];
        plain[..4].copy_from_slice(&header);
        plain[4..4 + data.len()].copy_from_slice(data);
        plain
    }

    #[test]
    fn exposes_the_vendor_collection_identity() {
        assert_eq!(VENDOR_ID, 0xfff1);
        assert_eq!(PRODUCT_ID, 0x00dd);
        assert_eq!(REPORT_ID, 0x55);
        assert_eq!(REPORT_LEN, 64);
        assert_eq!(GET_SHORTCUT_HEADER, [0x01, 0x06, 0x50, 0x01]);
        assert_eq!(SET_SHORTCUT_HEADER, [0x01, 0x06, 0x50, 0x04]);
        assert_eq!(GET_FIXED_FUNCTION_HEADER, [0x01, 0x06, 0x10, 0x01]);
        assert_eq!(SET_FIXED_FUNCTION_HEADER, [0x01, 0x06, 0x10, 0x04]);
        assert_eq!(GET_BATTERY_HEADER, [0x01, 0x01, 0x02, 0x01]);
        assert_eq!(HEARTBEAT_HEADER, [0x06, 0x01, 0x23, 0x00]);
    }

    #[test]
    fn decodes_a_captured_response() {
        let report = hex_bytes(CAPTURED_RESPONSE);
        assert_eq!(report.len(), REPORT_LEN);
        let plain = decode_report(&report).expect("captured report must decode");
        assert_eq!(&plain[..4], &[0x81, 0x04, 0x04, 0x11]);
    }

    #[test]
    fn decodes_reports_only_for_the_vendor_report_id() {
        assert!(decode_report(&[]).is_none());
        assert!(decode_report(&[REPORT_ID]).is_none());
        assert!(decode_report(&[0x00, 0x01]).is_none());
    }

    #[test]
    fn encodes_the_expected_set_shortcut_frame() {
        let payload = [
            0x00, 0x01, 0x04, 0x83, 0x01, 0x83, 0x04, 0x83, 0x02, 0x02, 0x42,
        ];
        let frame = encode_frame(SET_SHORTCUT_HEADER, &payload);
        assert_eq!(frame, array_from_hex(EXPECTED_SET_SHORTCUT_FRAME));
        assert_eq!(frame[0], REPORT_ID);
    }

    #[test]
    fn encodes_and_decodes_arbitrary_payloads() {
        let cases: &[&[u8]] = &[
            &[],
            &[0x00],
            &[
                0x00, 0x01, 0x04, 0x83, 0x01, 0x83, 0x04, 0x83, 0x02, 0x02, 0x42,
            ],
            &[0xaa; 52],
        ];
        for payload in cases {
            let frame = encode_frame(SET_FIXED_FUNCTION_HEADER, payload);
            let plain = decode_report(&frame).expect("own frame must decode");
            assert_eq!(&plain[..4], &SET_FIXED_FUNCTION_HEADER);
            assert_eq!(&plain[4..4 + payload.len()], *payload);
            assert!(plain[4 + payload.len()..56].iter().all(|byte| *byte == 0));
        }
    }

    #[test]
    fn truncates_payloads_longer_than_the_frame() {
        let long = encode_frame(GET_BATTERY_HEADER, &[0x11; 70]);
        let full = encode_frame(GET_BATTERY_HEADER, &[0x11; 60]);
        assert_eq!(long, full);
    }

    #[test]
    fn hotkey_maps_to_device_entries() {
        let hotkey = Hotkey::parse("Ctrl + Alt + Shift + F9").expect("fixture hotkey parses");
        assert_eq!(
            hotkey_entries(&hotkey),
            Ok(vec![
                KeyEntry {
                    page: 3,
                    value: 0x01,
                    sign: 1
                },
                KeyEntry {
                    page: 3,
                    value: 0x04,
                    sign: 1
                },
                KeyEntry {
                    page: 3,
                    value: 0x02,
                    sign: 1
                },
                KeyEntry {
                    page: 2,
                    value: 0x42,
                    sign: 0
                },
            ])
        );
        let button = Button::from_index(0).expect("button 1 exists");
        assert_eq!(
            shortcut_payload(button, &hotkey),
            Ok(vec![
                0x00, 0x01, 0x04, 0x83, 0x01, 0x83, 0x04, 0x83, 0x02, 0x02, 0x42
            ])
        );
    }

    #[test]
    fn parses_a_real_shortcut_readback() {
        let plain = array_from_hex(RIGHT_SIDE_READBACK);
        let readback = parse_shortcut_readback(&plain).expect("readback must parse");
        assert_eq!(readback.button, 0);
        assert_eq!(
            readback.entries,
            vec![
                KeyEntry {
                    page: 3,
                    value: 0x01,
                    sign: 0
                },
                KeyEntry {
                    page: 3,
                    value: 0x04,
                    sign: 0
                },
                KeyEntry {
                    page: 3,
                    value: 0x02,
                    sign: 0
                },
                KeyEntry {
                    page: 2,
                    value: 0x42,
                    sign: 0
                },
            ]
        );
    }

    #[test]
    fn rejects_malformed_shortcut_readbacks() {
        let wrong_command = response([0x81, 0x06, 0x10, 0x11], &[0x00, 0x01, 0x04]);
        assert!(parse_shortcut_readback(&wrong_command).is_none());
        let too_many = response([0x81, 0x06, 0x50, 0x11], &[0x00, 0x01, 0xff]);
        assert!(parse_shortcut_readback(&too_many).is_none());
        let mut no_high_bit = response([0x81, 0x06, 0x50, 0x11], &[0x00, 0x01, 0x00]);
        no_high_bit[0] = 0x01;
        assert!(parse_shortcut_readback(&no_high_bit).is_none());
    }

    #[test]
    fn readback_matching_requires_identical_entries_in_order() {
        let button = Button::from_index(0).expect("button 1 exists");
        let hotkey = Hotkey::parse("Ctrl + Alt + Shift + F9").expect("fixture hotkey parses");
        // The stored chord uses right-side modifiers (sign 0), so it is a
        // different binding from the recorded left-side chord.
        let right_side = array_from_hex(RIGHT_SIDE_READBACK);
        assert!(!shortcut_readback_matches(&right_side, button, &hotkey));

        let stored = response(
            [0x81, 0x06, 0x50, 0x11],
            &[
                0x00, 0x01, 0x04, 0x83, 0x01, 0x83, 0x04, 0x83, 0x02, 0x02, 0x42,
            ],
        );
        assert!(shortcut_readback_matches(&stored, button, &hotkey));
        assert!(!shortcut_readback_matches(
            &stored,
            Button::from_index(1).expect("button 2 exists"),
            &hotkey
        ));

        // Order is part of the stored binding.
        let swapped = response(
            [0x81, 0x06, 0x50, 0x11],
            &[
                0x00, 0x01, 0x04, 0x83, 0x04, 0x83, 0x01, 0x83, 0x02, 0x02, 0x42,
            ],
        );
        assert!(!shortcut_readback_matches(&swapped, button, &hotkey));

        // An unrepresentable hotkey can never match a stored binding.
        let unsupported = Hotkey::new([Modifier::Control], Key::Function(13));
        assert!(!shortcut_readback_matches(&stored, button, &unsupported));
    }

    #[test]
    fn direct_chords_accept_bare_enter_and_escape() {
        for (text, value, display) in [("Enter", 0x28u8, "Enter"), ("Esc", 0x29u8, "Escape")] {
            let entries = direct_entries(text).expect("bare key direct chord");
            assert_eq!(
                entries,
                vec![KeyEntry {
                    page: 2,
                    value,
                    sign: 0
                }]
            );
            assert_eq!(direct_canonical(text).unwrap(), display);
            let button = Button::from_index(2).expect("button 3");
            assert_eq!(
                shortcut_payload_entries(button, &entries),
                vec![0x02, 0x01, 0x01, 0x02, value]
            );
            let stored = response([0x81, 0x06, 0x50, 0x11], &[0x02, 0x01, 0x01, 0x02, value]);
            assert!(shortcut_readback_matches_entries(&stored, button, &entries));
        }
        // The combined chord still goes through the hotkey path.
        assert_eq!(direct_canonical("Ctrl + Win").unwrap(), "Ctrl + Win");
        assert!(direct_entries("Win").is_err());
        assert!(direct_entries("NotAChord").is_err());
    }

    #[test]
    fn builds_clear_and_fixed_function_payloads() {
        for button in Button::all() {
            assert_eq!(clear_shortcut_payload(button), [button.index(), 0x01, 0x00]);
            assert_eq!(
                fixed_function_clear_payload(button),
                [0x00, button.index(), 0x00]
            );
            assert_eq!(fixed_function_get_payload(button), [0x00, button.index()]);
        }
        assert_eq!(clear_shortcut_payload(Button(0)), [0x00, 0x01, 0x00]);
        assert_eq!(fixed_function_clear_payload(Button(2)), [0x00, 0x02, 0x00]);
    }

    #[test]
    fn parses_a_fixed_function_readback() {
        let plain = response([0x81, 0x06, 0x10, 0x11], &[0x00, 0x02, 0x00]);
        assert_eq!(parse_fixed_function_readback(&plain), Some((2, 0)));
        let non_zero_prefix = response([0x81, 0x06, 0x10, 0x11], &[0x01, 0x02, 0x00]);
        assert!(parse_fixed_function_readback(&non_zero_prefix).is_none());
        let wrong_command = response([0x81, 0x06, 0x50, 0x11], &[0x00, 0x02, 0x00]);
        assert!(parse_fixed_function_readback(&wrong_command).is_none());
    }

    #[test]
    fn response_matching_ignores_the_direction_byte() {
        let plain = response([0x81, 0x06, 0x50, 0x11], &[0x00]);
        assert!(response_matches(&plain, GET_SHORTCUT_HEADER));
        assert!(response_matches(&plain, [0x01, 0x06, 0x50, 0x00]));
        let mut no_high_bit = plain;
        no_high_bit[0] = 0x01;
        assert!(!response_matches(&no_high_bit, GET_SHORTCUT_HEADER));
        let other_command = response([0x81, 0x06, 0x10, 0x11], &[0x00]);
        assert!(!response_matches(&other_command, GET_SHORTCUT_HEADER));
    }

    fn key_usage(key: Key) -> u8 {
        let entries =
            hotkey_entries(&Hotkey::new([Modifier::Control], key)).expect("representable key");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].page, 2);
        assert_eq!(entries[1].sign, 0);
        entries[1].value
    }

    #[test]
    fn representable_keys_map_to_device_usage_ids() {
        for offset in 0..26u8 {
            assert_eq!(
                key_usage(Key::Character(char::from(b'A' + offset))),
                0x04 + offset
            );
        }
        for digit in 1..=9u8 {
            assert_eq!(
                key_usage(Key::Character(char::from(b'0' + digit))),
                0x1d + digit
            );
        }
        assert_eq!(key_usage(Key::Character('0')), 0x27);
        for number in 1..=12u8 {
            assert_eq!(key_usage(Key::Function(number)), 0x39 + number);
        }

        let named = [
            (Key::Character(' '), 0x2c),
            (Key::Enter, 0x28),
            (Key::Escape, 0x29),
            (Key::Backspace, 0x2a),
            (Key::Tab, 0x2b),
            (Key::Delete, 0x4c),
            (Key::Insert, 0x49),
            (Key::Home, 0x4a),
            (Key::End, 0x4d),
            (Key::PageUp, 0x4b),
            (Key::PageDown, 0x4e),
            (Key::Up, 0x52),
            (Key::Down, 0x51),
            (Key::Left, 0x50),
            (Key::Right, 0x4f),
        ];
        for (key, value) in named {
            assert_eq!(key_usage(key), value, "{key:?}");
        }

        let punctuation = [
            ('-', 0x2d),
            ('=', 0x2e),
            ('[', 0x2f),
            (']', 0x30),
            ('\\', 0x31),
            (';', 0x33),
            ('\'', 0x34),
            ('`', 0x35),
            (',', 0x36),
            ('.', 0x37),
            ('/', 0x38),
        ];
        for (character, value) in punctuation {
            assert_eq!(key_usage(Key::Character(character)), value, "{character:?}");
        }

        // Lowercase is normalised: recorded keys are stored uppercase.
        assert_eq!(key_usage(Key::Character('a')), 0x04);
    }

    #[test]
    fn rejects_keys_the_device_cannot_express() {
        for number in 13..=24u8 {
            let error = hotkey_entries(&Hotkey::new([Modifier::Control], Key::Function(number)))
                .expect_err("F13-F24 are not representable");
            assert!(error.contains(&format!("F{number}")), "got: {error}");
        }
        for character in ['?', '!', '@', '#', '\u{4e2d}'] {
            let error =
                hotkey_entries(&Hotkey::new([Modifier::Control], Key::Character(character)))
                    .expect_err("unsupported character must be rejected");
            assert!(
                error.contains(&character.to_string()),
                "error must name the key, got: {error}"
            );
        }
        let parsed = Hotkey::parse("Ctrl + \u{4e2d}").expect("single character parses");
        assert!(hotkey_entries(&parsed).is_err());
        let win = Hotkey::parse("Ctrl + Win").expect("Win parses as a dispatch modifier");
        assert_eq!(
            hotkey_entries(&win),
            Ok(vec![
                KeyEntry {
                    page: 3,
                    value: 0x01,
                    sign: 1
                },
                KeyEntry {
                    page: 3,
                    value: 0x08,
                    sign: 1
                }
            ])
        );
        let button = Button::from_index(2).expect("button 3");
        assert_eq!(
            shortcut_payload(button, &win),
            Ok(vec![0x02, 0x01, 0x02, 0x83, 0x01, 0x83, 0x08])
        );
    }

    #[test]
    fn plus_requires_shift() {
        let mut hotkey = Hotkey::new([Modifier::Control], Key::Character('+'));
        let error = hotkey_entries(&hotkey).expect_err("'+' without Shift would send '='");
        assert!(error.contains("Shift"), "got: {error}");
        assert!(shortcut_payload(Button::from_index(0).expect("button 1"), &hotkey).is_err());

        hotkey = Hotkey::parse("Ctrl + Shift + Plus").expect("'Plus' is a recorded key name");
        assert_eq!(
            hotkey_entries(&hotkey),
            Ok(vec![
                KeyEntry {
                    page: 3,
                    value: 0x01,
                    sign: 1
                },
                KeyEntry {
                    page: 3,
                    value: 0x02,
                    sign: 1
                },
                KeyEntry {
                    page: 2,
                    value: 0x2e,
                    sign: 0
                },
            ])
        );
    }

    #[test]
    fn buttons_cover_only_the_three_managed_slots() {
        assert!(Button::from_index(3).is_none());
        assert!(Button::from_index(255).is_none());
        assert_eq!(Button::all().map(Button::index), [0, 1, 2]);
        assert_eq!(
            Button::all().map(Button::label),
            ["Button 1", "Button 2", "Button 3"]
        );
        for button in Button::all() {
            assert_eq!(Button::from_index(button.index()), Some(button));
        }
    }
}
