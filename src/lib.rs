//! A crate to simplify establishing and keeping a Wifi connection on ESP32.
//! ```ignore
//!    use wifi_multi::WifiMulti;
//!
//!    let mut wfm = WifiMulti::new();
//!    wfm.add("ssid1", "psk1")?;
//!    wfm.add("ssid2", "psk2")?;
//!    wfm.run()?;
//! ```
//!
//! This will spawn a thread that searches for the given SSIDs,
//! selects the strongest one, and tries to stay connected (to any of
//! the SSIDs) forever.

use embedded_svc::wifi::*;
use esp_idf_hal::peripherals::Peripherals;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::wifi::EspWifi;
use std::net::Ipv4Addr;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const WIFI_TIMEOUT: u32 = 15;

/// Errors and states
#[derive(Debug, Clone, Copy, thiserror::Error, PartialEq)]
pub enum Error {
    #[error("Stopped")]
    Stopped,
    #[error("Searching")]
    Searching,
    #[error("Disconnected")]
    Disconnected,
    #[error("ConnectTimeout")]
    ConnectTimeout,
    #[error("No SSID list")]
    NoSSIDList, // forgot to use add()?
    #[error("No SSID found")]
    NoSSIDFound, // none of the SSIDs were found in scan results
    #[error("Already running")]
    AlreadyRunning,
    #[error("Scan error")]
    ScanError,
}

#[derive(PartialEq, Clone)]
struct SSIDEntry {
    ssid: String,
    psk: String,
}

pub struct WifiMulti<'a> {
    ssid_list: Arc<Mutex<Vec<SSIDEntry>>>,
    state: Arc<Mutex<Result<(), Error>>>,
    wifi_thread_handle: Option<thread::JoinHandle<()>>,
    ssid: Arc<Mutex<String>>,
    wifi: Arc<Mutex<EspWifi<'a>>>,
    stop: Arc<AtomicBool>,
}

impl WifiMulti<'static> {
    /// Creates an empty WifiMulti object.
    pub fn new() -> Self {
        // shared, needs to be created here or wifi_thread will panic
        let peripherals = Peripherals::take().unwrap();
        let sysloop = EspSystemEventLoop::take().unwrap();
        let wifi = Arc::new(Mutex::new(
            EspWifi::new(peripherals.modem, sysloop, None).unwrap(),
        ));

        Self {
            ssid_list: Arc::new(Mutex::new(vec![])),
            state: Arc::new(Mutex::new(Err(Error::Stopped))),
            wifi_thread_handle: None,
            ssid: Arc::new(Mutex::new("".to_string())),
            wifi,
            stop: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Add an SSID with its pre-shared key.
    ///
    /// The number you can add is not limited.
    ///
    /// Cannot be called after `run()` has been called.
    pub fn add(&mut self, ssid: &str, psk: &str) -> Result<(), Error> {
        if *self.state.lock().unwrap() != Err(Error::Stopped) {
            return Err(Error::AlreadyRunning);
        }

        self.ssid_list.lock().unwrap().push(SSIDEntry {
            ssid: ssid.to_string(),
            psk: psk.to_string(),
        });
        Ok(())
    }

    /// Starts the Wifi thread and returns immediately.
    pub fn run(&mut self) -> Result<(), Error> {
        if self.ssid_list.lock().unwrap().is_empty() {
            return Err(Error::NoSSIDList);
        }
        if *self.state.lock().unwrap() != Err(Error::Stopped) {
            return Err(Error::AlreadyRunning);
        }
        self.stop.store(false, Ordering::SeqCst);

        let builder = thread::Builder::new()
            .name("WifiMulti".into())
            .stack_size(10 * 1024);
        let state = Arc::clone(&self.state);
        let ssid = Arc::clone(&self.ssid);
        let ssidlist = Arc::clone(&self.ssid_list);
        let wifi = Arc::clone(&self.wifi);
        let stop = Arc::clone(&self.stop);
        self.wifi_thread_handle = Some(
            builder
                .spawn(move || _run(state, ssid, wifi, ssidlist, stop))
                .unwrap(),
        );
        Ok(())
    }

    /// Like `run()`, but waits for a connection to be established or
    /// the timeout (in seconds) to expire. Returns either `Ok(())` on
    /// success or `Err()` with the latest `state()` on timeout.
    pub fn run_wait(&mut self, timeout: Duration) -> Result<(), Error> {
        let start = Instant::now();
        self.run()?;
        loop {
            match self.state() {
                Ok(()) => return Ok(()),
                Err(e) => {
                    if start.elapsed() > timeout {
                        return Err(e);
                    } else {
                        thread::sleep(Duration::from_secs(1));
                    }
                }
            }
        }
    }

    /// Disconnect Wifi, stop the thread.
    ///
    /// Returns immediately, check `state()` for `Err(Stopped)`.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    /// Returns the SSID that we're connected to.
    pub fn connected_ssid(&self) -> Result<String, Error> {
        // return connected SSID, if any
        let s: &Result<(), Error> = &self.state.lock().unwrap();
        match s {
            Ok(()) => Ok((*self.ssid.lock().unwrap()).clone()),
            Err(e) => Err(*e),
        }
    }

    /// Returns the state of the connection -- either `Ok(())` when
    /// connected or one of the `Error`s.
    pub fn state(&self) -> Result<(), Error> {
        *self.state.lock().unwrap()
    }
}

impl Default for WifiMulti<'static> {
    fn default() -> Self {
        Self::new()
    }
}

fn _run(
    state: Arc<Mutex<Result<(), Error>>>,
    ssid: Arc<Mutex<String>>,
    wifi: Arc<Mutex<EspWifi>>,
    ssid_list: Arc<Mutex<Vec<SSIDEntry>>>,
    stop: Arc<AtomicBool>,
) {
    loop {
        // scan APs
        let scanres;
        {
            let mut wifi = wifi.lock().unwrap();
            scanres = wifi.scan();
        }
        let aps = match scanres {
            Err(_) => {
                *state.lock().unwrap() = Err(Error::ScanError);
                continue;
            }
            Ok(i) => i,
        };
        let mut best_ssid: Option<SSIDEntry> = None;
        {
            let mut best_signal = -127i8;
            let list = ssid_list.lock().unwrap();
            for ap in aps {
                for cand in &*list {
                    if ap.ssid.as_str().ne(&cand.ssid) {
                        continue;
                    };
                    if ap.signal_strength > best_signal {
                        best_signal = ap.signal_strength;
                        best_ssid = Some(cand.clone());
                    }
                }
            }
            if best_ssid.eq(&None) {
                *state.lock().unwrap() = Err(Error::NoSSIDFound);
                continue;
            }
        }

        // now we got a candidate
        let wifi_cfg = Configuration::Client(ClientConfiguration {
            ssid: best_ssid.as_ref().unwrap().ssid.as_str().into(),
            password: best_ssid.as_ref().unwrap().psk.as_str().into(),
            ..Default::default() // WPA2WPA3Personal doesn't work with WPA2-only AP
                                 //        auth_method: AuthMethod::WPA2WPA3Personal,
                                 //        bssid: None,
                                 //        channel: None,
        });

        {
            let mut wifi = wifi.lock().unwrap();
            wifi.set_configuration(&wifi_cfg).unwrap();

            //info!("wifi_thread: connecting to {}", CONFIG.wifi_ssid);
            wifi.start().unwrap();
            wifi.connect().unwrap();
        }

        let mut fail_count = 0;
        // loop and check; restart wifi on consecutive failures
        loop {
            thread::sleep(Duration::from_secs(1));
            if stop.load(Ordering::SeqCst) {
                break;
            }
            {
                let wifi = wifi.lock().unwrap();

                if wifi.is_connected().unwrap()
                    && wifi.sta_netif().get_ip_info().unwrap().ip != Ipv4Addr::UNSPECIFIED
                {
                    fail_count = 0;
                    *state.lock().unwrap() = Ok(());
                    *ssid.lock().unwrap() = best_ssid.as_ref().unwrap().ssid.as_str().into();
                    continue;
                }
            }
            *state.lock().unwrap() = Err(Error::Disconnected);
            fail_count += 1;
            //info!("wifi_thread: fail_count now {}", fail_count);
            if fail_count > WIFI_TIMEOUT {
                break;
            }
        }
        *state.lock().unwrap() = Err(Error::ConnectTimeout);
        {
            let mut wifi = wifi.lock().unwrap();
            wifi.disconnect().unwrap();
            wifi.stop().unwrap();
        }
        if stop.load(Ordering::SeqCst) {
            *state.lock().unwrap() = Err(Error::Stopped);
            return;
        }
        thread::sleep(Duration::from_secs(1));
    }
}
