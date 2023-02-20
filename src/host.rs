//! Host USB
use crate::{
    EndpointAttributes, SetupPacket, UsbDeviceHandler, UsbEndpoint, UsbInterface,
    UsbInterfaceHandler,
};
use log::*;
use rusb::{DeviceHandle, Direction, GlobalContext};
use std::any::Any;
use std::sync::{Arc, Mutex};
use std::io::{Result};

/// A handler to pass requests to a USB device of the host
#[derive(Clone)]
pub struct UsbHostInterfaceHandler {
    handle: Arc<Mutex<DeviceHandle<GlobalContext>>>,
}

impl UsbHostInterfaceHandler {
    pub fn new(handle: Arc<Mutex<DeviceHandle<GlobalContext>>>) -> Self {
        Self { handle }
    }
}

impl UsbInterfaceHandler for UsbHostInterfaceHandler {
    fn handle_urb(
        &mut self,
        _interface: &UsbInterface,
        ep: UsbEndpoint,
        setup: SetupPacket,
        req: &[u8],
    ) -> Result<Vec<u8>> {
        // debug!(
        //     "To host device: ep={:?} setup={:?} req={:?}",
        //     ep, setup, req
        // );
        let mut buffer = vec![0u8; ep.max_packet_size as usize];
        let timeout = std::time::Duration::new(1, 0);
        let handle = self.handle.lock().unwrap();
        if ep.attributes == EndpointAttributes::Control as u8 {
            // control
            if let Direction::In = ep.direction() {
                // control in
                if let Ok(len) = handle.read_control(
                    setup.request_type,
                    setup.request,
                    setup.value,
                    setup.index,
                    &mut buffer,
                    timeout,
                ) {
                    return Ok(Vec::from(&buffer[..len]));
                }
            } else {
                // control out
                handle
                    .write_control(
                        setup.request_type,
                        setup.request,
                        setup.value,
                        setup.index,
                        req,
                        timeout,
                    )
                    .ok();
            }
        } else if ep.attributes == EndpointAttributes::Interrupt as u8 {
            // interrupt
            if let Direction::In = ep.direction() {
                // interrupt in
                if let Ok(len) = handle.read_interrupt(ep.address, &mut buffer, timeout) {
                    info!("intr in {:?}", &buffer[..len]);
                    return Ok(Vec::from(&buffer[..len]));
                }
            } else {
                // interrupt out
                handle.write_interrupt(ep.address, req, timeout).ok();
            }
        } else if ep.attributes == EndpointAttributes::Bulk as u8 {
            // bulk
            if let Direction::In = ep.direction() {
                // bulk in
                if let Ok(len) = handle.read_bulk(ep.address, &mut buffer, timeout) {
                    return Ok(Vec::from(&buffer[..len]));
                }
            } else {
                // bulk out
                handle.write_bulk(ep.address, req, timeout).ok();
            }
        }
        Ok(vec![])
    }

    fn get_class_specific_descriptor(&self) -> Vec<u8> {
        vec![]
    }

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

/// A handler to pass requests to a USB device of the host
#[derive(Clone)]
pub struct UsbHostDeviceHandler {
    handle: Arc<Mutex<DeviceHandle<GlobalContext>>>,
}

impl UsbHostDeviceHandler {
    pub fn new(handle: Arc<Mutex<DeviceHandle<GlobalContext>>>) -> Self {
        Self { handle }
    }
}

impl UsbDeviceHandler for UsbHostDeviceHandler {
    fn handle_urb(&mut self, setup: SetupPacket, req: &[u8]) -> Result<Vec<u8>> {
        debug!("Host device handler: setup={:x?} req={:?}", setup, req);
        let mut buffer = [0u8; 1024];
        let timeout = std::time::Duration::new(1, 0);
        let handle = self.handle.lock().unwrap();
        // control
        if setup.request_type & 0x80 == 0 {
            debug!("HDH: Write");
            // control out
            match handle.write_control(
                setup.request_type,
                setup.request,
                setup.value,
                setup.index,
                req,
                timeout,
            ) {
                Ok(usize) => debug!("Wrote {usize} bytes."),
                Err(e) => debug!("ERR: {e}"),
            }
        } else {
            debug!("HDH: Read");
            // control in
            match handle.read_control(
                setup.request_type,
                setup.request,
                setup.value,
                setup.index,
                &mut buffer,
                timeout,
            ) {
                Ok(len) => return Ok(Vec::from(&buffer[..len])),
                Err(e) => debug!("ERR COULD NOT READ {e}"),
            }
        }
        Ok(vec![])
    }

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}
