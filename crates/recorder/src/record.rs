//! Windows raw-input capture.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::sync::Mutex;
use std::time::Instant;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::{
    GetRawInputData, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER, RID_INPUT, RIDEV_INPUTSINK, RIM_TYPEKEYBOARD,
    RIM_TYPEMOUSE, RegisterRawInputDevices,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetForegroundWindow, GetMessageW, GetWindowTextW, HWND_MESSAGE,
    MSG, RegisterClassW, WM_INPUT, WNDCLASSW,
};

struct Recorder {
    out: BufWriter<File>,
    start: Instant,
    focused: bool,
    last_flush: Instant,
    events: u64,
}

static RECORDER: Mutex<Option<Recorder>> = Mutex::new(None);

/// Game keys by virtual-key code. Anything else is not recorded.
fn key_name(vkey: u16) -> Option<&'static str> {
    Some(match vkey {
        0x57 => "w",
        0x41 => "a",
        0x53 => "s",
        0x44 => "d",
        0x20 => "space",
        0x10 | 0xA0 | 0xA1 => "shift",
        0x11 | 0xA2 | 0xA3 => "ctrl",
        0x45 => "e",
        0x51 => "q",
        0x46 => "f",
        0x31 => "1",
        0x32 => "2",
        0x33 => "3",
        0x34 => "4",
        0x35 => "5",
        0x36 => "6",
        0x37 => "7",
        0x38 => "8",
        0x39 => "9",
        _ => return None,
    })
}

fn minecraft_in_front() -> bool {
    unsafe {
        let window = GetForegroundWindow();
        let mut title = [0u16; 256];
        let len = GetWindowTextW(window, title.as_mut_ptr(), title.len() as i32);
        String::from_utf16_lossy(&title[..len.max(0) as usize]).starts_with("Minecraft")
    }
}

impl Recorder {
    fn write(&mut self, kind: char, a: &str, b: i32) {
        let t = self.start.elapsed().as_micros();
        let _ = writeln!(self.out, "{t},{kind},{a},{b}");
        self.events += 1;
        if self.last_flush.elapsed().as_secs() >= 2 {
            let _ = self.out.flush();
            self.last_flush = Instant::now();
            eprint!("\r{} events recorded", self.events);
        }
    }

    fn handle(&mut self, input: &RAWINPUT) {
        let focused = minecraft_in_front();
        if focused != self.focused {
            self.focused = focused;
            self.write('f', if focused { "1" } else { "0" }, 0);
        }
        if !focused {
            return;
        }
        unsafe {
            if input.header.dwType == RIM_TYPEMOUSE {
                let mouse = &input.data.mouse;
                if mouse.lLastX != 0 || mouse.lLastY != 0 {
                    self.write('m', &mouse.lLastX.to_string(), mouse.lLastY);
                }
                let flags = mouse.Anonymous.Anonymous.usButtonFlags;
                // RI_MOUSE_*_DOWN / *_UP bit pairs for left, right, middle.
                for (bit, button) in [(0x0001u16, "left"), (0x0004, "right"), (0x0010, "middle")] {
                    if flags & bit != 0 {
                        self.write('b', button, 1);
                    }
                    if flags & (bit << 1) != 0 {
                        self.write('b', button, 0);
                    }
                }
            } else if input.header.dwType == RIM_TYPEKEYBOARD {
                let keyboard = &input.data.keyboard;
                if let Some(name) = key_name(keyboard.VKey) {
                    // RI_KEY_BREAK (1) marks a release.
                    let down = (keyboard.Flags & 1) == 0;
                    self.write('k', name, down as i32);
                }
            }
        }
    }
}

unsafe extern "system" fn window_proc(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if message == WM_INPUT {
        let mut input: RAWINPUT = unsafe { std::mem::zeroed() };
        let mut size = size_of::<RAWINPUT>() as u32;
        let read = unsafe {
            GetRawInputData(
                lparam as HRAWINPUT,
                RID_INPUT,
                &mut input as *mut RAWINPUT as *mut _,
                &mut size,
                size_of::<RAWINPUTHEADER>() as u32,
            )
        };
        if read != u32::MAX {
            if let Some(recorder) = RECORDER.lock().unwrap().as_mut() {
                recorder.handle(&input);
            }
        }
    }
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

pub fn run(path: &str) -> std::io::Result<()> {
    let file = File::create(path)?;
    *RECORDER.lock().unwrap() =
        Some(Recorder { out: BufWriter::new(file), start: Instant::now(), focused: false, last_flush: Instant::now(), events: 0 });

    unsafe {
        let class_name: Vec<u16> = "rapidbot-recorder\0".encode_utf16().collect();
        let instance = GetModuleHandleW(std::ptr::null());
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: class_name.as_ptr(),
            ..std::mem::zeroed()
        };
        if RegisterClassW(&class) == 0 {
            return Err(std::io::Error::last_os_error());
        }
        // A message-only window: invisible, exists just to receive WM_INPUT.
        let window = CreateWindowExW(
            0,
            class_name.as_ptr(),
            class_name.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        );
        if window.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        // Generic desktop page: usage 2 is mouse, 6 is keyboard. INPUTSINK
        // delivers input even though this window is never focused.
        let devices = [
            RAWINPUTDEVICE { usUsagePage: 1, usUsage: 2, dwFlags: RIDEV_INPUTSINK, hwndTarget: window },
            RAWINPUTDEVICE { usUsagePage: 1, usUsage: 6, dwFlags: RIDEV_INPUTSINK, hwndTarget: window },
        ];
        if RegisterRawInputDevices(devices.as_ptr(), devices.len() as u32, size_of::<RAWINPUTDEVICE>() as u32) == 0 {
            return Err(std::io::Error::last_os_error());
        }

        eprintln!("recording to {path}; play Minecraft, then press Ctrl+C here to stop");
        let mut message: MSG = std::mem::zeroed();
        while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {
            DispatchMessageW(&message);
        }
    }
    Ok(())
}
