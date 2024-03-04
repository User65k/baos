use core::ptr::NonNull;
use core::ffi::c_void;
use std::ffi::CString;
use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::convert::TryInto;

mod kdrive;
use kdrive::{KDrive, KDriveTelegram, KDriveFT12, KDRIVE_CEMI_L_DATA_IND, KDRIVE_MAX_GROUP_VALUE_LEN};

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
	(c-b'a'+BUS_START_ADDR) as u16
}
const BUS_START_ADDR: u8 = 0xaa;

extern fn on_telegram(data: *const u8, len: u32, _user_data: Option<NonNull<c_void>>)
{
	let data = KDriveTelegram::new(data, len);
	let mut msg = [0;KDRIVE_MAX_GROUP_VALUE_LEN];

	if KDRIVE_CEMI_L_DATA_IND == data.get_msg_code() 
		&& data.is_group_write() {
		if let Ok(addr) = data.get_dest() {
			if let Ok(msg) = data.get_group_data(&mut msg)
			{
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
						let lower = (addr & 0xff) as u8;
						if upper == 0x11_00 {
							println!("Step: 0x{:x} {:?}", lower, msg);
						}else if upper == 0x10_00 {
							println!("Voll: 0x{:x} {:?}", lower, msg);
						}else{
							println!("Group Write: 0x{:x} {:?}", addr, msg);
						}
						if let Some(r) = lower.checked_sub(BUS_START_ADDR) {
							if r <= b'h'-b'a' {
								//keep track of own
							}
						}
					}
				}
				return;
			}
		}
	}
	println!("Data: {:?}", data);
}
