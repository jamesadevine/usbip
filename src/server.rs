use crate::{
    socket::{reader, writer},
    EndpointAttributes, UsbDevice, UsbEndpoint, UsbHostDeviceHandler, UsbHostInterfaceHandler,
    UsbInterface, UsbInterfaceHandler,
};
use log::*;
use rusb::*;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::{net::TcpListener, sync::Mutex};

/// Main struct of a USB/IP server
pub struct UsbIpServer {
    pub devices: Vec<UsbDevice>,
}

impl UsbIpServer {
    /// Create a [UsbIpServer] with simulated devices
    pub fn new_simulated(devices: Vec<UsbDevice>) -> Self {
        Self { devices }
    }

    fn with_devices(device_list: Vec<Device<GlobalContext>>) -> Vec<UsbDevice> {
        let mut devices = vec![];

        for dev in device_list {
            let open_device = match dev.open() {
                Ok(dev) => dev,
                Err(err) => {
                    println!("Impossible to share {dev:?}: {err}");
                    continue;
                }
            };
            let handle = Arc::new(std::sync::Mutex::new(open_device));
            let desc = dev.device_descriptor().unwrap();
            let cfg = dev.active_config_descriptor().unwrap();
            let mut interfaces = vec![];
            handle
                .lock()
                .unwrap()
                .set_auto_detach_kernel_driver(true)
                .ok();
            for intf in cfg.interfaces() {
                // ignore alternate settings
                let intf_desc = intf.descriptors().next().unwrap();
                handle
                    .lock()
                    .unwrap()
                    .set_auto_detach_kernel_driver(true)
                    .ok();
                handle
                    .lock()
                    .unwrap()
                    .claim_interface(intf.number())
                    .unwrap();
                let mut endpoints = vec![];

                for ep_desc in intf_desc.endpoint_descriptors() {
                    endpoints.push(UsbEndpoint {
                        address: ep_desc.address(),
                        attributes: ep_desc.transfer_type() as u8,
                        max_packet_size: ep_desc.max_packet_size(),
                        interval: ep_desc.interval(),
                    });
                }

                let handler = Arc::new(std::sync::Mutex::new(
                    Box::new(UsbHostInterfaceHandler::new(handle.clone()))
                        as Box<dyn UsbInterfaceHandler + Send>,
                ));
                interfaces.push(UsbInterface {
                    interface_class: intf_desc.class_code(),
                    interface_subclass: intf_desc.sub_class_code(),
                    interface_protocol: intf_desc.protocol_code(),
                    endpoints,
                    string_interface: intf_desc.description_string_index().unwrap_or(0),
                    class_specific_descriptor: Vec::from(intf_desc.extra()),
                    handler,
                });
            }
            let mut device = UsbDevice {
                path: format!(
                    "/sys/bus/{}/{}/{}",
                    dev.bus_number(),
                    dev.address(),
                    dev.port_number()
                ),
                bus_id: format!(
                    "{}-{}-{}",
                    dev.bus_number(),
                    dev.address(),
                    dev.port_number()
                ),
                bus_num: dev.bus_number() as u32,
                dev_num: dev.port_number() as u32,
                speed: dev.speed() as u32,
                vendor_id: desc.vendor_id(),
                product_id: desc.product_id(),
                device_class: desc.class_code(),
                device_subclass: desc.sub_class_code(),
                device_protocol: desc.protocol_code(),
                device_bcd: desc.device_version().into(),
                configuration_value: cfg.number(),
                num_configurations: desc.num_configurations(),
                ep0_in: UsbEndpoint {
                    address: 0x80,
                    attributes: EndpointAttributes::Control as u8,
                    max_packet_size: desc.max_packet_size() as u16,
                    interval: 0,
                },
                ep0_out: UsbEndpoint {
                    address: 0x00,
                    attributes: EndpointAttributes::Control as u8,
                    max_packet_size: desc.max_packet_size() as u16,
                    interval: 0,
                },
                interfaces,
                device_handler: Some(Arc::new(std::sync::Mutex::new(Box::new(
                    UsbHostDeviceHandler::new(handle.clone()),
                )))),
                usb_version: desc.usb_version().into(),
                ..UsbDevice::default()
            };

            // set strings
            if let Some(index) = desc.manufacturer_string_index() {
                device.string_manufacturer = device.new_string(
                    &handle
                        .lock()
                        .unwrap()
                        .read_string_descriptor_ascii(index)
                        .unwrap(),
                )
            }
            if let Some(index) = desc.product_string_index() {
                device.string_product = device.new_string(
                    &handle
                        .lock()
                        .unwrap()
                        .read_string_descriptor_ascii(index)
                        .unwrap(),
                )
            }
            if let Some(index) = desc.serial_number_string_index() {
                device.string_serial = device.new_string(
                    &handle
                        .lock()
                        .unwrap()
                        .read_string_descriptor_ascii(index)
                        .unwrap(),
                )
            }
            devices.push(device);
        }
        devices
    }

    /// Create a [UsbIpServer] exposing devices in the host, and redirect all USB transfers to them using libusb
    pub fn new_from_host() -> Self {
        match rusb::devices() {
            Ok(list) => {
                let mut devs = vec![];
                for d in list.iter() {
                    devs.push(d)
                }
                Self {
                    devices: Self::with_devices(devs),
                }
            }
            Err(_) => Self { devices: vec![] },
        }
    }

    pub fn new_from_host_with_filter<F>(filter: F) -> Self
    where
        F: FnMut(&Device<GlobalContext>) -> bool,
    {
        match rusb::devices() {
            Ok(list) => {
                let mut devs = vec![];
                for d in list.iter().filter(filter) {
                    devs.push(d)
                }
                Self {
                    devices: Self::with_devices(devs),
                }
            }
            Err(_) => Self { devices: vec![] },
        }
    }
}

#[derive(Clone, PartialEq)]
pub enum UsbIpCommand {
    ReqDevlist,
    ReqImport,
    CmdSubmit,
    CmdUnlink,
}

#[derive(Clone)]
pub struct UsbIpPacket {
    pub status: u32,
    pub sequence_number: u32,
    pub command: Vec<u8>,
    pub enum_command: UsbIpCommand,
    pub data: Vec<u8>,
}

/// Spawn a USB/IP server at `addr` using [TcpListener]
pub async fn server(addr: SocketAddr, server: UsbIpServer) {
    let listener = TcpListener::bind(addr).await.expect("bind to addr");

    let server = async move {
        let usbip_server = Arc::new(server);
        loop {
            match listener.accept().await {
                Ok((socket, _addr)) => {
                    info!("Got connection from {:?}", socket.peer_addr());

                    let packet_queue: Arc<Mutex<Vec<UsbIpPacket>>> = Arc::new(Mutex::new(vec![]));
                    let new_server = usbip_server.clone();
                    let pqueue = packet_queue.clone();
                    let (mut sock_reader, mut sock_writer) = tokio::io::split(socket);
                    let _t = tokio::spawn(async move {
                        let res = reader(&mut sock_reader, new_server, pqueue).await;
                        info!("Reader ended with {:?}", res);
                    });
                    let new_server = usbip_server.clone();
                    let pqueue = packet_queue.clone();
                    let _t1 = tokio::spawn(async move {
                        let res = writer(&mut sock_writer, new_server, pqueue).await;
                        info!("Writer ended with {:?}", res);
                    });
                }
                Err(err) => {
                    warn!("Got error {:?}", err);
                }
            }
        }
    };

    server.await
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::cdc;
    use crate::util::tests::*;
    use crate::ClassCode;

    #[tokio::test]
    async fn req_empty_devlist() {
        let server = UsbIpServer { devices: vec![] };

        // OP_REQ_DEVLIST
        let mut mock_socket = MockSocket::new(vec![0x01, 0x11, 0x80, 0x05, 0x00, 0x00, 0x00, 0x00]);
        handler(&mut mock_socket, Arc::new(server)).await.ok();
        // OP_REP_DEVLIST
        assert_eq!(
            mock_socket.output,
            [0x01, 0x11, 0x00, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]
        );
    }

    #[tokio::test]
    async fn req_sample_devlist() {
        let intf_handler = Arc::new(Mutex::new(
            Box::new(cdc::UsbCdcAcmHandler::new()) as Box<dyn UsbInterfaceHandler + Send>
        ));
        let server = UsbIpServer {
            devices: vec![UsbDevice::new(0).with_interface(
                ClassCode::CDC as u8,
                cdc::CDC_ACM_SUBCLASS,
                0x00,
                "Test CDC ACM",
                cdc::UsbCdcAcmHandler::endpoints(),
                intf_handler.clone(),
            )],
        };

        // OP_REQ_DEVLIST
        let mut mock_socket = MockSocket::new(vec![0x01, 0x11, 0x80, 0x05, 0x00, 0x00, 0x00, 0x00]);
        handler(&mut mock_socket, Arc::new(server)).await.ok();
        // OP_REP_DEVLIST
        // header: 0xC
        // device: 0x138
        // interface: 4 * 0x1
        assert_eq!(mock_socket.output.len(), 0xC + 0x138 + 4 * 0x1);
    }

    #[tokio::test]
    async fn req_import() {
        let intf_handler = Arc::new(Mutex::new(
            Box::new(cdc::UsbCdcAcmHandler::new()) as Box<dyn UsbInterfaceHandler + Send>
        ));
        let server = UsbIpServer {
            devices: vec![UsbDevice::new(0).with_interface(
                ClassCode::CDC as u8,
                cdc::CDC_ACM_SUBCLASS,
                0x00,
                "Test CDC ACM",
                cdc::UsbCdcAcmHandler::endpoints(),
                intf_handler.clone(),
            )],
        };

        // OP_REQ_IMPORT
        let mut req = vec![0x01, 0x11, 0x80, 0x03, 0x00, 0x00, 0x00, 0x00];
        let mut path = "0".as_bytes().to_vec();
        path.resize(32, 0);
        req.extend(path);
        let mut mock_socket = MockSocket::new(req);
        handler(&mut mock_socket, Arc::new(server)).await.ok();
        // OP_REQ_IMPORT
        assert_eq!(mock_socket.output.len(), 0x140);
    }

    #[tokio::test]
    async fn req_import_get_device_desc() {
        let intf_handler = Arc::new(Mutex::new(
            Box::new(cdc::UsbCdcAcmHandler::new()) as Box<dyn UsbInterfaceHandler + Send>
        ));
        let server = UsbIpServer {
            devices: vec![UsbDevice::new(0).with_interface(
                ClassCode::CDC as u8,
                cdc::CDC_ACM_SUBCLASS,
                0x00,
                "Test CDC ACM",
                cdc::UsbCdcAcmHandler::endpoints(),
                intf_handler.clone(),
            )],
        };

        // OP_REQ_IMPORT
        let mut req = vec![0x01, 0x11, 0x80, 0x03, 0x00, 0x00, 0x00, 0x00];
        let mut path = "0".as_bytes().to_vec();
        path.resize(32, 0);
        req.extend(path);
        // USBIP_CMD_SUBMIT
        req.extend(vec![
            0x00, 0x00, 0x00, 0x01, // command
            0x00, 0x00, 0x00, 0x01, // seq num
            0x00, 0x00, 0x00, 0x00, // dev id
            0x00, 0x00, 0x00, 0x01, // IN
            0x00, 0x00, 0x00, 0x00, // ep 0
            0x00, 0x00, 0x00, 0x00, // transfer flags
            0x00, 0x00, 0x00, 0x00, // transfer buffer length
            0x00, 0x00, 0x00, 0x00, // start frame
            0x00, 0x00, 0x00, 0x00, // number of packets
            0x00, 0x00, 0x00, 0x00, // interval
            0x80, 0x06, 0x00, 0x01, 0x00, 0x00, 0x40, 0x00, // GetDescriptor to Device
        ]);
        let mut mock_socket = MockSocket::new(req);
        handler(&mut mock_socket, Arc::new(server)).await.ok();
        // OP_REQ_IMPORT + USBIP_CMD_SUBMIT + Device Descriptor
        assert_eq!(mock_socket.output.len(), 0x140 + 0x30 + 0x12);
    }
}
