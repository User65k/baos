use core::ffi::c_void;
use core::ptr::NonNull;
use std::{ffi::{CStr, CString}, ops::{BitAnd, Shr}};

type c_char = i8;
type Ap = i32;
#[link(name = "kdriveExpress")]
extern "C" {
    /*
    fn kdrive_logger_set_level(level: c_int);
    fn kdrive_logger_console();
    fn kdrive_register_error_callback(func: extern fn(c_int, c_void), ka: c_int);
    fn kdrive_logger(level: c_int, msg: *const u8);
    /// Connect the Packet Trace logging mechanism to see the Rx and Tx packets
    fn kdrive_ap_packet_trace_connect(fd: Ap);
    */
    fn kdrive_ap_close(fd: Ap);
    fn kdrive_ap_release(fd: Ap);
    /// Sends a GroupValue_Write Telegram
    ///
    /// The length is specified in bits to enable values less than one byte to be sent (i.e. 1 bit boolean) etc.
    fn kdrive_ap_group_write(fd: Ap, target: u16, data: *const u8, len: u32) -> u32;
    /// We create a Access Port descriptor.
    /// This descriptor is then used for all calls to that specific access port.
    fn kdrive_ap_create() -> Ap;
    /// Open a connection to a KNX FT1.2 serial interface
    fn kdrive_ap_open_serial_ft12(fd: Ap, path: *const c_char) -> u32;
    fn kdrive_get_error_message(e: u32, msg: *mut c_char, len: u32);
    //kdrive_set_event_callback
    fn kdrive_ap_register_telegram_callback(
        fd: Ap,
        func: TelegramCallback<c_void>,
        user_data: Option<NonNull<c_void>>,
        key: &mut u32,
    );
    fn kdrive_ap_receive(fd: Ap, telegram: *mut u8, telegram_len: u32, timeout_ms: u32) -> u32;
    fn kdrive_ap_get_message_code(data: *const u8, len: u32, code: &mut u8);
    fn kdrive_ap_is_group_write(telegram: *const u8, telegram_len: u32) -> u32;
    fn kdrive_ap_get_dest(telegram: *const u8, telegram_len: u32, address: &mut u16) -> u32;
    fn kdrive_ap_get_group_data(
        telegram: *const u8,
        telegram_len: u32,
        data: *mut u8,
        data_len: &mut u32,
    ) -> u32;
}
pub type TelegramCallback<T> = extern "C" fn(*const u8, u32, Option<NonNull<T>>);
///cEMI message code for L_Data.ind
pub const KDRIVE_CEMI_L_DATA_IND: u8 = 0x29;
pub const KDRIVE_MAX_GROUP_VALUE_LEN: usize = 14;

#[derive(Clone)]
pub struct KDrive(Ap);
impl KDrive {
    pub fn new() -> Result<KDrive, ()> {
        let ap = unsafe { kdrive_ap_create() };
        if ap == -1 {
            Err(())
        } else {
            Ok(KDrive(ap))
        }
    }
    pub fn group_write(&self, addr: u16, data: &[u8]) {
        unsafe {
            kdrive_ap_group_write(self.0, addr, data.as_ptr(), data.len() as u32);
        }
    }
    pub fn register_telegram_callback<T>(
        &self,
        func: TelegramCallback<T>,
        user_data: Option<NonNull<T>>,
    ) -> u32 {
        let mut key = 0;
        unsafe {
            kdrive_ap_register_telegram_callback(
                self.0,
                core::mem::transmute::<TelegramCallback<T>, TelegramCallback<c_void>>(func),
                user_data.map(|nn| nn.cast()),
                &mut key,
            );
        }
        key
    }
    pub fn recv<'a>(&self, data: &'a mut [u8], timeout_ms: u32) -> &'a [u8] {
        let l =
            unsafe { kdrive_ap_receive(self.0, data.as_mut_ptr(), data.len() as u32, timeout_ms) };
        &data[..l as usize]
    }
}
impl Drop for KDrive {
    fn drop(&mut self) {
        unsafe {
            kdrive_ap_release(self.0);
        }
    }
}
#[derive(Clone)]
pub struct KDriveFT12(KDrive);
impl KDriveFT12 {
    pub fn open(ap: KDrive, dev: &CString) -> Result<KDriveFT12, KDriveErr> {
        let op = unsafe { kdrive_ap_open_serial_ft12(ap.0, dev.as_ptr().cast()) };
        if op != 0 {
            Err(KDriveErr(op))
        } else {
            Ok(KDriveFT12(ap))
        }
    }
}
impl Drop for KDriveFT12 {
    fn drop(&mut self) {
        unsafe {
            kdrive_ap_close(self.0 .0);
        }
    }
}
impl core::ops::Deref for KDriveFT12 {
    type Target = KDrive;
    fn deref(&self) -> &KDrive {
        &self.0
    }
}

/// Apparently a cEMI Message (common external message interface)
pub struct KDriveTelegram {
    data: *const u8,
    len: u32,
}
impl KDriveTelegram {
    pub fn new(data: *const u8, len: u32) -> KDriveTelegram {
        KDriveTelegram { data, len }
    }
    pub fn get_msg_code(&self) -> u8 {
        let mut code = 0;
        unsafe {
            kdrive_ap_get_message_code(self.data, self.len, &mut code);
        }
        code
    }
    pub fn is_group_write(&self) -> bool {
        (unsafe { kdrive_ap_is_group_write(self.data, self.len) } != 0)
    }
    pub fn get_dest(&self) -> Result<u16, KDriveErr> {
        let mut addr = 0;
        let op = unsafe { kdrive_ap_get_dest(self.data, self.len, &mut addr) };
        if op == 0 {
            Ok(addr)
        } else {
            Err(KDriveErr(op))
        }
    }
    pub fn get_group_data<'a>(
        &self,
        msg: &'a mut [u8; KDRIVE_MAX_GROUP_VALUE_LEN],
    ) -> Result<&'a [u8], KDriveErr> {
        let mut msg_len = KDRIVE_MAX_GROUP_VALUE_LEN as u32;
        let op = unsafe {
            kdrive_ap_get_group_data(self.data, self.len, msg.as_mut_ptr(), &mut msg_len)
        };
        if op == 0 {
            Ok(&msg[..msg_len as usize])
        } else {
            Err(KDriveErr(op))
        }
    }
}
impl std::fmt::Debug for KDriveTelegram {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let slice: &[u8] = self;
        f.write_fmt(format_args!("{:x?}", slice))
    }
}
impl std::ops::Deref for KDriveTelegram {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        unsafe { std::slice::from_raw_parts(self.data, self.len as usize) }
    }
}

/// https://weinzierl.de/images/download/software_tools/kdriveexpress/22_1_1/docu/c/kdrive__express__error_8h.html
pub struct KDriveErr(u32);
impl std::fmt::Debug for KDriveErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("KDriveErr: ")?;
        f.write_fmt(format_args!("0x{:X} - ", self.0))?;
        std::fmt::Display::fmt(&self, f)
    }
}
impl std::fmt::Display for KDriveErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut msg = Vec::with_capacity(1024);
        unsafe { kdrive_get_error_message(self.0, msg.as_mut_ptr(), msg.capacity() as u32) };
        if let Ok(s) = unsafe { CStr::from_ptr(msg.as_ptr().cast()) }.to_str() {
            f.write_str(s)
        } else {
            Err(std::fmt::Error)
        }
    }
}
impl std::error::Error for KDriveErr {}
