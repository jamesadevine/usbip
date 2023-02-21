use crate::server::UsbIpServer;
use crate::{SetupPacket, UsbIpCommand, UsbIpPacket};
use byteorder::ByteOrder;
use libc::ECONNRESET;
use log::*;
use std::io::{Cursor, ErrorKind, Result};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;
use tokio::time::sleep;

pub async fn reader<T: AsyncReadExt + Unpin>(
    socket: &mut T,
    server: Arc<UsbIpServer>,
    packet_queue: Arc<Mutex<Vec<UsbIpPacket>>>,
) -> Result<()> {
    loop {
        let mut command = [0u8; 4];
        if let Err(err) = socket.read_exact(&mut command).await {
            if err.kind() == ErrorKind::UnexpectedEof {
                info!("Remote closed the connection");
                return Ok(());
            } else {
                Err(err)?
            }
        }

        match command {
            [0x01, 0x11, 0x80, 0x05] => {
                trace!("Got OP_REQ_DEVLIST");
                let status = socket.read_u32().await?;
                packet_queue.lock().await.push(UsbIpPacket {
                    sequence_number: 0,
                    status: status,
                    command: command.to_vec(),
                    enum_command: UsbIpCommand::ReqDevlist,
                    data: vec![],
                });
            }
            [0x01, 0x11, 0x80, 0x03] => {
                trace!("Got OP_REQ_IMPORT");
                let status = socket.read_u32().await?;
                let mut data = [0u8; 32];
                socket.read_exact(&mut data).await?;
                packet_queue.lock().await.push(UsbIpPacket {
                    sequence_number: 0,
                    status: status,
                    command: command.to_vec(),
                    enum_command: UsbIpCommand::ReqImport,
                    data: data.to_vec(),
                });
            }
            [0x00, 0x00, 0x00, 0x01] => {
                trace!("Got CMD_SUBMIT");
                let seq_num = socket.read_u32().await?;
                let mut data = [0u8; 40];

                socket.read_exact(&mut data).await?;

                let mut cursor = Cursor::new(&data);
                let _dev_id = cursor.read_u32().await.unwrap_or_default();
                let _direction = cursor.read_u32().await.unwrap_or_default();
                let _ep = cursor.read_u32().await.unwrap_or_default();
                let _transfer_flags = cursor.read_u32().await.unwrap_or_default();
                let transfer_buffer_length = cursor.read_u32().await.unwrap_or_default();
                let _start_frame = cursor.read_u32().await.unwrap_or_default();
                let _number_of_packets = cursor.read_u32().await.unwrap_or_default();
                let _interval = cursor.read_u32().await.unwrap_or_default();

                // todo: might need to check direction here.
                // if direction is out (0), read additional data.
                let mut request_buffer = vec![0u8; transfer_buffer_length as usize];
                socket.read_exact(&mut request_buffer).await?;

                let mut data = data.to_vec();
                data.append(&mut request_buffer);

                packet_queue.lock().await.push(UsbIpPacket {
                    sequence_number: seq_num,
                    status: 0,
                    command: command.to_vec(),
                    enum_command: UsbIpCommand::CmdSubmit,
                    data: data,
                });
            }
            [0x00, 0x00, 0x00, 0x02] => {
                trace!("Got USBIP_CMD_UNLINK");
                let seq_num = socket.read_u32().await?;
                let mut data = [0u8; 16];
                socket.read_exact(&mut data).await?;
                packet_queue.lock().await.push(UsbIpPacket {
                    sequence_number: seq_num,
                    status: 0,
                    enum_command: UsbIpCommand::CmdUnlink,
                    command: command.to_vec(),
                    data: data.to_vec(),
                });

                // 24 bytes of struct padding
                let mut padding = [0u8; 6 * 4];
                socket.read_exact(&mut padding).await?;
            }
            _ => warn!("Got unknown command {:?}", command),
        }
    }
}

pub async fn writer<T: AsyncWriteExt + Unpin>(
    mut socket: &mut T,
    server: Arc<UsbIpServer>,
    packet_queue: Arc<Mutex<Vec<UsbIpPacket>>>,
) -> Result<()> {
    let mut current_import_device = None;
    let mut go_to_sleep = 0;
    loop {
        let mut queue = packet_queue.lock().await;

        // filter out any unsubmit requests.
        let mut usubmit_items = vec![];
        for packet in queue.iter_mut() {
            if packet.enum_command == UsbIpCommand::CmdUnlink {
                usubmit_items.push(packet.clone())
            }
        }

        for usubmit_pkt in usubmit_items {
            let mut cursor = Cursor::new(&usubmit_pkt.data);
            let dev_id = cursor.read_u32().await.unwrap_or_default();
            let direction = cursor.read_u32().await.unwrap_or_default();
            let ep = cursor.read_u32().await.unwrap_or_default();
            let seq_num_submit = cursor.read_u32().await.unwrap_or_default();

            let mut status_code = 0;

            // remove the packet indicated in the unsubmit request
            if let Some(position) = queue
                .iter_mut()
                .position(|pkt| pkt.sequence_number == seq_num_submit)
            {
                status_code = -ECONNRESET;
                queue.remove(position);
            }

            // remove the unsubmit packet
            if let Some(position) = queue
                .iter_mut()
                .position(|pkt| pkt.sequence_number == usubmit_pkt.sequence_number)
            {
                queue.remove(position);
            }

            // USBIP_CMD_UNLINK response
            socket.write_u32(0x4).await?;
            socket.write_u32(usubmit_pkt.sequence_number).await?;
            socket.write_u32(dev_id).await?;
            socket.write_u32(direction).await?;
            socket.write_u32(ep).await?;
            socket.write_i32(status_code).await?;

            let mut padding = [0u8; 6 * 4];
            socket.write_all(&mut padding).await?;
        }

        let mut current_pkt = None;
        if queue.len() > 0 {
            current_pkt = Some(queue.remove(0))
        } else {
            go_to_sleep += 1;
        }

        queue.remove(0);

        // explicitly drop the lock so reader can access the queue.
        drop(queue);

        if let Some(current_pkt) = current_pkt {
            go_to_sleep = 0;
            match current_pkt.enum_command {
                UsbIpCommand::ReqDevlist => {
                    // OP_REP_DEVLIST
                    trace!("Got OP_REQ_DEVLIST");
                    socket.write_u32(0x01110005).await?;
                    socket.write_u32(0).await?;
                    socket.write_u32(server.devices.len() as u32).await?;
                    for dev in &server.devices {
                        dev.write_dev_with_interfaces(&mut socket).await?;
                    }
                    trace!("Sent OP_REP_DEVLIST");
                }
                UsbIpCommand::ReqImport => {
                    trace!("Got OP_REQ_IMPORT");
                    let bus_id = &current_pkt.data;
                    assert_eq!(bus_id.len(), 32);
                    current_import_device = None;
                    for device in &server.devices {
                        let mut expected = device.bus_id.as_bytes().to_vec();
                        expected.resize(32, 0);
                        if &expected == bus_id {
                            current_import_device = Some(device);
                            info!("Found device {:?}", device.path);
                            break;
                        }
                    }

                    socket.write_u32(0x01110003).await?;
                    if let Some(dev) = current_import_device {
                        socket.write_u32(0).await?;
                        dev.write_dev(&mut socket).await?;
                    } else {
                        socket.write_u32(1).await?;
                    }
                    trace!("Sent OP_REP_IMPORT");
                }
                UsbIpCommand::CmdSubmit => {
                    let mut cursor = Cursor::new(&current_pkt.data);
                    let dev_id = cursor.read_u32().await.unwrap_or_default();
                    let direction = cursor.read_u32().await.unwrap_or_default();
                    let ep = cursor.read_u32().await.unwrap_or_default();
                    let _transfer_flags = cursor.read_u32().await.unwrap_or_default();
                    let transfer_buffer_length = cursor.read_u32().await.unwrap_or_default();
                    let _start_frame = cursor.read_u32().await.unwrap_or_default();
                    let _number_of_packets = cursor.read_u32().await.unwrap_or_default();
                    let _interval = cursor.read_u32().await.unwrap_or_default();
                    let mut setup = [0u8; 8];
                    cursor.read_exact(&mut setup).await?;

                    let setup_packet = SetupPacket::parse(&setup);

                    let mut request = vec![0u8; transfer_buffer_length as usize];
                    cursor.read_exact(&mut request).await?;

                    let device = current_import_device.unwrap();
                    let real_ep = if direction == 0 { ep } else { ep | 0x80 };
                    let (usb_ep, intf) = device.find_ep(real_ep as u8).unwrap();

                    let resp = device
                        .handle_urb(usb_ep, intf, setup_packet, request)
                        .await?;

                    if usb_ep.address != 0x85 {
                        trace!("Got USBIP_CMD_SUBMIT [{}]", current_pkt.sequence_number);
                        trace!("NUMBER OF PACKETS {_number_of_packets}");
                        trace!("->Endpoint {:02x?}", usb_ep);
                        trace!("->Setup {:02x?}", setup);
                        trace!("<-Resp {:02x?}", resp);
                    }

                    // USBIP_RET_SUBMIT
                    // command
                    socket.write_u32(0x3).await?;
                    socket.write_u32(current_pkt.sequence_number).await?;
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
                UsbIpCommand::CmdUnlink => panic!("Did not expect unlink in packet reader."),
            }
        }

        let duration = if go_to_sleep >= 10 {
            Duration::from_millis(1)
        } else {
            // Sleep a short while assuming intensive use
            Duration::from_micros(250)
        };

        sleep(duration).await;
    }
}
