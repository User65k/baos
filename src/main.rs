use core::ptr::NonNull;
use core::ffi::c_void;
use std::ffi::CString;
use core::slice::from_raw_parts;
use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::convert::TryInto;

type c_char = i8;
type c_int = isize;

type Ap = isize;
#[link(name = "kdriveExpress")]
extern {
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
 fn kdrive_ap_group_write(fd: Ap, target: u16, data: *const u8, len: usize) -> c_int;
 /// We create a Access Port descriptor.
 /// This descriptor is then used for all calls to that specific access port.
 fn kdrive_ap_create() -> Ap;
 /// Open a connection to a KNX FT1.2 serial interface
 fn kdrive_ap_open_serial_ft12(fd: Ap, path: *const c_char) -> c_int;
 //fn kdrive_get_error_message(e: c_int, msg: *mut u8, len: usize);
 //kdrive_set_event_callback
 fn kdrive_ap_register_telegram_callback(fd: Ap, func: TelegramCallback, user_data: Option<NonNull<c_void>>, key: &mut u32);
 fn kdrive_ap_receive(fd: Ap, telegram: *mut u8, telegram_len: u32, timeout_ms: u32) -> u32;
 fn kdrive_ap_get_message_code(data:*const u8, len: u32, code: &mut u8);
 fn kdrive_ap_is_group_write(telegram:*const u8, telegram_len: u32) -> u32;
 fn kdrive_ap_get_dest(telegram:*const u8, telegram_len: u32, address: &mut u16) -> u32;
 fn kdrive_ap_get_group_data(telegram: *const u8, telegram_len: u32, data: *mut u8, data_len: &mut u32) -> u32;
}
type TelegramCallback = extern fn(*const u8, u32, Option<NonNull<c_void>>);
const KDRIVE_CEMI_L_DATA_IND: u8 = 0x29;
const KDRIVE_MAX_GROUP_VALUE_LEN: usize = 14;

struct KDrive(Ap);
impl KDrive {
	fn new() -> Result<KDrive,()> {
		let ap = unsafe{kdrive_ap_create()};
		if ap==-1{
			Err(())
		}else{
			Ok(KDrive(ap))
		}
	}
	fn group_write(&self, addr: u16, data: &[u8]) {
		unsafe{
			kdrive_ap_group_write(self.0, addr, data.as_ptr(), data.len());
		}
	}
	fn register_telegram_callback(&self, func: TelegramCallback, user_data: Option<NonNull<c_void>>) -> u32 {
		let mut key = 0;
		unsafe {
			kdrive_ap_register_telegram_callback(self.0, func, user_data, &mut key);
		}
		key
	}
	fn recv<'a>(&self, data: &'a mut [u8], timeout_ms: u32) -> &'a [u8] {
		let l = unsafe {
			kdrive_ap_receive(self.0, data.as_mut_ptr(), data.len() as u32, timeout_ms)
		};
		&data[..l as usize]
	}
}
impl Drop for KDrive {
	fn drop(&mut self) {
	 unsafe {kdrive_ap_release(self.0);}
	}
}
struct KDriveFT12(KDrive);
impl KDriveFT12 {
	fn open(ap: KDrive, dev: &CString) -> Result<KDriveFT12, KDrive> {
		if unsafe{kdrive_ap_open_serial_ft12(ap.0, dev.as_ptr())} != 0 {
			Err(ap)
		}else{
			Ok(KDriveFT12(ap))
		}
	}
}
impl Drop for KDriveFT12 {
        fn drop(&mut self) {
         unsafe {kdrive_ap_close(self.0.0);}
        }
}
impl core::ops::Deref for KDriveFT12 {
	type Target = KDrive;
	fn deref(&self) -> &KDrive {&self.0}
}

fn main() {
	let listener = TcpListener::bind("0.0.0.0:1337").expect("listen port");

	let serial = CString::new("/dev/ttyAMA0").unwrap();
	let k = KDrive::new().expect("KDrive");
	let k = KDriveFT12::open(k,&serial).map_err(|_e|"").expect("open FT12");

	k.register_telegram_callback(on_telegram, None);

	let mut buf = [0u8; 4];
	for stream in listener.incoming() {
		if let Err(e) = handle_connection(stream, &mut buf, /*&serial,*/ &k) {
			println!("handle_connection failed {}", e);
		}
    }
}
fn handle_connection(stream: std::io::Result<TcpStream>,
  buf: &mut [u8],
  //serial: &CString
  k: &KDrive
  ) -> std::io::Result<()> {
//	let mut stream = stream?;
let mut stream = match stream {
Ok(s) => s,
Err(e) => panic!("accept: {:?}",e),
};
	//stream.read_exact(buf)?;
	let len = stream.read(buf)?;
	let buf = &buf[..len];
	println!("Cmd: {:?}", buf);
	let mut i = buf.split(|&c|c==b' ');

	let target = i.next()
		.ok_or(std::io::ErrorKind::AddrNotAvailable)?;
	
	let (upper_addr, data) = match i.next() {
                Some(b"1") => (0x1000, &[1]),//zu
                Some(b"0") => (0x1000, &[0]),//auf
		Some(b"Z") => (0x1000, &[1]),//zu
		Some(b"A") => (0x1000, &[0]),//auf
		Some(b"S") => (0x1100, &[1]),
		Some(b"D") => (0x1100, &[1]),//runter
		Some(b"U") => (0x1100, &[0]),//rauf
		_ => {return Err(std::io::ErrorKind::InvalidData.into());}
	};
	//let k = KDrive::new().expect("KDrive");
  //let k = KDriveFT12::open(k,serial).map_err(|_e|"").expect("ft12");
	match target {
		b"A" => {
			for addr in to_bus_addr(b'a')..=to_bus_addr(b'h') {
				k.group_write(addr+upper_addr, data);
			}
		},
		b"B" => {
			k.group_write(to_bus_addr(b'g')+upper_addr, data);
			k.group_write(to_bus_addr(b'd')+upper_addr, data);
		},
		b"W" => {
			for addr in &[to_bus_addr(b'h'), to_bus_addr(b'f'), to_bus_addr(b'e'), to_bus_addr(b'c')] {
				k.group_write(*addr+upper_addr, data);
			}
		},
		l => k.group_write(get_addr(l)?+upper_addr, data),
	}
	Ok(())
}
fn get_addr(c: &[u8]) -> std::io::Result<u16> {
	Ok(match c {
		b"W2" => to_bus_addr(b'h'),
		b"BR" => to_bus_addr(b'g'),
		b"W1" => to_bus_addr(b'f'),
		b"W4" => to_bus_addr(b'e'),
		b"BL" => to_bus_addr(b'd'),
		b"W3" => to_bus_addr(b'c'),
		b"S" => to_bus_addr(b'b'),
		b"K" => to_bus_addr(b'a'),
		_ => {return Err(std::io::ErrorKind::InvalidData.into());}
	})
}
const fn to_bus_addr(c: u8) -> u16 {
	(c-b'a'+0xaa) as u16
}

extern fn on_telegram(data: *const u8, len: u32, _user_data: Option<NonNull<c_void>>)
{
	let mut code = 0;
	let mut addr = 0;
	let mut msg_len = KDRIVE_MAX_GROUP_VALUE_LEN as u32;
	let mut msg = [0;KDRIVE_MAX_GROUP_VALUE_LEN];

	unsafe {
		kdrive_ap_get_message_code(data, len, &mut code);

		if KDRIVE_CEMI_L_DATA_IND == code 
			&& kdrive_ap_is_group_write(data, len) != 0
			&& kdrive_ap_get_dest(data, len, &mut addr) == 0
			&& kdrive_ap_get_group_data(data, len, msg.as_mut_ptr(), &mut msg_len) == 0
		{
			let msg = &msg[..msg_len as usize];
			match addr {
				1 => {//zero  1: alles hoch / 0: alles cool
					if msg != [0] {
						println!("Group Write: 1 {:?}", msg);
					}
				},
				2 => {//wind - no longer avail
					println!("Wind: {}", u16::from_be_bytes(msg[..2].try_into().unwrap()));
				},
				addr => {
					let upper = addr & 0xFF00;
					if upper == 0x11_00 {
						println!("Step: 0x{:x} {:?}", addr&0xff, msg);
					}else if upper == 0x10_00 {
						println!("Voll: 0x{:x} {:?}", addr&0xff, msg);
					}else{
						println!("Group Write: 0x{:x} {:?}", addr, msg);
					}
				}
			}
		}else{
			println!("Data: {:?}", from_raw_parts(data, len as usize));
		}
	}
}

