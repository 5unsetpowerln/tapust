//! Tests that need a display run in a headless sway: the test starts one
//! and runs a test of its own binary in it, as GTK is set up once per
//! process from the environment. `TAPUST_TEST_SWAY` names the sway to run
//! (`sway` by default).

use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use gtk::glib;

/// Runs the ignored test `name` of this binary in a new headless sway.
pub fn run(name: &str) {
    let exe = std::env::current_exe().expect("the test binary");
    // target/<profile>/deps/<binary>: the files go under target/tmp.
    let target = exe.ancestors().nth(3).expect("the target directory");
    let dir = Dir(target.join("tmp").join(format!("sway-{}", std::process::id())));
    let dir = &dir.0;
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir.join("config")).expect("the sway directory");
    let sway_config = dir.join("sway.conf");
    std::fs::write(&sway_config, "output HEADLESS-1 resolution 1280x800\n").expect("the sway configuration");
    let program = std::env::var_os("TAPUST_TEST_SWAY").unwrap_or_else(|| "sway".into());
    let mut sway = Sway(
        Command::new(&program)
            .arg("-c")
            .arg(&sway_config)
            .env("XDG_RUNTIME_DIR", dir)
            .env("WLR_BACKENDS", "headless")
            .env("WLR_LIBINPUT_NO_DEVICES", "1")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("DISPLAY")
            .spawn()
            .unwrap_or_else(|err| panic!("cannot run {program:?} ({err}); set TAPUST_TEST_SWAY to a sway")),
    );
    let socket = dir.join("wayland-1");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !socket.exists() {
        if let Some(status) = sway.0.try_wait().expect("sway's status") {
            panic!("{program:?} exited ({status}) before it made a Wayland socket; set TAPUST_TEST_SWAY to a sway");
        }
        assert!(Instant::now() < deadline, "{program:?} made no Wayland socket in 10 s");
        std::thread::sleep(Duration::from_millis(20));
    }
    let status = Command::new(&exe)
        .args(["--exact", name, "--ignored", "--nocapture", "--test-threads=1"])
        .env("XDG_RUNTIME_DIR", dir)
        .env("WAYLAND_DISPLAY", "wayland-1")
        .env("GDK_BACKEND", "wayland")
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("GSETTINGS_BACKEND", "memory")
        .env("GTK_A11Y", "none")
        .env_remove("DISPLAY")
        .env_remove("DBUS_SESSION_BUS_ADDRESS")
        .status()
        .expect("the test binary in sway");
    drop(sway);
    assert!(status.success(), "{name} failed in the headless sway ({status})");
}

/// Removes the directory it holds when dropped, also when the test fails.
struct Dir(PathBuf);

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Stops the sway it holds when dropped, also when the test fails.
struct Sway(Child);

impl Drop for Sway {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Runs GTK's main loop until `done`, failing after 5 s.
pub fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    // A timer keeps the loop turning while nothing else happens.
    let tick = glib::timeout_add_local(Duration::from_millis(10), || glib::ControlFlow::Continue);
    let deadline = Instant::now() + Duration::from_secs(5);
    let context = glib::MainContext::default();
    while !done() {
        assert!(Instant::now() < deadline, "no {what} in 5 s");
        context.iteration(true);
    }
    tick.remove();
}

/// Runs GTK's main loop until `count` has not changed for 300 ms.
pub fn settle(count: impl Fn() -> u32) {
    let (mut last, mut since) = (count(), Instant::now());
    wait_until("a quiet 300 ms", || {
        if count() != last {
            (last, since) = (count(), Instant::now());
        }
        since.elapsed() >= Duration::from_millis(300)
    });
}
