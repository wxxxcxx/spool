use std::{
    env, fs,
    io::{Error, ErrorKind, Result, Write},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use tracing::{info, warn};

use crate::util::exe_path;

/// The bundle identifier for the `spool` service.
pub const ID: &str = "com.wxxxcxx.spool";

/// `Service` manages the installation, uninstallation, starting, and stopping of the `spool` application as a launchd service.
/// It encapsulates the `launchctl::Service` and the path to the executable.
#[derive(Debug)]
pub struct Service {
    /// The underlying `launchctl::Service` instance.
    pub raw: launchctl::Service,
    /// The absolute path to the `spool` executable.
    pub bin_path: PathBuf,
    /// The user's home directory.
    home_dir: PathBuf,
}

impl Service {
    /// Creates a new `Service` instance.
    /// It determines the executable path and constructs the `launchctl::Service` with appropriate settings.
    ///
    /// # Arguments
    ///
    /// * `name` - The name of the service (e.g., "com.wxxxcxx.spool").
    ///
    /// # Returns
    ///
    /// `Ok(Self)` if the service is created successfully, otherwise `Err(Error)` if the executable path or home directory cannot be found.
    pub fn try_new(name: &str) -> Result<Self> {
        let home_dir = env::home_dir().ok_or(Error::new(
            ErrorKind::NotFound,
            "Cannot find home directory.",
        ))?;
        Ok(Self {
            bin_path: exe_path().ok_or(Error::new(
                ErrorKind::NotFound,
                "Cannot find current executable path.",
            ))?,
            raw: launchctl::Service::builder()
                .name(name)
                .uid(unsafe { libc::getuid() }.to_string())
                .plist_path(format!(
                    "{home}/Library/LaunchAgents/{name}.plist",
                    home = home_dir.display()
                ))
                .build(),
            home_dir,
        })
    }

    /// Returns the path to the launchd plist file for this service.
    #[must_use]
    pub fn plist_path(&self) -> &Path {
        Path::new(&self.raw.plist_path)
    }

    /// Checks if the service is currently installed (i.e., its plist file exists).
    #[must_use]
    pub fn is_installed(&self) -> bool {
        self.plist_path().is_file()
    }

    /// Installs the service as a launch agent by writing its plist file.
    /// If the service is already installed, a warning is logged, and installation is skipped.
    ///
    /// # Returns
    ///
    /// `Ok(())` if the service is installed successfully or already exists, otherwise `Err(Error)` if a file system error occurs.
    pub fn install(&self) -> Result<()> {
        let plist_path = self.plist_path();
        let dir = plist_path.parent().ok_or(Error::last_os_error())?;
        if !dir.exists() {
            fs::create_dir_all(dir)?;
        }

        if self.is_installed() {
            warn!(
                "existing launch agent detected at `{}`, skipping installation",
                plist_path.display()
            );
            return Ok(());
        }

        let contents = self.launchd_plist()?;
        let mut plist = fs::File::create(plist_path)?;
        plist.write_all(contents.as_bytes())?;
        info!("installed launch agent to `{}`", plist_path.display());
        info!("check logfile /tmp/com.wxxxcxx.spool*.log for potential error messages");
        Ok(())
    }

    /// Uninstalls the service by removing its plist file.
    /// If the service is not installed, a warning is logged, and uninstallation is skipped.
    /// It also attempts to stop the service before removing the file.
    ///
    /// # Returns
    ///
    /// `Ok(())` if the service is uninstalled successfully or not found, otherwise `Err(Error)` if a file system error occurs.
    pub fn uninstall(&self) -> Result<()> {
        let plist_path = self.plist_path();
        if !self.is_installed() {
            warn!(
                "no launch agent detected at `{}`, skipping uninstallation",
                plist_path.display(),
            );
            return Ok(());
        }

        if let Err(e) = self.stop() {
            warn!("failed to stop service: {e:?}");
        }

        fs::remove_file(plist_path)?;
        info!(
            "removed existing launch agent at `{}`",
            plist_path.display()
        );
        Ok(())
    }

    /// Reinstalls the service by first uninstalling it and then installing it again.
    ///
    /// # Returns
    ///
    /// `Ok(())` if the service is reinstalled successfully, otherwise `Err(Error)` from underlying install/uninstall operations.
    pub fn reinstall(&self) -> Result<()> {
        self.uninstall()?;
        self.install()
    }

    /// Starts the service using `launchctl`.
    /// If the service is not installed, it will be installed first.
    ///
    /// # Returns
    ///
    /// `Ok(())` if the service starts successfully, otherwise `Err(Error)` from `launchctl`.
    pub fn start(&self) -> Result<()> {
        if !self.is_installed() {
            self.install()?;
        }
        info!("starting service...");
        self.create_log_files()?;
        for args in start_commands(&self.raw, self.is_bootstrapped()?) {
            Self::run_launchctl(&args)?;
        }
        info!("service started");
        Ok(())
    }

    fn create_log_files(&self) -> Result<()> {
        for path in [&self.raw.error_log_path, &self.raw.out_log_path] {
            if !Path::new(path).exists() {
                fs::File::create(path)?;
            }
        }
        Ok(())
    }

    fn is_bootstrapped(&self) -> Result<bool> {
        let output = Self::launchctl(&["print", self.raw.service_target.as_str()])?;
        Ok(output.status.success())
    }

    fn run_launchctl(args: &[&str]) -> Result<()> {
        let output = Self::launchctl(args)?;
        if output.status.success() {
            return Ok(());
        }

        Err(Error::other(format!(
            "launchctl {} failed with {}: {}",
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }

    fn launchctl(args: &[&str]) -> Result<Output> {
        Command::new("/bin/launchctl").args(args).output()
    }

    /// Stops the service using `launchctl`.
    ///
    /// # Returns
    ///
    /// `Ok(())` if the service stops successfully, otherwise `Err(Error)` from `launchctl`.
    pub fn stop(&self) -> Result<()> {
        info!("stopping service...");
        self.raw.stop()?;
        info!("service stopped");
        Ok(())
    }

    /// Restarts the service by first stopping it and then starting it again.
    ///
    /// # Returns
    ///
    /// `Ok(())` if the service restarts successfully, otherwise `Err(Error)` from underlying stop/start operations.
    pub fn restart(&self) -> Result<()> {
        self.stop()?;
        self.start()
    }

    /// Spawns a detached `spool restart` subprocess.
    /// Used by the in-daemon restart command so launchctl stop/start runs outside
    /// the process being stopped.
    pub fn request_restart() -> Result<()> {
        let bin_path = exe_path().ok_or(Error::new(
            ErrorKind::NotFound,
            "Cannot find current executable path.",
        ))?;
        Command::new(bin_path)
            .arg("restart")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        Ok(())
    }

    /// Serializes the launch agent with the system property-list serializer.
    ///
    /// # Errors
    /// Returns an error if serialization or the system converter fails.
    pub fn launchd_plist(&self) -> Result<String> {
        let bin_path = self.bin_path.to_str().ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidInput,
                "executable path is not valid UTF-8",
            )
        })?;
        let xdg_config_home = env::var("XDG_CONFIG_HOME")
            .unwrap_or_else(|_| format!("{}/.config", self.home_dir.display()));
        let rust_log = env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string());
        let document = serde_json::json!({
            "KeepAlive": { "Crashed": true, "SuccessfulExit": false },
            "Label": self.raw.name,
            "Nice": -20,
            "ProcessType": "Interactive",
            "Program": bin_path,
            "EnvironmentVariables": {
                "NO_COLOR": "1",
                "RUST_LOG": rust_log,
                "XDG_CONFIG_HOME": xdg_config_home,
            },
            "RunAtLoad": true,
            "StandardErrorPath": self.raw.error_log_path,
            "StandardOutPath": self.raw.out_log_path,
        });
        let json = serde_json::to_vec(&document).map_err(Error::other)?;
        let mut converter = Command::new("/usr/bin/plutil")
            .args(["-convert", "xml1", "-o", "-", "--", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let written = converter.stdin.take().map_or_else(
            || Err(Error::other("plist converter has no stdin")),
            |mut stdin| stdin.write_all(&json),
        );
        let output = converter.wait_with_output()?;
        written?;
        if !output.status.success() {
            return Err(Error::other(format!(
                "unable to serialize launch agent: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        String::from_utf8(output.stdout).map_err(Error::other)
    }
}

fn start_commands(service: &launchctl::Service, bootstrapped: bool) -> Vec<Vec<&str>> {
    if bootstrapped {
        vec![vec!["kickstart", service.service_target.as_str()]]
    } else {
        vec![
            vec!["enable", service.service_target.as_str()],
            vec![
                "bootstrap",
                service.domain_target.as_str(),
                service.plist_path.as_str(),
            ],
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service() -> launchctl::Service {
        launchctl::Service::builder()
            .name("com.wxxxcxx.spool")
            .uid("501")
            .plist_path("/Users/test/Library/LaunchAgents/com.wxxxcxx.spool.plist")
            .build()
    }

    #[test]
    fn start_kickstarts_a_bootstrapped_service_by_service_target() {
        assert_eq!(
            start_commands(&service(), true),
            vec![vec!["kickstart", "gui/501/com.wxxxcxx.spool"]]
        );
    }

    #[test]
    fn start_enables_and_bootstraps_an_unloaded_service() {
        assert_eq!(
            start_commands(&service(), false),
            vec![
                vec!["enable", "gui/501/com.wxxxcxx.spool"],
                vec![
                    "bootstrap",
                    "gui/501",
                    "/Users/test/Library/LaunchAgents/com.wxxxcxx.spool.plist"
                ]
            ]
        );
    }

    #[test]
    fn launch_agent_preserves_paths_with_xml_special_characters() {
        let service = Service {
            raw: service(),
            bin_path: PathBuf::from("/tmp/Spool & <work>/spool"),
            home_dir: PathBuf::from("/Users/test"),
        };
        let plist = service.launchd_plist().unwrap();
        let mut parser = Command::new("/usr/bin/plutil")
            .args(["-convert", "json", "-o", "-", "--", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        parser
            .stdin
            .take()
            .unwrap()
            .write_all(plist.as_bytes())
            .unwrap();
        let output = parser.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "generated launch agent must be a valid plist: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(parsed["Program"], "/tmp/Spool & <work>/spool");
        assert_eq!(parsed["KeepAlive"]["Crashed"], true);
        assert_eq!(parsed["KeepAlive"]["SuccessfulExit"], false);
        assert_eq!(parsed["RunAtLoad"], true);
        assert_eq!(parsed["Nice"], -20);
        assert!(parsed.get("MachServices").is_none());
    }

    #[test]
    fn launch_agent_rejects_unrepresentable_paths_without_panicking() {
        use std::os::unix::ffi::OsStringExt;

        let service = Service {
            raw: service(),
            bin_path: std::ffi::OsString::from_vec(b"/tmp/spool-\xff".to_vec()).into(),
            home_dir: PathBuf::from("/Users/test"),
        };
        assert_eq!(
            service.launchd_plist().unwrap_err().kind(),
            ErrorKind::InvalidInput
        );
    }
}
