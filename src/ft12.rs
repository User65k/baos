use core::ptr::NonNull;
use std::{
    ffi::CString, ops::{BitAnd, Shr}, os::fd::RawFd, sync::{mpsc::Sender, Arc, Mutex}, thread::{self, JoinHandle}, time::Duration
};

// FT1.2 Protocol Constants
const FT12_RESET_REQUEST: u8 = 0x10;
const FT12_RESET_INDICATION: u8 = 0x40;
const FT12_DATA_REQUEST: u8 = 0x73;
const FT12_DATA_CONFIRM: u8 = 0xF3;
const FT12_DATA_INDICATION: u8 = 0xD3;
const FT12_ACKNOWLEDGE: u8 = 0xE5;
const FT12_FRAME_START: u8 = 0x68;
const FT12_FRAME_END: u8 = 0x16;

// Service codes
const GROUP_VALUE_WRITE: u16 = 0x0080;
pub type TelegramCallback<T> = fn(*const u8, u32, Option<NonNull<T>>);
///cEMI message code for L_Data.ind
pub const KDRIVE_CEMI_L_DATA_IND: u8 = 0x29;
/// cEMI Message Code for L_Data.req
pub const KDRIVE_CEMI_L_DATA_REQ: u8 = 0x11;
pub const KDRIVE_MAX_GROUP_VALUE_LEN: usize = 14;

// FT1.2 Frame structure
#[derive(Debug, Clone)]
struct Ft12Frame {
    control: u8,    // Control field
    data: Vec<u8>,  // Data payload
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

pub struct KDrive {
    port: Option<Arc<Mutex<TTYPort>>>,
    callbacks: Arc<Mutex<Vec<CallbackEntry>>>,
    stop_sender: Option<Sender<()>>,
    _receiver_thread: Option<JoinHandle<()>>,
}

struct CallbackEntry {
    callback: fn(*const u8, u32, Option<NonNull<()>>),
    user_data: Option<NonNull<()>>,
    key: u32,
}

// SAFETY: CallbackEntry is thread-safe as long as the callback function and user_data
// are valid across thread boundaries. The user is responsible for ensuring this.
unsafe impl Send for CallbackEntry {}
unsafe impl Sync for CallbackEntry {}

impl KDrive {
    pub fn new() -> Result<KDrive, ()> {
        Ok(KDrive {
            port: None,
            callbacks: Arc::new(Mutex::new(Vec::new())),
            stop_sender: None,
            _receiver_thread: None,
        })
    }
    
    pub fn group_write(&mut self, addr: u16, data: &[u8]) -> Result<(), KDriveErr> {
        if self.port.is_some() {
            // Create cEMI frame for group write
            let cemi_frame = self.create_group_write_cemi(addr, data)?;
            
            // Wrap in FT1.2 frame
            let ft12_frame = Ft12Frame::new(FT12_DATA_REQUEST, cemi_frame);
            
            // Send frame
            self.send_frame(&ft12_frame)
        } else {
            Err(KDriveErr::NotConnected) // Not connected
        }
    }
    
    fn create_group_write_cemi(&self, addr: u16, data: &[u8]) -> Result<Vec<u8>, KDriveErr> {
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
        
        Ok(cemi)
    }
    
    fn send_frame(&mut self, frame: &Ft12Frame) -> Result<(), KDriveErr> {
        if let Some(ref port_arc) = self.port {
            let mut port = port_arc.lock().unwrap();
            let bytes = frame.to_bytes();
            match port.write_all(&bytes) {
                Ok(()) => {
                    // Wait for acknowledge
                    let mut ack_buf = [0u8; 1];
                    port.wait_read_fd(Some(Duration::from_millis(100))).map_err(|_|KDriveErr::TimeoutWaitingForDeviceReadyIndication)?;
                    match port.read_exact(&mut ack_buf) {
                        Ok(()) if ack_buf[0] == FT12_ACKNOWLEDGE => Ok(()),
                        Ok(()) => Err(KDriveErr::WrongAcknowledge), // Wrong acknowledge
                        Err(_e) => Err(KDriveErr::ReadError), // Read error
                    }
                }
                Err(_e) => Err(KDriveErr::WriteError), // Write error
            }
        } else {
            Err(KDriveErr::NotConnected) // Not connected
        }
    }
    
    pub fn register_telegram_callback<T>(
        &mut self,
        func: TelegramCallback<T>,
        user_data: Option<NonNull<T>>,
    ) -> Result<u32, KDriveErr> {
        let key = self.callbacks.lock().unwrap().len() as u32;
        
        // Type erase the callback
        let type_erased_callback: fn(*const u8, u32, Option<NonNull<()>>) = unsafe {
            std::mem::transmute(func)
        };
        
        let type_erased_user_data = user_data.map(|ptr| ptr.cast::<()>());
        
        let entry = CallbackEntry {
            callback: type_erased_callback,
            user_data: type_erased_user_data,
            key,
        };
        
        self.callbacks.lock().unwrap().push(entry);
        Ok(key)
    }
    
    pub fn recv<'a>(&self, data: &'a mut [u8], _timeout_ms: u32) -> &'a [u8] {
        // For now, return empty slice - this would be implemented with proper timeout handling
        &data[..0]
    }
    
    /// Check if the receiver thread is currently running
    pub fn is_receiver_active(&self) -> bool {
        self._receiver_thread.as_ref().map_or(false, |handle| !handle.is_finished())
    }
}

struct TTYPort(RawFd);
impl TTYPort {
    pub fn wait_read_fd(&self, timeout: Option<Duration>) -> std::io::Result<()> {
        self.wait_fd(libc::POLLIN, timeout)
    }

    fn wait_write_fd(&self, timeout: Duration) -> std::io::Result<()> {
        self.wait_fd(libc::POLLOUT, Some(timeout))
    }

    fn wait_fd(&self, events: i16, timeout: Option<Duration>) -> std::io::Result<()> {
        let mut fds = vec!(libc::pollfd { fd: self.0, events, revents: 0 });

        let wait = Self::do_poll(&mut fds, timeout);

        if wait < 0 {
            return Err(std::io::Error::last_os_error());
        }

        if wait == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "Operation timed out"));
        }

        if fds[0].revents & events != 0 {
            return Ok(());
        }

        if fds[0].revents & (libc::POLLHUP | libc::POLLNVAL) != 0 {
            return Err(std::io::ErrorKind::BrokenPipe.into());
        }

        Err(std::io::Error::other(""))
    }
    #[inline]
    fn do_poll(fds: &mut [libc::pollfd], timeout: Option<Duration>) -> i32 {
        use std::ptr;

        let timeout_ts;
        let timeout = if let Some(t) = timeout {
            timeout_ts = libc::timespec {
                tv_sec: t.as_secs() as libc::time_t,
                tv_nsec: t.subsec_nanos() as libc::c_long,
            };
            &timeout_ts
        }else{
            ptr::null()
        };

        unsafe {
            libc::ppoll((fds[..]).as_mut_ptr(),
                fds.len() as u32,
                timeout,
                ptr::null())
        }
    }
    pub fn read(&self, buf: &mut [u8]) -> std::io::Result<usize> {
        let len = unsafe {
            libc::read(self.0, buf.as_mut_ptr().cast(), buf.len())
        };

        if len >= 0 {
            Ok(len as usize)
        }
        else {
            Err(std::io::Error::last_os_error())
        }
    }
    pub fn read_exact(&self, buf: &mut [u8]) -> std::io::Result<()> {
        let mut buf = buf;
        loop {
            self.wait_read_fd(None)?;
            let r = self.read(buf)?;
            match r {
                s if s == buf.len() => return Ok(()),
                0 => return Err(std::io::ErrorKind::UnexpectedEof.into()),
                p => buf = &mut buf[p..]
            }
        }
    }
    pub fn write(&self, buf: &[u8]) -> std::io::Result<usize> {
        self.wait_write_fd(Duration::from_millis(10))?;
        let len = unsafe {
            libc::write(self.0, buf.as_ptr().cast(), buf.len())
        };

        if len >= 0 {
            Ok(len as usize)
        }
        else {
            Err(std::io::Error::last_os_error())
        }
    }
    pub fn write_all(&self, buf: &[u8]) -> std::io::Result<()> {
        let mut buf = buf;
        loop {
            let w = self.write(buf)?;
            match w {
                s if s == buf.len() => return Ok(()),
                0 => return Err(std::io::ErrorKind::UnexpectedEof.into()),
                p => buf = &buf[p..]
            }
        }
    }
}

pub struct KDriveFT12(KDrive);

impl KDriveFT12 {
    pub fn open(mut ap: KDrive, dev: &CString) -> Result<KDriveFT12, KDriveErr> {
        let fd = unsafe { libc::open(dev.as_ptr(), libc::O_RDWR | libc::O_NOCTTY | libc::O_NONBLOCK | libc::O_LARGEFILE, 0) };
        if fd < 0 {
            //std::io::Error::last_os_error()
            return Err(KDriveErr::FailedToOpenSerialPort);
        }
        if unsafe { libc::fcntl(fd, libc::F_SETFL, 0) } < 0 {
            return Err(KDriveErr::FailedToOpenSerialPort);
        }
        let mut termios = match termios::Termios::from_fd(fd) {
            Ok(t) => t,
            Err(e) => return Err(KDriveErr::FailedToOpenSerialPort),
        };
        let o = termios::cfgetospeed(&termios);
        let i = termios::cfgetispeed(&termios);
        if let Err(err) = termios::tcflush(fd, termios::TCIFLUSH) {
            return Err(KDriveErr::FailedToOpenSerialPort);
        }
        if o!=i || o != termios::B19200 {
            println!("baud rate aint cool: {} {}", o, i);
            termios::cfsetspeed(&mut termios, termios::B19200);
        }
        //set magic flags from strace
        termios.c_iflag=0x14;
        termios.c_oflag=0x4;
        termios.c_cflag=0xdbe;
        termios.c_lflag=0xa20;
        termios.c_cc = [3, 28, 127, 21, 4, 0, 1, 0, 17, 19, 26, 0, 18, 15, 23, 22, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        //termios.c_cc[termios::VMIN]=1;
        //termios.c_cc[termios::VTIME]=0;
        if let Err(err) = termios::tcsetattr(fd, termios::TCSANOW, &termios) {
            return Err(KDriveErr::FailedToOpenSerialPort);
        }
        let port = TTYPort(fd);
        
        ap.port = Some(Arc::new(Mutex::new(port)));
        
        let mut ft12 = KDriveFT12(ap);
        
        // Send reset request to initialize the connection
        ft12.send_reset()?;
        
        // Complete device initialization sequence
        ft12.initialize_device()?;
        
        // Start receiver thread
        ft12.start_receiver_thread();
        
        Ok(ft12)
    }
    
    fn send_reset(&mut self) -> Result<(), KDriveErr> {
        if let Some(ref port_arc) = self.0.port {
            let mut port = port_arc.lock().unwrap();
            // Send reset request: 10 40 40 16
            let reset_frame = [FT12_RESET_REQUEST, FT12_RESET_INDICATION, FT12_RESET_INDICATION, FT12_FRAME_END];
            port.write_all(&reset_frame).map_err(|_| KDriveErr::ResetRequestFailed)?;
            
            // Wait for acknowledge
            let mut ack_buf = [0u8; 1];
            port.wait_read_fd(Some(Duration::from_millis(100))).map_err(|_|KDriveErr::TimeoutWaitingForDeviceReadyIndication)?;
            port.read_exact(&mut ack_buf).map_err(|e| {
                eprintln!("Read IO Err: {e}");
                KDriveErr::ResetAcknowledgeFailed
            })?;
            
            if ack_buf[0] != FT12_ACKNOWLEDGE {
                return Err(KDriveErr::WrongResetAcknowledge); // Wrong acknowledge
            }
            
            Ok(())
        } else {
            Err(KDriveErr::NotConnected) // Not connected
        }
    }
    
    /// Initialize the KNX device with the complete startup sequence.
    /// 
    /// This method replicates the initialization sequence observed in the tty_trace:
    /// 1. Initial configuration request
    /// 2. Multiple property read requests for device configuration
    /// 
    /// The sequence is based on lines 12-46 from the trace file and establishes
    /// proper communication with the KNX interface before normal operation begins.
    fn initialize_device(&mut self) -> Result<(), KDriveErr> {
        println!("Starting KNX device initialization...");
        
        if let Some(ref port_arc) = self.0.port {
            let port_clone = Arc::clone(port_arc);
            
            // Step 1: Initial configuration request (line 12 in trace)
            // Request: \x68\x02\x02\x68\x73\xa7\x1a\x16
            // Expected response: \x68\x0c\x0c\x68\xf3\xa8\xff\xff\x00\xc5\x01\x03\xa2\xe2\x00\x04\xea\x16
            let init_frame = Ft12Frame::new(FT12_DATA_REQUEST, vec![0xa7]);
            self.send_frame(&init_frame)?;
            self.expect_specific_response(&port_clone, &[FT12_DATA_CONFIRM, 0xa8, 0xff, 0xff, 0x00, 0xc5, 0x01, 0x03, 0xa2, 0xe2, 0x00, 0x04], "initial config")?;
            
            // Step 2: Property read 1 (line 18)
            // Request: \x68\x08\x08\x68\x53\xfc\x00\x08\x01\x40\x10\x01\xa9\x16
            // Expected response: \x68\x0a\x0a\x68\xd3\xfb\x00\x08\x01\x40\x10\x01\x00\x0b\x33\x16
            let prop1_frame = Ft12Frame::new(0x53, vec![0xfc, 0x00, 0x08, 0x01, 0x40, 0x10, 0x01]);
            self.send_frame(&prop1_frame)?;
            self.expect_specific_response(&port_clone, &[FT12_DATA_INDICATION, 0xfb, 0x00, 0x08, 0x01, 0x40, 0x10, 0x01, 0x00, 0x0b], "property read 1")?;
            
            // Step 3: Property read 2 (line 23)
            // Request: \x68\x09\x09\x68\x73\xf6\x00\x08\x01\x34\x10\x01\x00\xb7\x16
            // Expected response: \x68\x08\x08\x68\xf3\xf5\x00\x08\x01\x34\x10\x01\x36\x16
            let prop2_frame = Ft12Frame::new(FT12_DATA_REQUEST, vec![0xf6, 0x00, 0x08, 0x01, 0x34, 0x10, 0x01, 0x00]);
            self.send_frame(&prop2_frame)?;
            self.expect_specific_response(&port_clone, &[FT12_DATA_CONFIRM, 0xf5, 0x00, 0x08, 0x01, 0x34, 0x10, 0x01], "property read 2")?;
            
            // Step 4: Property read 3 (line 28)
            // Request: \x68\x08\x08\x68\x53\xfc\x00\x08\x01\x34\x10\x01\x9d\x16
            // Expected response: \x68\x09\x09\x68\xd3\xfb\x00\x08\x01\x34\x10\x01\x00\x1c\x16
            let prop3_frame = Ft12Frame::new(0x53, vec![0xfc, 0x00, 0x08, 0x01, 0x34, 0x10, 0x01]);
            self.send_frame(&prop3_frame)?;
            self.expect_specific_response(&port_clone, &[FT12_DATA_INDICATION, 0xfb, 0x00, 0x08, 0x01, 0x34, 0x10, 0x01, 0x00], "property read 3")?;
            
            // Step 5: Property read 4 (line 33)
            // Request: \x68\x08\x08\x68\x73\xfc\x00\x08\x01\x33\x10\x01\xbc\x16
            // Expected response: \x68\x0a\x0a\x68\xf3\xfb\x00\x08\x01\x33\x10\x01\x00\x02\x3d\x16
            let prop4_frame = Ft12Frame::new(FT12_DATA_REQUEST, vec![0xfc, 0x00, 0x08, 0x01, 0x33, 0x10, 0x01]);
            self.send_frame(&prop4_frame)?;
            self.expect_specific_response(&port_clone, &[FT12_DATA_CONFIRM, 0xfb, 0x00, 0x08, 0x01, 0x33, 0x10, 0x01, 0x00, 0x02], "property read 4")?;
            
            // Step 6: Property read 5 (line 38)
            // Request: \x68\x08\x08\x68\x53\xfc\x00\x00\x01\x38\x10\x01\x99\x16
            // Expected response: \x68\x0a\x0a\x68\xd3\xfb\x00\x00\x01\x38\x10\x01\x00\x37\x4f\x16
            let prop5_frame = Ft12Frame::new(0x53, vec![0xfc, 0x00, 0x00, 0x01, 0x38, 0x10, 0x01]);
            self.send_frame(&prop5_frame)?;
            self.expect_specific_response(&port_clone, &[FT12_DATA_INDICATION, 0xfb, 0x00, 0x00, 0x01, 0x38, 0x10, 0x01, 0x00, 0x37], "property read 5")?;
            
            println!("KNX device initialization completed successfully");
            Ok(())
        } else {
            Err(KDriveErr::NotConnected) // Not connected
        }
    }
    
    fn expect_specific_response(
        &self, 
        port_arc: &Arc<Mutex<TTYPort>>, 
        expected_data: &[u8], 
        step_name: &str
    ) -> Result<(), KDriveErr> {
        let mut port = port_arc.lock().unwrap();
        let mut response_buffer = Vec::new();
        let mut temp_buf = [0u8; 64];
        
        // Read response with timeout
        for _attempt in 0..20 {
            port.wait_read_fd(Some(Duration::from_secs(10))).map_err(|_|KDriveErr::TimeoutWaitingForDeviceReadyIndication)?;
            match port.read(&mut temp_buf) {
                Ok(bytes_read) if bytes_read > 0 => {
                    response_buffer.extend_from_slice(&temp_buf[..bytes_read]);
                    
                    // Try to parse complete frame
                    if let Some(frame_data) = self.extract_complete_frame(&response_buffer) {
                        // Parse FT1.2 frame
                        match Ft12Frame::from_bytes(&frame_data) {
                            Ok(frame) => {
                                // Validate response data matches expected
                                if frame.data == expected_data[1..] && frame.control == expected_data[0] {
                                    // Send acknowledgment
                                    port.write_all(&[FT12_ACKNOWLEDGE]).map_err(|_| KDriveErr::FailedToSendResponseAck2)?;
                                    return Ok(());
                                } else {
                                    println!("Step '{}': Unexpected response data", step_name);
                                    println!("Expected: {:02x?}", &expected_data[1..]);
                                    println!("Received: {:02x?}", &frame.data[..expected_data.len().min(frame.data.len())]);
                                    return Err(KDriveErr::UnexpectedResponseDataDuringInitialization); // Unexpected response data
                                }
                            }
                            Err(e) => {
                                println!("Step '{}': Failed to parse response frame: {}", step_name, e);
                                continue;
                            }
                        }
                    }
                }
                Ok(_) => {
                    // No data, wait a bit
                    thread::sleep(Duration::from_millis(50));
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {
                    continue;
                }
                Err(e) => {
                    println!("Step '{}': Read error: {}", step_name, e);
                    return Err(KDriveErr::ReadErrorDuringResponseValidation); // Read error
                }
            }
        }
        
        println!("Step '{}': Timeout waiting for response", step_name);
        Err(KDriveErr::TimeoutWaitingForInitializationResponse2) // Timeout
    }
    
    fn extract_complete_frame(&self, buffer: &[u8]) -> Option<Vec<u8>> {
        // Look for FT1.2 frame start (0x68)
        if let Some(start_pos) = buffer.iter().position(|&b| b == FT12_FRAME_START) {
            if buffer.len() >= start_pos + 4 {
                let length = buffer[start_pos + 1] as usize;
                let total_frame_length = length + 6; // length + 4 header bytes + checksum + end
                
                if buffer.len() >= start_pos + total_frame_length {
                    return Some(buffer[start_pos..start_pos + total_frame_length].to_vec());
                }
            }
        }
        None
    }
    
    fn start_receiver_thread(&mut self) {
        if let Some(ref port_arc) = self.0.port {
            let callbacks = Arc::clone(&self.0.callbacks);
            let port_clone = Arc::clone(port_arc);
            let (stop_sender, stop_receiver) = std::sync::mpsc::channel();
            
            self.0.stop_sender = Some(stop_sender);
            
            let _handle = thread::spawn(move || {
                let mut buffer = vec![0u8; 256];
                
                loop {
                    // Check if we should stop
                    if stop_receiver.try_recv().is_ok() {
                        break;
                    }
                    
                    // Read data from serial port with timeout
                    let read_result = Self::read_and_ack_whole_frame(&port_clone, &mut buffer);
                    
                    match read_result {
                        Ok(frame) => {
                            println!("Receiver thread got frame: {:02x?}", frame);
                            if let Err(e) = Self::process_frame(frame, &callbacks) {
                                println!("Error processing frame: {}", e);
                            }
                        
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {
                            // Timeout, continue
                            println!("timeout waiting for frame");
                            continue;
                        }
                        Err(e) => {
                            println!("Serial read error: {}", e);
                            return;
                        }
                    }
                }
            });
            
            self.0._receiver_thread = Some(_handle);
        }
    }
    fn read_and_ack_whole_frame<'buf>(m_port: &Arc<Mutex<TTYPort>>, buf: &'buf mut [u8]) -> std::io::Result<&'buf [u8]> {
        let port = m_port.lock().unwrap();
        //timeout so the lock is realeased for sending
        port.wait_read_fd(Some(Duration::from_secs(1)))?;
        let mut read = port.read(buf)?;
        if read == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "EOF reached"));
        }
        // we should not pick up stray acks here, so it needs to be a frame start
        if buf[0] != FT12_FRAME_START {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "Not a frame start"));
        }
        if read == 1 {
            //only frame start read, need to read length and rest
            port.wait_read_fd(None)?;
            port.read_exact(&mut buf[1..4])?;
            read = 4;
        }
        let length = buf[1] as usize +6;    // length + 4 header bytes + checksum + end
        port.wait_read_fd(None)?;
        port.read_exact(&mut buf[read..length])?;
        //send ack
        port.write_all(&[FT12_ACKNOWLEDGE])?;
        Ok(&buf[..length])
    }
    
    fn process_frame(frame_data: &[u8], callbacks: &Arc<Mutex<Vec<CallbackEntry>>>) -> Result<(), KDriveErr> {
        // Handle single-byte acknowledge
        if frame_data.len() == 1 && frame_data[0] == FT12_ACKNOWLEDGE {
            // Just an acknowledge, ignore
            return Ok(());
        }
        
        // Parse FT1.2 frame
        let frame = Ft12Frame::from_bytes(frame_data)?;
        
        // Check if this is a data indication (incoming telegram)
        if frame.control == FT12_DATA_INDICATION {
            // Extract cEMI data from frame
            let cemi_data = &frame.data;
            
            // Dispatch to callbacks
            let callbacks_guard = callbacks.lock().unwrap();
            for entry in callbacks_guard.iter() {
                // Call the callback with cEMI data
                (entry.callback)(
                    cemi_data.as_ptr(),
                    cemi_data.len() as u32,
                    entry.user_data,
                );
            }
        }
        
        Ok(())
    }
}
impl core::ops::Deref for KDriveFT12 {
    type Target = KDrive;
    fn deref(&self) -> &KDrive {
        &self.0
    }
}

impl core::ops::DerefMut for KDriveFT12 {
    fn deref_mut(&mut self) -> &mut KDrive {
        &mut self.0
    }
}

impl Drop for KDrive {
    fn drop(&mut self) {
        // Signal receiver thread to stop
        if let Some(ref stop_sender) = self.stop_sender {
            let _ = stop_sender.send(());
        }
        
        // Wait for receiver thread to finish
        if let Some(handle) = self._receiver_thread.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for KDriveFT12 {
    fn drop(&mut self) {
        // KDrive's drop will handle the receiver thread cleanup
        // Serial port cleanup is handled by SerialPort's Drop implementation
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
