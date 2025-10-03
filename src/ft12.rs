use core::ptr::NonNull;
use std::{
    ffi::CString, ops::{BitAnd, Shr}, os::fd::{AsRawFd, RawFd}
};

use tokio::{io::{unix::AsyncFd, Interest}, sync::SemaphorePermit};

// FT1.2 Protocol Constants
const FT12_RESET_REQUEST: u8 = 0x10;
const FT12_RESET_INDICATION: u8 = 0x40;
//https://weinzierl.de/images/download/products/770/knx_baos_protocol.pdf
const FT12_CTRL_HOST_ODD: u8 = 0x73;
const FT12_CTRL_HOST_EVEN: u8 = 0x53;
const FT12_CTRL_SRV_ODD: u8 = 0xF3;
const FT12_CTRL_SRV_EVEN: u8 = 0xD3;

const FT12_ACKNOWLEDGE: u8 = 0xE5;
const FT12_FRAME_START: u8 = 0x68;
const FT12_FRAME_END: u8 = 0x16;

// Service codes
///cEMI message code for L_Data.ind
pub const KDRIVE_CEMI_L_DATA_IND: u8 = 0x29;
/// cEMI Message Code for L_Data.req
pub const KDRIVE_CEMI_L_DATA_REQ: u8 = 0x11;
pub const KDRIVE_MAX_GROUP_VALUE_LEN: usize = 14;

// FT1.2 Frame structure
#[derive(Debug, Clone)]
pub struct Ft12Frame {
    pub control: u8,    // Control field
    pub data: Vec<u8>,  // Data payload
}

impl Ft12Frame {
    fn new(control: u8, data: Vec<u8>) -> Self {        
        Self {
            control,
            data,
        }
    }
    
    fn calculate_checksum(control: u8, data: &[u8]) -> u8 {
        let mut sum = control as usize;
        for &byte in data {
            sum += byte as usize;
        }
        (sum & 0xFF) as u8
    }
    
    fn to_bytes(&self) -> Vec<u8> {
        let length = (self.data.len() + 1) as u8; // +1 for control field
        let checksum = Self::calculate_checksum(self.control, &self.data);
        let mut bytes = Vec::with_capacity(self.data.len()+7);
        bytes.push(FT12_FRAME_START);
        bytes.push(length);
        bytes.push(length);
        bytes.push(FT12_FRAME_START);
        bytes.push(self.control);
        bytes.extend_from_slice(&self.data);
        bytes.push(checksum);
        bytes.push(FT12_FRAME_END);
        bytes
    }
    
    fn from_bytes(bytes: &[u8]) -> Result<Self, KDriveErr> {
        if bytes.len() < 8 {
            return Err(KDriveErr::InvalidFrameLength); // Invalid frame length
        }
        
        if bytes[0] != FT12_FRAME_START || bytes[3] != FT12_FRAME_START {
            return Err(KDriveErr::InvalidFrameStart); // Invalid frame start
        }
        
        let length = bytes[1];
        if bytes[2] != length {
            return Err(KDriveErr::LengthMismatch); // Length mismatch
        }
        
        if bytes.len() != (length as usize + 6) {
            return Err(KDriveErr::FrameLengthMismatch); // Frame length mismatch
        }
        
        let control = bytes[4];
        let data_len = length as usize - 1;
        let data = bytes[5..5+data_len].to_vec();
        let checksum = bytes[5+data_len];
        let end = bytes[5+data_len+1];
        
        if end != FT12_FRAME_END {
            return Err(KDriveErr::InvalidFrameEnd); // Invalid frame end
        }
        
        let expected_checksum = Self::calculate_checksum(control, &data);
        if checksum != expected_checksum {
            return Err(KDriveErr::ChecksumMismatch); // Checksum mismatch
        }
        
        Ok(Self {
            control,
            data,
        })
    }
}

struct TTYPort(AsyncFd<RawFd>);
impl TTYPort {
    pub async fn wait_readable(&self) -> std::io::Result<()> {
        self.0.readable().await.map(|_|())
    }
    pub fn try_read(&self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.try_io(Interest::READABLE, |fd|Self::blocking_read(*fd, buf))
    }
    pub async fn read(&self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.async_io(Interest::READABLE, |fd|Self::blocking_read(*fd, buf)).await
    }
    fn blocking_read(fd: i32, buf: &mut [u8]) -> std::io::Result<usize> {
        let len = unsafe {
            libc::read(fd, buf.as_mut_ptr().cast(), buf.len())
        };

        if len >= 0 {
            println!("read({:x?})", &buf[..len as usize]);
            Ok(len as usize)
        }
        else {
            Err(std::io::Error::last_os_error())
        }
    }
    pub async fn read_exact(&self, buf: &mut [u8]) -> std::io::Result<()> {
        let mut buf = buf;
        loop {
            let r = self.read(buf).await?;
            match r {
                s if s == buf.len() => return Ok(()),
                0 => return Err(std::io::ErrorKind::UnexpectedEof.into()),
                p => buf = &mut buf[p..]
            }
        }
    }
    pub async fn write(&self, buf: &[u8]) -> std::io::Result<usize> {
        let a = self.0.writable().await?;
        let len = unsafe {
            libc::write(a.get_inner().as_raw_fd(), buf.as_ptr().cast(), buf.len())
        };

        if len >= 0 {
            println!("write({:x?})", &buf[..len as usize]);
            Ok(len as usize)
        }
        else {
            Err(std::io::Error::last_os_error())
        }
    }
    pub async fn write_all(&self, buf: &[u8]) -> std::io::Result<()> {
        let mut buf = buf;
        loop {
            let w = self.write(buf).await?;
            match w {
                s if s == buf.len() => return Ok(()),
                0 => return Err(std::io::ErrorKind::UnexpectedEof.into()),
                p => buf = &buf[p..]
            }
        }
    }
}
struct FT12Dev {
    ///the bus is in use (waiting for more data to read)
    s: tokio::sync::Semaphore,
    d: TTYPort,
    /// protected by semaphore
    recv_odd: std::cell::UnsafeCell<bool>,
    /// protected by semaphore
    send_odd: std::cell::UnsafeCell<bool>,
}
impl FT12Dev {
    pub fn new(dev: &CString) -> std::io::Result<Self> {
        let fd = unsafe { libc::open(dev.as_ptr(), libc::O_RDWR | libc::O_NOCTTY | libc::O_NONBLOCK | libc::O_LARGEFILE, 0) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if unsafe { libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut termios = termios::Termios::from_fd(fd)?;
        let o = termios::cfgetospeed(&termios);
        let i = termios::cfgetispeed(&termios);
        termios::tcflush(fd, termios::TCIFLUSH)?;
        if o!=i || o != termios::B19200 {
            println!("baud rate aint cool: {} {}", o, i);
            termios::cfsetspeed(&mut termios, termios::B19200)?;
        }
        //set magic flags from strace
        termios.c_iflag=0x14;
        termios.c_oflag=0x4;
        termios.c_cflag=0xdbe;
        termios.c_lflag=0xa20;
        termios.c_cc = [3, 28, 127, 21, 4, 0, 1, 0, 17, 19, 26, 0, 18, 15, 23, 22, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        //termios.c_cc[termios::VMIN]=1;
        //termios.c_cc[termios::VTIME]=0;
        termios::tcsetattr(fd, termios::TCSANOW, &termios)?;
        let port = TTYPort(AsyncFd::new(fd)?);
        Ok(FT12Dev{d: port, s: tokio::sync::Semaphore::new(1), recv_odd: true.into(), send_odd: true.into()})
    }
    pub async fn reset(&mut self) -> std::io::Result<()> {
        // Send reset request: 10 40 40 16
        let reset_frame = [FT12_RESET_REQUEST, FT12_RESET_INDICATION, FT12_RESET_INDICATION, FT12_FRAME_END];
        let sp =self.s.acquire().await.unwrap();
        self.send_and_ack(&reset_frame, sp).await
    }
    pub async fn write(&self, data: Vec<u8>) -> std::io::Result<()> {
        let sp =self.s.acquire().await.unwrap();
        let control = if unsafe { self.send_odd.get().read() } {
            unsafe { self.send_odd.get().write(false) };
            FT12_CTRL_HOST_ODD
        }else{
            unsafe { self.send_odd.get().write(true) };
            FT12_CTRL_HOST_EVEN
        };
        let frame = Ft12Frame::new(control, data);
        self.send_and_ack(&frame.to_bytes(), sp).await
    }
    ///exclusively write and wait for an ack
    async fn send_and_ack(&self, data: &[u8], sp: SemaphorePermit<'_>) -> std::io::Result<()> {     
        self.d.write_all(data).await?;

        let mut ack_buf = [0u8; 1];
        self.d.read_exact(&mut ack_buf).await?;
        drop(sp);
        if ack_buf[0] != FT12_ACKNOWLEDGE {
            return Err(std::io::ErrorKind::InvalidData.into()); // Wrong acknowledge
        }
        Ok(())
    }
    /*pub async fn blocking_read(&self, buf: &mut [u8]) -> std::io::Result<Ft12Frame> {
        let sp =self.s.acquire().await.unwrap();

        println!("read lock");
        let read = self.d.read(buf).await?;
        self.internal_read(read, buf, sp).await
    }*/
    /// wait for data to read
    /// only exclusively read (and ack) once the initial read was successful
    pub async fn try_read(&self, buf: &mut [u8]) -> std::io::Result<Ft12Frame> {
        let (read, sp) = loop {
            self.d.wait_readable().await?;
            // there seems to be data - lock the bus and see if its true...
            let sp =self.s.acquire().await.unwrap();

            println!("read lock");
            match self.d.try_read(buf) {
                //no data -> release and wait again
                Err(e) if e.kind()==std::io::ErrorKind::WouldBlock => {drop(sp);continue},
                Err(e) => return Err(e),
                //data! - read the rest, only release after the ack
                Ok(n) => break (n, sp),
            }
        };
        let f = self.internal_read(read, buf, sp).await?;

        let expected_ctrl = if unsafe { self.recv_odd.get().read() } {
            unsafe { self.recv_odd.get().write(false) };
            FT12_CTRL_SRV_ODD
        }else{
            unsafe { self.recv_odd.get().write(true) };
            FT12_CTRL_SRV_EVEN
        };
        if f.control != expected_ctrl {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "odd/even desync"));
        }
        Ok(f)
    }
    /// read a frame from the bus and ack it. needs a locked bus
    async fn internal_read(&self, mut read: usize, buf: &mut [u8], sp: SemaphorePermit<'_>) -> std::io::Result<Ft12Frame> {
        if read == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "EOF reached"));
        }
        // we should not pick up stray acks here, so it needs to be a frame start
        if buf[0] != FT12_FRAME_START {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "Not a frame start"));
        }
        if read == 1 {
            //only frame start read, need to read length and rest
            self.d.read_exact(&mut buf[1..4]).await?;
            read = 4;
        }
        let length = buf[1] as usize +6;    // length + 4 header bytes + checksum + end
        self.d.read_exact(&mut buf[read..length]).await?;
        //send ack
        self.d.write_all(&[FT12_ACKNOWLEDGE]).await?;
        drop(sp);
        Ft12Frame::from_bytes(&buf[..length]).map_err(|e|std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }
}

pub struct KDriveFT12(FT12Dev);

impl KDriveFT12 {
    pub async fn open(dev: &CString) -> std::io::Result<KDriveFT12> {        
        let mut d = FT12Dev::new(dev)?;
        // Send reset request to initialize the connection
        d.reset().await?;
        let mut ft12 = KDriveFT12(d);
        
        // Complete device initialization sequence
        ft12.initialize_device().await?;
                
        Ok(ft12)
    }
    
    /// Initialize the KNX device with the complete startup sequence.
    /// 
    /// This method replicates the initialization sequence observed in the tty_trace:
    /// 1. Initial configuration request
    /// 2. Multiple property read requests for device configuration
    /// 
    /// The sequence is based on lines 12-46 from the trace file and establishes
    /// proper communication with the KNX interface before normal operation begins.
    async fn initialize_device(&mut self) -> std::io::Result<()> {
        println!("Starting KNX device initialization...");            
        // Step 1: Initial configuration request (line 12 in trace)
        // Request: \x68\x02\x02\x68\x73\xa7\x1a\x16
        // Expected response: \x68\x0c\x0c\x68\xf3\xa8\xff\xff\x00\xc5\x01\x03\xa2\xe2\x00\x04\xea\x16
        let init_frame = vec![0xa7];
        self.0.write(init_frame).await?;
        self.expect_specific_response(&[0xa8, 0xff, 0xff, 0x00, 0xc5, 0x01, 0x03, 0xa2, 0xe2, 0x00, 0x04], "initial config").await?;
        
        // Step 2: Property read 1 (line 18)
        // Request: \x68\x08\x08\x68\x53\xfc\x00\x08\x01\x40\x10\x01\xa9\x16
        // Expected response: \x68\x0a\x0a\x68\xd3\xfb\x00\x08\x01\x40\x10\x01\x00\x0b\x33\x16
        let prop1_frame = vec![0xfc, 0x00, 0x08, 0x01, 0x40, 0x10, 0x01];
        self.0.write(prop1_frame).await?;
        self.expect_specific_response(&[0xfb, 0x00, 0x08, 0x01, 0x40, 0x10, 0x01, 0x00, 0x0b], "property read 1").await?;
        
        // Step 3: Property read 2 (line 23)
        // Request: \x68\x09\x09\x68\x73\xf6\x00\x08\x01\x34\x10\x01\x00\xb7\x16
        // Expected response: \x68\x08\x08\x68\xf3\xf5\x00\x08\x01\x34\x10\x01\x36\x16
        let prop2_frame = vec![0xf6, 0x00, 0x08, 0x01, 0x34, 0x10, 0x01, 0x00];
        self.0.write(prop2_frame).await?;
        self.expect_specific_response(&[0xf5, 0x00, 0x08, 0x01, 0x34, 0x10, 0x01], "property read 2").await?;
        
        // Step 4: Property read 3 (line 28)
        // Request: \x68\x08\x08\x68\x53\xfc\x00\x08\x01\x34\x10\x01\x9d\x16
        // Expected response: \x68\x09\x09\x68\xd3\xfb\x00\x08\x01\x34\x10\x01\x00\x1c\x16
        let prop3_frame = vec![0xfc, 0x00, 0x08, 0x01, 0x34, 0x10, 0x01];
        self.0.write(prop3_frame).await?;
        self.expect_specific_response(&[0xfb, 0x00, 0x08, 0x01, 0x34, 0x10, 0x01, 0x00], "property read 3").await?;
        
        // Step 5: Property read 4 (line 33)
        // Request: \x68\x08\x08\x68\x73\xfc\x00\x08\x01\x33\x10\x01\xbc\x16
        // Expected response: \x68\x0a\x0a\x68\xf3\xfb\x00\x08\x01\x33\x10\x01\x00\x02\x3d\x16
        let prop4_frame = vec![0xfc, 0x00, 0x08, 0x01, 0x33, 0x10, 0x01];
        self.0.write(prop4_frame).await?;
        self.expect_specific_response(&[0xfb, 0x00, 0x08, 0x01, 0x33, 0x10, 0x01, 0x00, 0x02], "property read 4").await?;
        
        // Step 6: Property read 5 (line 38)
        // Request: \x68\x08\x08\x68\x53\xfc\x00\x00\x01\x38\x10\x01\x99\x16
        // Expected response: \x68\x0a\x0a\x68\xd3\xfb\x00\x00\x01\x38\x10\x01\x00\x37\x4f\x16
        let prop5_frame = vec![0xfc, 0x00, 0x00, 0x01, 0x38, 0x10, 0x01];
        self.0.write(prop5_frame).await?;
        self.expect_specific_response(&[0xfb, 0x00, 0x00, 0x01, 0x38, 0x10, 0x01, 0x00, 0x37], "property read 5").await?;
        
        println!("KNX device initialization completed successfully");
        Ok(())
    }
    
    async fn expect_specific_response(
        &mut self,
        expected_data: &[u8], 
        step_name: &str
    ) -> std::io::Result<()> {
        let mut temp_buf = [0u8; 512];
        
        let frame = self.0.try_read(&mut temp_buf).await?;
        // Validate response data matches expected
        if frame.data == expected_data {
            Ok(())
        } else {
            println!("Step '{}': Unexpected response data", step_name);
            println!("Expected: {:02x?}", &expected_data);
            println!("Received: {:02x?}", &frame.data[..expected_data.len().min(frame.data.len())]);
            Err(std::io::ErrorKind::InvalidData.into())// Unexpected response data
        }
    }

    
    pub async fn group_write(&self, addr: u16, data: &[u8]) -> std::io::Result<()> {
        // Create cEMI frame for group write
        let cemi_frame = self.create_group_write_cemi(addr, data);
                    
        // Send frame
        self.0.write(cemi_frame).await
    }
    
    fn create_group_write_cemi(&self, addr: u16, data: &[u8]) -> Vec<u8> {
        let mut cemi = Vec::with_capacity(data.len()+10);
        
        // cEMI Message Code L_Data.req
        cemi.push(KDRIVE_CEMI_L_DATA_REQ);
        
        // Additional Info Length (0 = no additional info)
        cemi.push(0x00);
        
        // Control Field 1 (Standard frame, not repeated, broadcast, priority normal, no ack req)
        cemi.push(0xBC);
        
        // Control Field 2 (Hop count 6, Extended frame format)
        cemi.push(0xE0);
        
        // Source address (0x0000 = device address)
        cemi.push(0x00);
        cemi.push(0x00);
        
        // Destination address (group address)
        cemi.push((addr >> 8) as u8);
        cemi.push((addr & 0xFF) as u8);
        
        // TPCI (Transport layer) = 0x00, APCI (Application layer) = Group Value Write (0x80)
        // For 1-byte data, encode the data value in the lower 6 bits of APCI
        if data.len() == 1 {
            // Data length (NPDU + data)
            cemi.push(1);
            
            let apci = 0x0080 | (data[0] as u16 & 0x003F);
            cemi.push((apci >> 8) as u8);
            cemi.push((apci & 0xFF) as u8);
        } else {
            todo!("I think this len should be in bits");
            // Data length (NPDU + data)
            let data_len = 1 + data.len(); // +1 for TPCI/APCI
            cemi.push(data_len as u8);

            // For multi-byte data, use standard APCI
            cemi.push(0x00); // TPCI
            cemi.push(0x80); // APCI Group Value Write
        }
        
        // Additional data (if any)
        if data.len() > 1 {
            cemi.extend_from_slice(&data[1..]);
        }
        
        cemi
    }
    ///returns a cEMI Message
    pub async fn read_frame(&self, buf: &mut [u8]) -> std::io::Result<Vec<u8>> {
        Ok(self.0.try_read(buf).await?.data)
    }
}

/// cEMI Message (common external message interface)
pub struct cEMIMsg<'a> {
    data: &'a [u8],
}
/*
Structure of a KNX CEMI Telegram

Message Code (1 byte): 0x11 for L_Data.req
Additional Info Length (1 byte): 0x00 if no additional info
Control Field 1 (1 byte): e.g., 0xBC (standard frame, not repeated, broadcast, priority normal, no ack req)
Control Field 2 (1 byte): e.g., 0xE0 (hop count 6, Extended frame format)
Source Address (2 bytes): e.g., 0x0000 (device address)
Destination Address (2 bytes): e.g., 0x0C00 (group address)
Data Length (1 byte): e.g., 0x02 (NPDU + data)
TPCI/APCI (2 bytes): e.g., 0x0080 (TPCI = 0x00, APCI = Group Value Write)
Data (N bytes)

Example FT1.2 Frame with cEMI Telegram:
\x68\x0c\x0c\x68\x73\x11\x00\xbc\xe0\x00\x00\x10\x2f\x01\x00\x80\xe0\x16

0x68 - Start of frame
0x0c - Length of frame (12 bytes)
0x0c - Length of frame (repeated)
0x68 - Start of frame (repeated)
0x73 - Control field (Data request)
0x11 - cEMI Message Code (L_Data.req)
0x00 - Additional Info Length
0xbc - Control Field 1
0xe0 - Control Field 2
0x00 0x00 - Source Address
0x10 0x2f - Destination Address (group address)
0x01 - Data Length (1 byte NPDU + data)
0x00 0x80 - TPCI/APCI (Group Value Write with data 0x00)
0xe0 - FT1.2 Checksum
0x16 - End of frame

*/

impl<'a> cEMIMsg<'a> {
    pub fn new(data: *const u8, len: u32) -> cEMIMsg<'a> {
        assert!(len >= 10);
        cEMIMsg {
            data: unsafe { std::slice::from_raw_parts(data, len as usize) },
        }
    }
    
    pub fn from_bytes(data: &'a [u8]) -> cEMIMsg<'a> {
        cEMIMsg { data }
    }
    
    pub fn get_msg_code(&self) -> u8 {
        self.data[0]
    }
    
    pub fn is_group_write(&self) -> bool {
        if self.data.len() < 11 {
            return false;
        }
        
        // Check APCI (Application Protocol Control Information) in bytes 9-10
        let apci = u16::from_be_bytes([self.data[9], self.data[10]])
            .shr(6u32)
            .bitand(0x0F);
        
        apci == 2 // Group Value Write
    }
    
    pub fn get_dest(&self) -> Result<u16, KDriveErr> {
        if self.data.len() < 8 {
            return Err(KDriveErr::CemiFrameTooShortForDestination);
        }
        Ok(u16::from_be_bytes([self.data[6], self.data[7]]))
    }
    
    pub fn get_group_data<'b>(
        &self,
        msg: &'b mut [u8; KDRIVE_MAX_GROUP_VALUE_LEN],
    ) -> Result<&'b [u8], KDriveErr> {
        if self.data.len() < 9 {
            return Err(KDriveErr::CemiFrameTooShortForDataLength);
        }
        
        let len = self.data[8] as usize;
        if len == 0 {
            return Ok(&msg[..0]);
        }
        
        let payload_start = 10;
        if self.data.len() < payload_start + len {
            return Err(KDriveErr::CemiFrameTruncated);
        }
        
        let payload = &self.data[payload_start..payload_start + len];
        
        // Copy payload to buffer
        if payload.len() > KDRIVE_MAX_GROUP_VALUE_LEN {
            return Err(KDriveErr::CemiDataTooLarge);
        }
        
        msg[..payload.len()].copy_from_slice(payload);
        
        // Mask the first byte to get only the data part (remove APCI bits)
        if !payload.is_empty() {
            msg[0] &= 0x3F;
        }
        
        Ok(&msg[..len.saturating_sub(1).max(1)]) // Subtract 1 for APCI, but at least 1 byte
    }
}
impl std::ops::Deref for cEMIMsg<'_> {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.data
    }
}

// KDriveTelegram is replaced by cEMIMsg - no longer needed

/// Pure Rust KDrive Error implementation
#[derive(Debug, Clone)]
pub enum KDriveErr {
    InvalidFrameLength,
    InvalidFrameStart,
    LengthMismatch,
    FrameLengthMismatch,
    InvalidFrameEnd,
    ChecksumMismatch,
    WriteError,
    WrongAcknowledge,
    ReadError,
    NotConnected,
    FailedToOpenSerialPort,
    ResetRequestFailed,
    ResetAcknowledgeFailed,
    WrongResetAcknowledge,
    CemiFrameTooShortForDestination,
    CemiFrameTooShortForDataLength,
    CemiFrameTruncated,
    CemiDataTooLarge,
    FailedToSendResponseAck,
    TimeoutWaitingForInitializationResponse,
    FailedToAcknowledgeDeviceReadyIndication,
    TimeoutWaitingForDeviceReadyIndication,
    FailedToSendResponseAck2,
    UnexpectedResponseDataDuringInitialization,
    ReadErrorDuringResponseValidation,
    TimeoutWaitingForInitializationResponse2,
}

impl std::fmt::Display for KDriveErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use KDriveErr::*;
        let msg = match self {
            InvalidFrameLength => "Invalid frame length",
            InvalidFrameStart => "Invalid frame start",
            LengthMismatch => "Length mismatch",
            FrameLengthMismatch => "Frame length mismatch",
            InvalidFrameEnd => "Invalid frame end",
            ChecksumMismatch => "Checksum mismatch",
            WriteError => "Write error",
            WrongAcknowledge => "Wrong acknowledge",
            ReadError => "Read error",
            NotConnected => "Not connected",
            FailedToOpenSerialPort => "Failed to open serial port",
            ResetRequestFailed => "Reset request failed",
            ResetAcknowledgeFailed => "Reset acknowledge failed",
            WrongResetAcknowledge => "Wrong reset acknowledge",
            CemiFrameTooShortForDestination => "cEMI frame too short for destination",
            CemiFrameTooShortForDataLength => "cEMI frame too short for data length",
            CemiFrameTruncated => "cEMI frame truncated",
            CemiDataTooLarge => "cEMI data too large",
            FailedToSendResponseAck => "Failed to send response acknowledgment",
            TimeoutWaitingForInitializationResponse => "Timeout waiting for initialization response",
            FailedToAcknowledgeDeviceReadyIndication => "Failed to acknowledge device ready indication",
            TimeoutWaitingForDeviceReadyIndication => "Timeout waiting for device ready indication",
            FailedToSendResponseAck2 => "Failed to send response acknowledgment",
            UnexpectedResponseDataDuringInitialization => "Unexpected response data during initialization",
            ReadErrorDuringResponseValidation => "Read error during response validation",
            TimeoutWaitingForInitializationResponse2 => "Timeout waiting for initialization response",
        };
        write!(f, "KDriveErr: {}", msg)
    }
}
impl std::error::Error for KDriveErr {}

impl From<KDriveErr> for std::io::Error {
    fn from(error: KDriveErr) -> Self {
        std::io::Error::other(error)
    }
}
