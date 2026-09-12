//! Conservative registration ownership and recoverable artifact publication.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    io::{self, Write},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Owner {
    Local,
    Legacy,
    Managed,
    Unknown,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stamp {
    schema_version: u32,
    manager: String,
    registration_path: PathBuf,
    sha256: String,
}
#[link(name = "System")]
unsafe extern "C" {
    fn CC_SHA256(data: *const std::ffi::c_void, len: u32, digest: *mut u8) -> *mut u8;
}
fn hash(bytes: &[u8]) -> io::Result<String> {
    use std::fmt::Write as _;
    let len = u32::try_from(bytes.len()).map_err(io::Error::other)?;
    let mut digest = [0u8; 32];
    unsafe {
        CC_SHA256(bytes.as_ptr().cast(), len, digest.as_mut_ptr());
    }
    let mut output = String::with_capacity(64);
    for byte in digest {
        write!(&mut output, "{byte:02x}").map_err(io::Error::other)?;
    }
    Ok(output)
}
fn stamp_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".spool-owner.json");
    name.into()
}
fn safe_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| {
        metadata.is_file()
            && !metadata.file_type().is_symlink()
            && metadata.uid() == unsafe { libc::geteuid() }
    })
}
fn managed_path(path: &Path) -> bool {
    path.ancestors().any(|path| {
        fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink())
    })
}
pub(super) fn read(path: &Path) -> io::Result<(Vec<u8>, Value)> {
    let bytes = fs::read(path)?;
    let mut command = Command::new("/usr/bin/plutil")
        .args(["-convert", "json", "-o", "-", "--", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    command
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("plist input missing"))?
        .write_all(&bytes)?;
    let output = command.wait_with_output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "configuration_error: malformed registration {}",
            path.display()
        )));
    }
    Ok((bytes, serde_json::from_slice(&output.stdout)?))
}
fn legacy(value: &Value, label: &str) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    let keys = [
        "KeepAlive",
        "Label",
        "Nice",
        "ProcessType",
        "Program",
        "EnvironmentVariables",
        "RunAtLoad",
        "StandardErrorPath",
        "StandardOutPath",
    ];
    if object.len() != keys.len() || keys.iter().any(|key| !object.contains_key(*key)) {
        return false;
    }
    let absolute = |value: &Value| {
        value
            .as_str()
            .is_some_and(|value| Path::new(value).is_absolute())
    };
    value["Label"] == label
        && value["KeepAlive"] == json!({"Crashed":true,"SuccessfulExit":false})
        && value["Nice"] == -20
        && value["ProcessType"] == "Interactive"
        && value["RunAtLoad"] == true
        && absolute(&value["Program"])
        && absolute(&value["StandardErrorPath"])
        && absolute(&value["StandardOutPath"])
        && value["EnvironmentVariables"]
            .as_object()
            .is_some_and(|env| {
                env.len() == 3
                    && env.get("NO_COLOR") == Some(&json!("1"))
                    && env.get("RUST_LOG").is_some_and(Value::is_string)
                    && env.get("XDG_CONFIG_HOME").is_some_and(absolute)
            })
}
pub(super) fn classify(
    path: &Path,
    expected: &Path,
    label: &str,
    bytes: &[u8],
    document: &Value,
) -> Owner {
    if managed_path(path) {
        return Owner::Managed;
    }
    if !safe_file(path) {
        return Owner::Unknown;
    }
    let stamp = stamp_path(path);
    if fs::symlink_metadata(&stamp).is_ok() {
        if !safe_file(&stamp) || managed_path(&stamp) {
            return Owner::Unknown;
        }
        return fs::read(&stamp)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Stamp>(&bytes).ok())
            .filter(|stamp| {
                stamp.schema_version == 1
                    && stamp.manager == "spool-cli"
                    && stamp.registration_path == path
                    && stamp.registration_path.is_absolute()
                    && hash(bytes).is_ok_and(|hash| hash == stamp.sha256)
            })
            .map_or(Owner::Unknown, |_| Owner::Local);
    }
    if path == expected && legacy(document, label) {
        Owner::Legacy
    } else {
        Owner::Unknown
    }
}
pub(super) fn validate_arguments(document: &Value) -> io::Result<bool> {
    let program = document.get("Program").and_then(Value::as_str);
    let arguments = document.get("ProgramArguments");
    if arguments.is_none() && program.is_some_and(|program| Path::new(program).is_absolute()) {
        return Ok(false);
    }
    let Some(args) = arguments.and_then(Value::as_array) else {
        return Err(io::Error::other(
            "configuration_error: invalid ProgramArguments",
        ));
    };
    let valid = args.len() == 3
        && args[0]
            .as_str()
            .is_some_and(|arg| Path::new(arg).is_absolute())
        && args[1] == "service"
        && args[2] == "run"
        && program.is_none_or(|program| args[0] == program);
    if valid {
        Ok(true)
    } else {
        Err(io::Error::other(
            "configuration_error: expected executable, service, run",
        ))
    }
}
pub(super) fn refusal(path: &Path, owner: Owner, migration: bool) -> io::Error {
    let code = if migration {
        "migration_required"
    } else {
        "ownership_unknown"
    };
    let advice = match owner {
        Owner::Local | Owner::Legacy => {
            "run spool service stop, spool service reinstall, then spool service start"
        }
        Owner::Managed => {
            "update the package and service module, then activate through Nix/Home Manager or the owning manager"
        }
        Owner::Unknown => {
            "review and back up this registration; migrate or explicitly retire it through its known manager/manual administration before installing"
        }
    };
    io::Error::other(format!("{code}: {}: {advice}", path.display()))
}
fn write_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}
pub(super) fn publish(path: &Path, bytes: &[u8], previous: Option<&[u8]>) -> io::Result<()> {
    if !path.is_absolute() {
        return Err(io::Error::other("registration path must be absolute"));
    }
    let stamp = stamp_path(path);
    if previous.is_none() && fs::symlink_metadata(&stamp).is_ok() {
        return Err(refusal(path, Owner::Unknown, false));
    }
    let suffix = uuid::Uuid::new_v4();
    let staged = path.with_extension(format!("{suffix}.new"));
    let staged_stamp = path.with_extension(format!("{suffix}.owner-new"));
    let backup = path.with_extension(format!("{suffix}.backup"));
    let metadata = serde_json::to_vec(&Stamp {
        schema_version: 1,
        manager: "spool-cli".into(),
        registration_path: path.into(),
        sha256: hash(bytes)?,
    })?;
    let cleanup = scopeguard::guard((staged.clone(), staged_stamp.clone()), |(a, b)| {
        let _ = fs::remove_file(a);
        let _ = fs::remove_file(b);
    });
    write_new(&staged, bytes)?;
    write_new(&staged_stamp, &metadata)?;
    if let Some(previous) = previous {
        if !safe_file(path) || managed_path(path) || fs::read(path)? != previous {
            return Err(io::Error::other("registration changed during replacement"));
        }
        fs::rename(path, &backup)?;
    }
    // hard_link fails if a concurrent creator occupied the destination.
    let result = fs::hard_link(&staged, path).and_then(|()| fs::rename(&staged_stamp, &stamp));
    if let Err(error) = result {
        if safe_file(path) && fs::read(path).is_ok_and(|current| current == bytes) {
            let _ = fs::remove_file(path);
        }
        if previous.is_some() && fs::symlink_metadata(path).is_err() {
            fs::rename(&backup, path)?;
        }
        return Err(error);
    }
    if previous.is_some() {
        fs::remove_file(backup)?;
    }
    drop(cleanup);
    Ok(())
}
pub(super) fn remove_stamp(path: &Path) -> io::Result<()> {
    match fs::remove_file(stamp_path(path)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn frozen_legacy_requires_complete_generator_shape() {
        let mut v = json!({"KeepAlive":{"Crashed":true,"SuccessfulExit":false},"Label":"spool","Nice":-20,"ProcessType":"Interactive","Program":"/nix/store/package/bin/spool","EnvironmentVariables":{"NO_COLOR":"1","RUST_LOG":"info","XDG_CONFIG_HOME":"/Users/test/.config"},"RunAtLoad":true,"StandardErrorPath":"/tmp/spool.err.log","StandardOutPath":"/tmp/spool.log"});
        assert!(legacy(&v, "spool"));
        v["EnvironmentVariables"]
            .as_object_mut()
            .unwrap()
            .remove("RUST_LOG");
        assert!(!legacy(&v, "spool"));
        v["EnvironmentVariables"] = json!({"NO_COLOR":"1","SPOOL_LUA":"/nix/store/init.lua"});
        assert!(!legacy(&v, "spool"));
    }
    #[test]
    fn sidecar_binding_does_not_authorize_changed_bytes_or_symlinks() {
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(uuid::Uuid::new_v4().to_string());
        fs::create_dir(&root).unwrap();
        let path = root.join("agent.plist");
        publish(&path, b"original", None).unwrap();
        assert_eq!(
            classify(&path, &path, "spool", b"original", &Value::Null),
            Owner::Local
        );
        fs::write(&path, b"changed").unwrap();
        assert_eq!(
            classify(&path, &path, "spool", b"changed", &Value::Null),
            Owner::Unknown
        );
        let link = root.join("linked.plist");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert_eq!(
            classify(&link, &link, "spool", b"changed", &Value::Null),
            Owner::Managed
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn argument_validation_separates_old_compatible_and_malformed() {
        assert!(!validate_arguments(&json!({"Program":"/bin/spool"})).unwrap());
        assert!(
            validate_arguments(&json!({"ProgramArguments":["/bin/spool","service","run"]}))
                .unwrap()
        );
        assert!(
            validate_arguments(
                &json!({"Program":"/bin/spool","ProgramArguments":["/bin/other","service","run"]})
            )
            .is_err()
        );
        assert!(validate_arguments(&json!({"ProgramArguments":["/bin/spool","launch"]})).is_err());
    }
}
