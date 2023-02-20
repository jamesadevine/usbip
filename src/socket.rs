use crate::server::UsbIpServer;
use libc::ECONNRESET;
use log::*;
use std::io::{ErrorKind, Result};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub async fn handler<T: AsyncReadExt + AsyncWriteExt + Unpin>(
    mut socket: &mut T,
    server: Arc<UsbIpServer>,
) -> Result<()> {
    let mut current_import_device = None;
    loop {
        let mut command = [0u8; 4];
        if let Err(err) = socket.read_exact(&mut command).await {
            if err.kind() == ErrorKind::UnexpectedEof {
                info!("Remote closed the connection");
                return Ok(());
            } else {
                return Err(err);
            }
        }
        match command {
            [0x01, 0x11, 0x80, 0x05] => {
                trace!("Got OP_REQ_DEVLIST");
                let _status = socket.read_u32().await?;

                // OP_REP_DEVLIST
                socket.write_u32(0x01110005).await?;
                socket.write_u32(0).await?;
                socket.write_u32(server.devices.len() as u32).await?;
                for dev in &server.devices {
                    dev.write_dev_with_interfaces(&mut socket).await?;
                }
                trace!("Sent OP_REP_DEVLIST");
            }
            [0x01, 0x11, 0x80, 0x03] => {
                trace!("Got OP_REQ_IMPORT");
                let _status = socket.read_u32().await?;
                let mut bus_id = [0u8; 32];
                socket.read_exact(&mut bus_id).await?;
                current_import_device = None;
                for device in &server.devices {
                    let mut expected = device.bus_id.as_bytes().to_vec();
                    expected.resize(32, 0);
                    if expected == bus_id {
                        current_import_device = Some(device);
                        info!("Found device {:?}", device.path);
                        break;
                    }
                }

                // OP_REP_IMPORT
                trace!("Sent OP_REP_IMPORT");
                socket.write_u32(0x01110003).await?;
                if let Some(dev) = current_import_device {
                    socket.write_u32(0).await?;
                    dev.write_dev(&mut socket).await?;
                } else {
                    socket.write_u32(1).await?;
                }
            }
            [0x00, 0x00, 0x00, 0x01] => {
                let seq_num = socket.read_u32().await?;
                let dev_id = socket.read_u32().await?;
                let direction = socket.read_u32().await?;
                let ep = socket.read_u32().await?;
                let _transfer_flags = socket.read_u32().await?;
                let transfer_buffer_length = socket.read_u32().await?;
                let _start_frame = socket.read_u32().await?;
                let _number_of_packets = socket.read_u32().await?;
                let _interval = socket.read_u32().await?;
                let mut setup = [0u8; 8];
                socket.read_exact(&mut setup).await?;
                let device = current_import_device.unwrap();
                let real_ep = if direction == 0 { ep } else { ep | 0x80 };
                let (usb_ep, intf) = device.find_ep(real_ep as u8).unwrap();

                let resp = device
                    .handle_urb(socket, usb_ep, intf, transfer_buffer_length, setup)
                    .await?;

                if usb_ep.address != 0x85 {
                    trace!("Got USBIP_CMD_SUBMIT [{seq_num}]");
                    trace!("NUMBER OF PACKETS {_number_of_packets}");
                    trace!("->Endpoint {:02x?}", usb_ep);
                    trace!("->Setup {:02x?}", setup);
                    trace!("<-Resp {:02x?}", resp);
                }

                // USBIP_RET_SUBMIT
                // command
                socket.write_u32(0x3).await?;
                socket.write_u32(seq_num).await?;
                socket.write_u32(dev_id).await?;
                socket.write_u32(direction).await?;
                socket.write_u32(ep).await?;
                // status
                socket.write_u32(0).await?;
                // actual length
                socket.write_u32(resp.len() as u32).await?;
                // start frame
                socket.write_u32(0).await?;
                // number of packets
                socket.write_u32(0).await?;
                // error count
                socket.write_u32(0).await?;
                // setup
                socket.write_all(&setup).await?;
                // data
                socket.write_all(&resp).await?;
            }
            [0x00, 0x00, 0x00, 0x02] => {
                trace!("Got USBIP_CMD_UNLINK");
                let seq_num = socket.read_u32().await?;
                let dev_id = socket.read_u32().await?;
                let direction = socket.read_u32().await?;
                let ep = socket.read_u32().await?;
                let _seq_num_submit = socket.read_u32().await?;
                // 24 bytes of struct padding
                let mut padding = [0u8; 6 * 4];
                socket.read_exact(&mut padding).await?;

                // USBIP_RET_UNLINK
                // command
                socket.write_u32(0x4).await?;
                socket.write_u32(seq_num).await?;
                socket.write_u32(dev_id).await?;
                socket.write_u32(direction).await?;
                socket.write_u32(ep).await?;
                // status
                socket.write_i32(-ECONNRESET).await?;
                socket.write_all(&mut padding).await?;
            }
            _ => warn!("Got unknown command {:?}", command),
        }
    }
}
