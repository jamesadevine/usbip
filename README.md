# usbip

A Rust library to run a USB/IP server to simulate USB devices.

[![Coverage Status](https://coveralls.io/repos/github/jiegec/usbip/badge.svg?branch=master)](https://coveralls.io/github/jiegec/usbip?branch=master)

## How to use

See examples directory. Two examples are provided:

1. hid_keyboard.rs: simulate a hid keyboard that types something every second.
2. cdc_acm_serial.rs : simulate a serial that gets a character every second.

To run example, run:

```bash
$ env RUST_LOG=info cargo run --example hid_keyboard
```

Then, in a USB/IP client environment:

```bash
$ usbip list -r $remote_ip
$ usbip attach -r $remote_ip -b $bus_id
```

Then, you can inspect the simulated USB device behavior in both sides.

## API

See code comments. Not finalized yet, so get prepared for api breaking changes.
