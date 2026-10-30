#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

/// Показывает ошибку запуска в окне (если интерфейс не смог стартовать).
#[cfg(windows)]
fn startup_error(text: &str) {
    #[link(name = "user32")]
    extern "system" {
        fn MessageBoxW(hwnd: *mut core::ffi::c_void, text: *const u16, caption: *const u16, utype: u32) -> i32;
    }

    fn wide(s: &str) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        std::ffi::OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
    }

    let body = wide(text);
    let title = wide("Шторка");
    unsafe {
        MessageBoxW(std::ptr::null_mut(), body.as_ptr(), title.as_ptr(), 0x10);
    }
}

#[cfg(not(windows))]
fn startup_error(text: &str) {
    eprintln!("{text}");
}

fn main() {
    if let Err(e) = tauri::Builder::default().run(tauri::generate_context!()) {
        startup_error(&format!(
            "Не удалось запустить интерфейс Шторки.\n\n{e}\n\n\
             Чаще всего это значит, что на компьютере нет компонента \
             Microsoft Edge WebView2 Runtime. Его можно бесплатно установить:\n\
             https://go.microsoft.com/fwlink/p/?LinkId=2124703\n\n\
             После установки запустите Shtorka.exe снова."
        ));
        std::process::exit(1);
    }
}
