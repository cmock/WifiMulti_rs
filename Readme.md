# WifiMulti for Rust on ESP32

This crate is a simple Wifi connection manager for Rust projects on
the ESP32 architecture, inspired by the Arduino library of the same
name.

## In a nutshell

```rust
use wifi_multi::WifiMulti;

fn main() {
    let mut wfm = WifiMulti::new();
    wfm.add("ssid1", "psk1")?;
    wfm.add("ssid2", "psk2")?;
    wfm.run()?;

    let s = wfm.state();
    println!("WifiMulti state {:?}", s);
    if s.is_ok() {
        info!("Connected to {:?}", wfm.connected_ssid());
    }
}
```

## Details

The code aims to be fire-and-forget. Add your SSID data, call `run()`,
that's it.

`run()` fires off a thread that scans for APs, selects the one with the
strongest signal, connects, monitors the connection and tries to
reconnect forever.

You can add as many SSIDs as you like (they're kept in a `Vec`), but
only before you call `run()`.

Note that `run()` returns immediately, before a connection is
established. If you want to wait, use `run_wait(timeout)`.

`state()` returns a `Result`, `Ok(())` when there is a connection and
`Err(WifiMulti::Error)` otherwise.

## Shortcomings

There are hardcoded timeouts.

It always tries to connect to the SSID with the strongest signal; if
that fails (e.g. because the PSK is wrong), it will try over and over
and never look at other configured SSIDs.

I'm very new to Rust.
