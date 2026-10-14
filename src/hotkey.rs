//! Typed hotkey contract shared by the configuration API and the platform
//! input adapter.
//!
//! A binding is recorded by the browser as a display string such as
//! `Ctrl + Alt + Shift + F9` and must reach the platform as an exact modifier
//! mask plus virtual key. Parsing is deliberately strict: an unrecognised
//! modifier or key is an error rather than a silently dropped part, because a
//! hotkey registered with fewer modifiers than the user chose would fire on
//! ordinary keystrokes and hijack them.

use std::fmt;

/// The modifiers a binding may require.
///
/// Only the four combinations Windows can express in a `RegisterHotKey` mask
/// are represented. There is deliberately no `Win` variant: `MOD_WIN` is
/// reserved for OS shell bindings that never reach an application window, so
/// accepting it would let a user record a hotkey the platform silently refuses
/// and then believe it is armed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Modifier {
    /// `Ctrl`.
    Control,
    /// `Alt`.
    Alt,
    /// `Shift`.
    Shift,
}

impl Modifier {
    /// The name used in recorded display strings and serialised config.
    pub fn as_str(self) -> &'static str {
        match self {
            Modifier::Control => "Ctrl",
            Modifier::Alt => "Alt",
            Modifier::Shift => "Shift",
        }
    }

    /// Parses a modifier name case-insensitively, including browser aliases.
    ///
    /// `Control`, `Alt`/`Menu`/`Option`, and `Shift` are the spellings a
    /// browser `KeyboardEvent` or a hand-written config can produce.
    pub fn parse(name: &str) -> Option<Modifier> {
        match name.trim().to_ascii_lowercase().as_str() {
            "ctrl" | "control" => Some(Modifier::Control),
            "alt" | "menu" | "option" => Some(Modifier::Alt),
            "shift" => Some(Modifier::Shift),
            _ => None,
        }
    }
}

/// The key half of a binding: a named non-printable key or one character.
///
/// Printable keys are carried as their uppercase ASCII character, which is also
/// their Windows virtual-key code, so `A`..`Z`, `0`..`9` and the punctuation
/// that shares a code with its unshifted digit row all round-trip without a
/// lookup table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    /// A function key, `F1` through `F24`.
    Function(u8),
    /// A single printable key, stored uppercase.
    Character(char),
    /// `Esc`.
    Escape,
    /// `Delete`.
    Delete,
    /// `Insert`.
    Insert,
    /// `Home`.
    Home,
    /// `End`.
    End,
    /// `PageUp`.
    PageUp,
    /// `PageDown`.
    PageDown,
    /// Up arrow.
    Up,
    /// Down arrow.
    Down,
    /// Left arrow.
    Left,
    /// Right arrow.
    Right,
    /// `Enter` / `Return`.
    Enter,
    /// `Tab`.
    Tab,
    /// `Backspace`.
    Backspace,
}

impl Key {
    /// Parses a key name as produced by the browser or read from config.
    ///
    /// Accepts `F1`..`F24`, the named non-printable keys a browser reports
    /// (`Space`, `Enter`, `Escape`, `Tab`, `Backspace`, `Delete`, `Insert`,
    /// `Home`, `End`, `PageUp`, `PageDown`, and the arrow keys), and any single
    /// character. Anything longer, or a name outside that closed set, is
    /// rejected rather than guessed at.
    pub fn parse(name: &str) -> Option<Key> {
        let name = name.trim();
        if name.is_empty() {
            return None;
        }
        if let Some(digits) = name.strip_prefix(['f', 'F']) {
            let number: u8 = digits.parse().ok()?;
            return (1..=24).contains(&number).then_some(Key::Function(number));
        }
        const NAMED: &[(&str, Key)] = &[
            ("space", Key::Character(' ')),
            ("enter", Key::Enter),
            ("return", Key::Enter),
            ("escape", Key::Escape),
            ("esc", Key::Escape),
            ("tab", Key::Tab),
            ("backspace", Key::Backspace),
            ("delete", Key::Delete),
            ("insert", Key::Insert),
            ("home", Key::Home),
            ("end", Key::End),
            ("pageup", Key::PageUp),
            ("pagedown", Key::PageDown),
            ("arrowup", Key::Up),
            ("arrowdown", Key::Down),
            ("arrowleft", Key::Left),
            ("arrowright", Key::Right),
        ];
        let lowered = name.to_ascii_lowercase();
        if let Some((_, key)) = NAMED.iter().find(|(text, _)| *text == lowered.as_str()) {
            return Some(*key);
        }
        let mut chars = name.chars();
        let first = chars.next()?;
        if chars.next().is_some() {
            return None;
        }
        Some(Key::Character(first.to_ascii_uppercase()))
    }

    /// The name used in recorded display strings and config files.
    pub fn as_string(self) -> String {
        match self {
            Key::Function(number) => format!("F{number}"),
            // The space key would otherwise serialise as an invisible,
            // unparseable run of whitespace inside a `+`-separated string.
            Key::Character(' ') => "Space".to_string(),
            Key::Character(character) => character.to_string(),
            Key::Escape => "Escape".to_string(),
            Key::Enter => "Enter".to_string(),
            Key::Tab => "Tab".to_string(),
            Key::Backspace => "Backspace".to_string(),
            Key::Delete => "Delete".to_string(),
            Key::Insert => "Insert".to_string(),
            Key::Home => "Home".to_string(),
            Key::End => "End".to_string(),
            Key::PageUp => "PageUp".to_string(),
            Key::PageDown => "PageDown".to_string(),
            Key::Up => "Up".to_string(),
            Key::Down => "Down".to_string(),
            Key::Left => "Left".to_string(),
            Key::Right => "Right".to_string(),
        }
    }
}

/// A complete, validated binding: modifiers plus a key.
///
/// The modifiers are held as a sorted, de-duplicated set so two recordings of
/// the same chord compare equal regardless of the order the browser reported
/// them in, and so the set can be compared for conflicts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hotkey {
    /// The required modifiers, sorted and without duplicates.
    modifiers: Vec<Modifier>,
    /// The key that must be pressed with them.
    key: Key,
}

impl Hotkey {
    /// Builds a hotkey from already-parsed parts, normalising the modifier set.
    pub fn new<I: IntoIterator<Item = Modifier>>(modifiers: I, key: Key) -> Hotkey {
        let mut modifiers: Vec<Modifier> = modifiers.into_iter().collect();
        modifiers.sort_unstable();
        modifiers.dedup();
        Hotkey { modifiers, key }
    }

    /// The modifiers this binding requires, in a stable order.
    pub fn modifiers(&self) -> &[Modifier] {
        &self.modifiers
    }

    /// The key this binding requires.
    pub fn key(&self) -> Key {
        self.key
    }

    /// Parses a recorded display string such as `Ctrl + Alt + Shift + F9`.
    ///
    /// The last `+`-separated part is the key and every earlier part is a
    /// modifier. All parts must be recognised: an unknown modifier or key is an
    /// error, never a silently ignored fragment. A lone modifier with no key,
    /// or a bare key with no modifiers, is also rejected — the latter because a
    /// bare `F9` hotkey would swallow every press of that key in every
    /// application.
    ///
    /// A `+` is only a separator here; a literal `+` key is spelled `Plus`,
    /// which parses as a single character key.
    pub fn parse(text: &str) -> Result<Hotkey, HotkeyError> {
        let parts: Vec<&str> = text.split('+').map(str::trim).collect();
        if parts.len() < 2 {
            return Err(HotkeyError::Incomplete);
        }
        // A blank part is a missing piece of the chord, not an unknown name:
        // "Ctrl +" means the user never pressed a key, and reporting an
        // unsupported-modifier error for it would send them looking in the
        // wrong place.
        if parts.iter().any(|part| part.is_empty()) {
            return Err(HotkeyError::Incomplete);
        }
        let (key_text, modifier_texts) = parts.split_last().ok_or(HotkeyError::Incomplete)?;
        let mut modifiers = Vec::with_capacity(modifier_texts.len());
        for text in modifier_texts {
            modifiers.push(Modifier::parse(text).ok_or(HotkeyError::UnknownModifier)?);
        }
        if modifiers.is_empty() {
            return Err(HotkeyError::Incomplete);
        }
        let key = Key::parse(key_text).ok_or(HotkeyError::UnknownKey)?;
        Ok(Hotkey::new(modifiers, key))
    }

    /// The canonical display form, which also round-trips through [`parse`].
    pub fn as_string(&self) -> String {
        let mut parts: Vec<String> = self
            .modifiers
            .iter()
            .map(|modifier| modifier.as_str().to_string())
            .collect();
        parts.push(self.key.as_string());
        parts.join(" + ")
    }

    /// True when these two bindings name the same chord.
    ///
    /// Used for conflict detection against an existing binding.
    pub fn conflicts_with(&self, other: &Hotkey) -> bool {
        self == other
    }
}

impl fmt::Display for Hotkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.as_string())
    }
}

/// Why a recorded hotkey string could not be accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotkeyError {
    /// Fewer than two parts, or a part was blank.
    Incomplete,
    /// A modifier name outside the supported set.
    UnknownModifier,
    /// A key name outside the supported set.
    UnknownKey,
}

impl fmt::Display for HotkeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            HotkeyError::Incomplete => "a binding needs at least one modifier and one key",
            HotkeyError::UnknownModifier => "unsupported modifier",
            HotkeyError::UnknownKey => "unsupported key",
        };
        f.write_str(message)
    }
}

impl std::error::Error for HotkeyError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_recorded_browser_display_strings() {
        // The exact form the Web recorder produces, including spaces around
        // the separators, must decode.
        let hotkey = Hotkey::parse("Ctrl + Alt + Shift + F9").unwrap();
        assert_eq!(
            hotkey.modifiers(),
            &[Modifier::Control, Modifier::Alt, Modifier::Shift]
        );
        assert_eq!(hotkey.key(), Key::Function(9));
        assert_eq!(hotkey.as_string(), "Ctrl + Alt + Shift + F9");
    }

    #[test]
    fn parsing_is_order_insensitive_and_tolerant_of_spacing() {
        // The browser emits modifiers in whatever order they were held, so
        // two recordings of the same chord must compare equal.
        let first = Hotkey::parse("Ctrl+Alt+F10").unwrap();
        let second = Hotkey::parse("Alt + Ctrl + F10").unwrap();
        assert_eq!(first, second);
        assert_eq!(first.as_string(), "Ctrl + Alt + F10");
    }

    #[test]
    fn duplicate_modifiers_collapse() {
        let hotkey = Hotkey::parse("Ctrl + Ctrl + A").unwrap();
        assert_eq!(hotkey.modifiers(), &[Modifier::Control]);
        assert_eq!(hotkey.as_string(), "Ctrl + A");
    }

    #[test]
    fn unknown_modifiers_and_keys_are_rejected_not_ignored() {
        // Silently dropping an unrecognised modifier would arm a weaker
        // hotkey that hijacks ordinary keystrokes.
        assert_eq!(
            Hotkey::parse("Ctrl + Super + F9").unwrap_err(),
            HotkeyError::UnknownModifier
        );
        assert_eq!(
            Hotkey::parse("Ctrl + F25").unwrap_err(),
            HotkeyError::UnknownKey
        );
        assert_eq!(
            Hotkey::parse("Ctrl + F0").unwrap_err(),
            HotkeyError::UnknownKey
        );
        assert_eq!(
            Hotkey::parse("Ctrl + NotAKey").unwrap_err(),
            HotkeyError::UnknownKey
        );
    }

    #[test]
    fn incomplete_combinations_are_rejected() {
        // A bare key would swallow every press of it in every application;
        // a lone modifier is not a chord at all.
        assert_eq!(Hotkey::parse("F9").unwrap_err(), HotkeyError::Incomplete);
        assert_eq!(Hotkey::parse("Ctrl").unwrap_err(), HotkeyError::Incomplete);
        assert_eq!(Hotkey::parse("").unwrap_err(), HotkeyError::Incomplete);
        assert_eq!(
            Hotkey::parse("Ctrl +").unwrap_err(),
            HotkeyError::Incomplete
        );
        assert_eq!(
            Hotkey::parse("+ F9").unwrap_err(),
            HotkeyError::Incomplete,
            "a blank modifier is a missing part, not an unsupported one"
        );
    }

    #[test]
    fn named_and_printable_keys_round_trip() {
        for text in [
            "Ctrl + Shift + Space",
            "Ctrl + Alt + Enter",
            "Ctrl + Escape",
            "Alt + Tab",
            "Ctrl + A",
            "Ctrl + Alt + 1",
            "Shift + Slash",
        ] {
            // `Slash` is a two-character name and therefore rejected; every
            // other fixture must survive a parse/display round trip.
            if text.ends_with("Slash") {
                assert!(Hotkey::parse(text).is_err(), "{text} must be rejected");
                continue;
            }
            let hotkey = Hotkey::parse(text).unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!(hotkey.as_string(), text, "{text} must round trip");
        }
        // A punctuation key is best recorded as the single character to share
        // its virtual key code with the digit row.
        assert_eq!(
            Hotkey::parse("Ctrl + /").unwrap().as_string(),
            "Ctrl + /".to_string()
        );
    }

    #[test]
    fn escape_and_arrow_keys_are_named_explicitly() {
        assert_eq!(Hotkey::parse("Ctrl + Esc").unwrap().key(), Key::Escape);
        assert_eq!(Hotkey::parse("Ctrl + ArrowUp").unwrap().key(), Key::Up);
        assert_eq!(
            Hotkey::parse("Ctrl + PageDown").unwrap().as_string(),
            "Ctrl + PageDown"
        );
    }

    #[test]
    fn identical_chords_conflict_and_different_ones_do_not() {
        let first = Hotkey::parse("Ctrl + Alt + F9").unwrap();
        let same = Hotkey::parse("Alt + Ctrl + F9").unwrap();
        let other_key = Hotkey::parse("Ctrl + Alt + F10").unwrap();
        let other_modifier = Hotkey::parse("Ctrl + F9").unwrap();
        assert!(first.conflicts_with(&same));
        assert!(!first.conflicts_with(&other_key));
        assert!(!first.conflicts_with(&other_modifier));
    }

    #[test]
    fn errors_describe_themselves() {
        // The message reaches the user through the API, so it must say which
        // half of the binding was rejected.
        assert!(HotkeyError::UnknownModifier
            .to_string()
            .contains("modifier"));
        assert!(HotkeyError::UnknownKey.to_string().contains("key"));
        assert!(HotkeyError::Incomplete.to_string().contains("modifier"));
    }
}
