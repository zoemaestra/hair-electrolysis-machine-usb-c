use core::sync::atomic::AtomicBool;

// Monitor to see if the USB is connected at any point
// If it is, any treatment should be stopped
// Don't fuck around and try to bypass this please! This is not a wired device!
pub static USB_CONNECTED: AtomicBool = AtomicBool::new(true);
