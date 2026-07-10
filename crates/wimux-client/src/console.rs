//! Configuration du mode console d'entrée sous Windows.
//!
//! Pour recevoir l'entrée clavier sous forme de séquences VT brutes (et non de
//! lignes cuisinées avec écho), on active `ENABLE_VIRTUAL_TERMINAL_INPUT` et on
//! désactive les modes ligne/écho/traitement. L'état d'origine est restauré au
//! `Drop` afin de ne pas laisser le terminal de l'utilisateur dans un état
//! bancal.

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Console::{
    CONSOLE_MODE, ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT,
    ENABLE_VIRTUAL_TERMINAL_INPUT, GetConsoleMode, GetStdHandle, STD_INPUT_HANDLE, SetConsoleMode,
};

pub struct RawStdinGuard {
    handle: HANDLE,
    original: CONSOLE_MODE,
    restore: bool,
}

impl RawStdinGuard {
    /// Passe stdin en mode VT brut. Sans effet (mais sans erreur) si stdin n'est
    /// pas une console.
    pub fn set() -> RawStdinGuard {
        unsafe {
            let handle = match GetStdHandle(STD_INPUT_HANDLE) {
                Ok(h) => h,
                Err(_) => return RawStdinGuard::inert(),
            };
            let mut original = CONSOLE_MODE(0);
            if GetConsoleMode(handle, &mut original).is_err() {
                return RawStdinGuard::inert();
            }
            let new = (original
                & !(ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_PROCESSED_INPUT))
                | ENABLE_VIRTUAL_TERMINAL_INPUT;
            if SetConsoleMode(handle, new).is_err() {
                return RawStdinGuard::inert();
            }
            RawStdinGuard {
                handle,
                original,
                restore: true,
            }
        }
    }

    fn inert() -> RawStdinGuard {
        RawStdinGuard {
            handle: HANDLE::default(),
            original: CONSOLE_MODE(0),
            restore: false,
        }
    }
}

impl Drop for RawStdinGuard {
    fn drop(&mut self) {
        if self.restore {
            unsafe {
                let _ = SetConsoleMode(self.handle, self.original);
            }
        }
    }
}
