use core::ptr::NonNull;
use std::{
    ffi::CString,
    ops::{BitAnd, Shr},
    sync::{Arc, Mutex, mpsc::Sender},
    thread::{self, JoinHandle},
    time::Duration,
    io::{Read, Write},
};
use serialport::SerialPort;

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
pub const KDRIVE_MAX_GROUP_VALUE_LEN: usize = 14;

// FT1.2 Frame structure
#[derive(Debug, Clone)]
struct Ft12Frame {
    start: u8,      // 0x68
    length: u8,     // Length of data part
    length2: u8,    // Repeated length
    start2: u8,     // 0x68 again
    control: u8,    // Control field
    data: Vec<u8>,  // Data payload
    checksum: u8,   // Checksum
    end: u8,        // 0x16
}

impl Ft12Frame {
    fn new(control: u8, data: Vec<u8>) -> Self {
        let length = (data.len() + 1) as u8; // +1 for control field
        let checksum = Self::calculate_checksum(control, &data);
        
        Self {
            start: FT12_FRAME_START,
            length,
            length2: length,
            start2: FT12_FRAME_START,
            control,
            data,
            checksum,
            end: FT12_FRAME_END,
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
        let mut bytes = Vec::new();
        bytes.push(self.start);
        bytes.push(self.length);
        bytes.push(self.length2);
        bytes.push(self.start2);
        bytes.push(self.control);
        bytes.extend_from_slice(&self.data);
        bytes.push(self.checksum);
        bytes.push(self.end);
        bytes
    }
    
    fn from_bytes(bytes: &[u8]) -> Result<Self, KDriveErr> {
        if bytes.len() < 8 {
            return Err(KDriveErr(1)); // Invalid frame length
        }
        
        if bytes[0] != FT12_FRAME_START || bytes[3] != FT12_FRAME_START {
            return Err(KDriveErr(2)); // Invalid frame start
        }
        
        let length = bytes[1];
        if bytes[2] != length {
            return Err(KDriveErr(3)); // Length mismatch
        }
        
        if bytes.len() != (length as usize + 6) {
            return Err(KDriveErr(4)); // Frame length mismatch
        }
        
        let control = bytes[4];
        let data_len = length as usize - 1;
        let data = bytes[5..5+data_len].to_vec();
        let checksum = bytes[5+data_len];
        let end = bytes[5+data_len+1];
        
        if end != FT12_FRAME_END {
            return Err(KDriveErr(5)); // Invalid frame end
        }
        
        let expected_checksum = Self::calculate_checksum(control, &data);
        if checksum != expected_checksum {
            return Err(KDriveErr(6)); // Checksum mismatch
        }
        
        Ok(Self {
            start: bytes[0],
            length,
            length2: bytes[2],
            start2: bytes[3],
            control,
            data,
            checksum,
            end,
        })
    }
}

pub struct KDrive {
    port: Option<Arc<Mutex<Box<dyn SerialPort>>>>,
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
            Err(KDriveErr(10)) // Not connected
        }
    }
    
    fn create_group_write_cemi(&self, addr: u16, data: &[u8]) -> Result<Vec<u8>, KDriveErr> {
        let mut cemi = Vec::new();
        
        // cEMI Message Code L_Data.req
        cemi.push(0x11);
        
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
        
        // Data length (NPDU + data)
        let data_len = 1 + data.len(); // +1 for TPCI/APCI
        cemi.push(data_len as u8);
        
        // TPCI (Transport layer) = 0x00, APCI (Application layer) = Group Value Write (0x80)
        // For 1-byte data, encode the data value in the lower 6 bits of APCI
        if data.len() == 1 {
            let apci = 0x0080 | (data[0] as u16 & 0x003F);
            cemi.push((apci >> 8) as u8);
            cemi.push((apci & 0xFF) as u8);
        } else {
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
                    match port.read_exact(&mut ack_buf) {
                        Ok(()) if ack_buf[0] == FT12_ACKNOWLEDGE => Ok(()),
                        Ok(()) => Err(KDriveErr(8)), // Wrong acknowledge
                        Err(_e) => Err(KDriveErr(9)), // Read error
                    }
                }
                Err(_e) => Err(KDriveErr(7)), // Write error
            }
        } else {
            Err(KDriveErr(10)) // Not connected
        }
    }
    
    pub fn register_telegram_callback<T>(
        &mut self,
        func: TelegramCallback<T>,
        user_data: Option<NonNull<T>>,
    ) -> Result<u32, KDriveErr> {
        let key = rand::random::<u32>();
        
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

pub struct KDriveFT12(KDrive);

impl KDriveFT12 {
    pub fn open(mut ap: KDrive, dev: &CString) -> Result<KDriveFT12, KDriveErr> {
        let port_name = dev.to_str().map_err(|_| KDriveErr(11))?;
        
        let port = serialport::new(port_name, 19200)
            .timeout(Duration::from_millis(1000))
            .open()
            .map_err(|_| KDriveErr(12))?;
        
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
            port.write_all(&reset_frame).map_err(|_| KDriveErr(13))?;
            
            // Wait for acknowledge
            let mut ack_buf = [0u8; 1];
            port.read_exact(&mut ack_buf).map_err(|_| KDriveErr(14))?;
            
            if ack_buf[0] != FT12_ACKNOWLEDGE {
                return Err(KDriveErr(15)); // Wrong acknowledge
            }
            
            Ok(())
        } else {
            Err(KDriveErr(10)) // Not connected
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
            let init_frame = Ft12Frame::new(FT12_DATA_REQUEST, vec![0xa7, 0x1a]);
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
            self.expect_specific_response(&port_clone, &[FT12_DATA_INDICATION, 0xfb, 0x00, 0x00, 0x01, 0x38, 0x10, 0x01, 0x00], "property read 5")?;
            
            // Step 7: Wait for device ready indication (line 43-46)
            // Expected: \x68\x0c\x0c\x68\xf3\x29\x00\xbc\xd0\x10\x01\x00\x01\x01\x00\x80\x3b\x16
            self.expect_device_ready_indication(&port_clone)?;
            
            println!("KNX device initialization completed successfully");
            Ok(())
        } else {
            Err(KDriveErr(10)) // Not connected
        }
    }
    
    fn expect_specific_response(
        &self, 
        port_arc: &Arc<Mutex<Box<dyn SerialPort>>>, 
        expected_data: &[u8], 
        step_name: &str
    ) -> Result<(), KDriveErr> {
        let mut port = port_arc.lock().unwrap();
        let mut response_buffer = Vec::new();
        let mut temp_buf = [0u8; 64];
        
        // Read response with timeout
        for _attempt in 0..20 {
            match port.read(&mut temp_buf) {
                Ok(bytes_read) if bytes_read > 0 => {
                    response_buffer.extend_from_slice(&temp_buf[..bytes_read]);
                    
                    // Try to parse complete frame
                    if let Some(frame_data) = self.extract_complete_frame(&response_buffer) {
                        // Parse FT1.2 frame
                        match Ft12Frame::from_bytes(&frame_data) {
                            Ok(frame) => {
                                println!("Step '{}': Received response with control=0x{:02x}, data len={}", 
                                    step_name, frame.control, frame.data.len());
                                
                                // Validate response data matches expected
                                if frame_data.starts_with(expected_data) {
                                    // Send acknowledgment
                                    port.write_all(&[FT12_ACKNOWLEDGE]).map_err(|_| KDriveErr(29))?;
                                    println!("Step '{}': Response validated successfully", step_name);
                                    return Ok(());
                                } else {
                                    println!("Step '{}': Unexpected response data", step_name);
                                    println!("Expected: {:02x?}", expected_data);
                                    println!("Received: {:02x?}", &frame_data[..expected_data.len().min(frame_data.len())]);
                                    return Err(KDriveErr(30)); // Unexpected response data
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
                    return Err(KDriveErr(31)); // Read error
                }
            }
        }
        
        println!("Step '{}': Timeout waiting for response", step_name);
        Err(KDriveErr(32)) // Timeout
    }
    
    fn expect_device_ready_indication(
        &self, 
        port_arc: &Arc<Mutex<Box<dyn SerialPort>>>
    ) -> Result<(), KDriveErr> {
        let mut port = port_arc.lock().unwrap();
        let mut response_buffer = Vec::new();
        let mut temp_buf = [0u8; 64];
        
        // Expected pattern from trace line 43-46: \xf3\x29\x00\xbc\xd0\x10\x01\x00\x01\x01\x00\x80
        let ready_pattern = [FT12_DATA_CONFIRM, 0x29, 0x00, 0xbc, 0xd0, 0x10, 0x01, 0x00, 0x01, 0x01, 0x00, 0x80];
        
        println!("Waiting for device ready indication...");
        
        // Read response with extended timeout for device ready
        for attempt in 0..40 {
            match port.read(&mut temp_buf) {
                Ok(bytes_read) if bytes_read > 0 => {
                    response_buffer.extend_from_slice(&temp_buf[..bytes_read]);
                    
                    // Look for the ready pattern in the buffer
                    if response_buffer.windows(ready_pattern.len()).any(|window| window == ready_pattern) {
                        // Send acknowledgment
                        port.write_all(&[FT12_ACKNOWLEDGE]).map_err(|_| KDriveErr(33))?;
                        println!("Device ready indication received and acknowledged");
                        return Ok(());
                    }
                    
                    // Keep only recent data to prevent buffer overflow
                    if response_buffer.len() > 256 {
                        response_buffer.drain(0..128);
                    }
                }
                Ok(_) => {
                    thread::sleep(Duration::from_millis(100));
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {
                    continue;
                }
                Err(e) => {
                    println!("Device ready: Read error: {}", e);
                    return Err(KDriveErr(34)); // Read error
                }
            }
        }
        
        println!("Timeout waiting for device ready indication");
        Err(KDriveErr(35)) // Timeout
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
                let mut frame_buffer = Vec::new();
                
                loop {
                    // Check if we should stop
                    if let Ok(_) = stop_receiver.try_recv() {
                        break;
                    }
                    
                    // Read data from serial port with timeout
                    let read_result = {
                        let mut port = port_clone.lock().unwrap();
                        port.read(&mut buffer)
                    };
                    
                    match read_result {
                        Ok(bytes_read) if bytes_read > 0 => {
                            frame_buffer.extend_from_slice(&buffer[..bytes_read]);
                            
                            // Process complete frames
                            while let Some(frame) = Self::extract_frame(&mut frame_buffer) {
                                if let Err(e) = Self::process_frame(&frame, &callbacks) {
                                    println!("Error processing frame: {}", e);
                                }
                            }
                        }
                        Ok(_) => {
                            // No data, continue
                            thread::sleep(Duration::from_millis(10));
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {
                            // Timeout, continue
                            continue;
                        }
                        Err(e) => {
                            println!("Serial read error: {}", e);
                            thread::sleep(Duration::from_millis(100));
                        }
                    }
                }
            });
            
            self.0._receiver_thread = Some(_handle);
        }
    }
    
    fn extract_frame(buffer: &mut Vec<u8>) -> Option<Vec<u8>> {
        // Look for FT1.2 frame start (0x68)
        if let Some(start_pos) = buffer.iter().position(|&b| b == FT12_FRAME_START) {
            if start_pos > 0 {
                // Remove data before frame start
                buffer.drain(0..start_pos);
            }
            
            if buffer.len() >= 4 {
                // Check if we have enough data to read the length
                let length = buffer[1] as usize;
                let total_frame_length = length + 6; // length + 4 header bytes + checksum + end
                
                if buffer.len() >= total_frame_length {
                    // We have a complete frame
                    let frame = buffer.drain(0..total_frame_length).collect();
                    return Some(frame);
                }
            }
        } else if buffer.len() > 256 {
            // Clear buffer if it gets too large without finding a frame start
            buffer.clear();
        }
        
        None
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
            return Err(KDriveErr(16));
        }
        Ok(u16::from_be_bytes([self.data[6], self.data[7]]))
    }
    
    pub fn get_group_data<'b>(
        &self,
        msg: &'b mut [u8; KDRIVE_MAX_GROUP_VALUE_LEN],
    ) -> Result<&'b [u8], KDriveErr> {
        if self.data.len() < 9 {
            return Err(KDriveErr(17));
        }
        
        let len = self.data[8] as usize;
        if len == 0 {
            return Ok(&msg[..0]);
        }
        
        let payload_start = 10;
        if self.data.len() < payload_start + len {
            return Err(KDriveErr(18));
        }
        
        let payload = &self.data[payload_start..payload_start + len];
        
        // Copy payload to buffer
        if payload.len() > KDRIVE_MAX_GROUP_VALUE_LEN {
            return Err(KDriveErr(19));
        }
        
        msg[..payload.len()].copy_from_slice(payload);
        
        // Mask the first byte to get only the data part (remove APCI bits)
        if payload.len() > 0 {
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
pub struct KDriveErr(pub u32);

impl std::fmt::Display for KDriveErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let msg = match self.0 {
            1 => "Invalid frame length",
            2 => "Invalid frame start",
            3 => "Length mismatch",
            4 => "Frame length mismatch",
            5 => "Invalid frame end",
            6 => "Checksum mismatch",
            7 => "Write error",
            8 => "Wrong acknowledge",
            9 => "Read error",
            10 => "Not connected",
            11 => "Invalid device path",
            12 => "Failed to open serial port",
            13 => "Reset request failed",
            14 => "Reset acknowledge failed",
            15 => "Wrong reset acknowledge",
            16 => "cEMI frame too short for destination",
            17 => "cEMI frame too short for data length",
            18 => "cEMI frame truncated",
            19 => "cEMI data too large",
            20 => "Failed to send initialization frame",
            21 => "Failed to receive initialization acknowledgment",
            22 => "Wrong initialization acknowledgment",
            23 => "Failed to send response acknowledgment",
            24 => "Failed to read initialization response",
            25 => "Timeout waiting for initialization response",
            26 => "Failed to acknowledge device ready indication",
            27 => "Failed to read device ready indication",
            28 => "Timeout waiting for device ready indication",
            29 => "Failed to send response acknowledgment",
            30 => "Unexpected response data during initialization",
            31 => "Read error during response validation",
            32 => "Timeout waiting for initialization response",
            33 => "Failed to acknowledge device ready indication",
            34 => "Read error waiting for device ready",
            35 => "Timeout waiting for device ready indication",
            _ => "Unknown error",
        };
        write!(f, "KDriveErr({}): {}", self.0, msg)
    }
}
impl std::error::Error for KDriveErr {}

impl From<KDriveErr> for std::io::Error {
    fn from(error: KDriveErr) -> Self {
        std::io::Error::new(std::io::ErrorKind::Other, error)
    }
}
