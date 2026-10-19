//! Host implementation of the action crate's hotkey backend.

#[cfg(windows)]
pub(crate) struct WindowsHotkeyBackend {
    handles: Vec<crate::windows::RegisteredHotkey>,
}

#[cfg(windows)]
impl crate::actions::HotkeyBackend for WindowsHotkeyBackend {
    fn register(&mut self, hotkey: &crate::hotkey::Hotkey, id: i32) -> Result<(), String> {
        let handle = crate::windows::RegisteredHotkey::register(hotkey, id)?;
        self.handles.push(handle);
        Ok(())
    }

    fn clear(&mut self) {
        self.handles.clear();
    }
}

#[cfg(windows)]
pub(crate) fn backend() -> Option<Box<dyn crate::actions::HotkeyBackend>> {
    Some(Box::new(WindowsHotkeyBackend {
        handles: Vec::new(),
    }))
}

#[cfg(not(windows))]
pub(crate) fn backend() -> Option<Box<dyn crate::actions::HotkeyBackend>> {
    None
}
