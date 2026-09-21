//! Finds drives that could become bootable sticks. Each OS reports drives
//! differently, so each has its own thin command wrapper feeding a pure
//! parser (unit-tested against sample output) — and every parser marks the
//! system disk so [`Device::is_safe_target`] can refuse it.

use serde_json::Value;

use super::Device;
use crate::{EngineError, EngineResult};

/// Every drive we can see, including ones that aren't safe targets. Callers
/// showing choices to a user should filter with [`Device::is_safe_target`].
pub fn list_devices() -> EngineResult<Vec<Device>> {
    #[cfg(target_os = "macos")]
    {
        macos::list()
    }
    #[cfg(target_os = "linux")]
    {
        linux::list()
    }
    #[cfg(windows)]
    {
        windows::list()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        Err(EngineError::Other("bootable USB creation isn't supported on this platform".into()))
    }
}

pub fn list_safe_targets() -> EngineResult<Vec<Device>> {
    Ok(list_devices()?.into_iter().filter(Device::is_safe_target).collect())
}

fn json_bool(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::String(s) => s == "1" || s.eq_ignore_ascii_case("true"),
        Value::Number(n) => n.as_u64() == Some(1),
        _ => false,
    }
}

fn json_u64(v: &Value) -> u64 {
    match v {
        Value::Number(n) => n.as_u64().unwrap_or(0),
        Value::String(s) => s.parse().unwrap_or(0),
        _ => 0,
    }
}

fn run_capture(cmd: &mut std::process::Command) -> EngineResult<String> {
    let out = cmd
        .output()
        .map_err(|e| EngineError::Other(format!("couldn't run {:?}: {e}", cmd.get_program())))?;
    if !out.status.success() {
        return Err(EngineError::Other(format!(
            "{:?} failed: {}",
            cmd.get_program(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

// ---------------------------------------------------------------- macOS

#[allow(dead_code)] // only called on macos; parsers are tested everywhere
pub(crate) mod macos {
    use super::*;
    use std::io::Write;
    use std::process::{Command, Stdio};

    /// Converts a plist (as printed by `diskutil ... -plist`) to JSON via the
    /// system `plutil`, avoiding a plist-parsing dependency.
    pub(crate) fn plist_to_json(plist: &str) -> EngineResult<Value> {
        let mut child = Command::new("plutil")
            .args(["-convert", "json", "-o", "-", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| EngineError::Other(format!("couldn't run plutil: {e}")))?;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(plist.as_bytes())
            .map_err(|e| EngineError::Other(e.to_string()))?;
        let out = child.wait_with_output().map_err(|e| EngineError::Other(e.to_string()))?;
        serde_json::from_slice(&out.stdout).map_err(|e| EngineError::Other(format!("bad plutil output: {e}")))
    }

    pub(crate) fn diskutil_json(args: &[&str]) -> EngineResult<Value> {
        let text = run_capture(Command::new("diskutil").args(args))?;
        plist_to_json(&text)
    }

    pub(crate) fn list() -> EngineResult<Vec<Device>> {
        let listing = diskutil_json(&["list", "-plist"])?;
        let boot_disk = diskutil_json(&["info", "-plist", "/"])
            .ok()
            .and_then(|v| v["ParentWholeDisk"].as_str().map(str::to_owned));
        let mut devices = Vec::new();
        for id in listing["WholeDisks"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            let Ok(info) = diskutil_json(&["info", "-plist", id]) else { continue };
            let mounted = mount_points_of(&listing, id);
            if let Some(d) = parse_info(&info, mounted, boot_disk.as_deref()) {
                devices.push(d);
            }
        }
        Ok(devices)
    }

    fn mount_points_of(listing: &Value, whole: &str) -> Vec<String> {
        let mut out = Vec::new();
        for disk in listing["AllDisksAndPartitions"].as_array().into_iter().flatten() {
            if disk["DeviceIdentifier"].as_str() != Some(whole) {
                continue;
            }
            for part in disk["Partitions"].as_array().into_iter().flatten() {
                if let Some(m) = part["MountPoint"].as_str() {
                    out.push(m.to_owned());
                }
            }
            if let Some(m) = disk["MountPoint"].as_str() {
                out.push(m.to_owned());
            }
        }
        out
    }

    /// Turns one `diskutil info -plist` result (already JSON) into a
    /// [`Device`]. Only removable/external whole disks are returned; the
    /// disk holding `/` is kept but flagged so it can never be a target.
    pub(crate) fn parse_info(info: &Value, mounted: Vec<String>, boot_disk: Option<&str>) -> Option<Device> {
        if !json_bool(&info["WholeDisk"]) {
            return None;
        }
        let internal = json_bool(&info["Internal"]);
        let removable = json_bool(&info["RemovableMedia"]) || json_bool(&info["RemovableMediaOrExternalDevice"]);
        if internal && !removable {
            return None;
        }
        let id = info["DeviceIdentifier"].as_str()?.to_owned();
        let is_virtual = info["VirtualOrPhysical"].as_str() == Some("Virtual")
            || info["BusProtocol"].as_str() == Some("Disk Image");
        let is_system = boot_disk == Some(id.as_str()) || json_bool(&info["SystemImage"]);
        Some(Device {
            node: format!("/dev/r{id}"),
            name: info["MediaName"].as_str().unwrap_or("Unknown drive").trim().to_owned(),
            size: json_u64(&info["Size"]),
            bus: info["BusProtocol"].as_str().unwrap_or("").to_owned(),
            mounted,
            is_system,
            is_virtual,
            id,
        })
    }
}

// ---------------------------------------------------------------- Linux

#[allow(dead_code)] // only called on linux; parsers are tested everywhere
pub(crate) mod linux {
    use super::*;
    use std::process::Command;

    pub(crate) fn list() -> EngineResult<Vec<Device>> {
        let text = run_capture(Command::new("lsblk").args([
            "-J", "-b", "-o", "NAME,PATH,SIZE,RM,HOTPLUG,TRAN,TYPE,MODEL,VENDOR,MOUNTPOINT",
        ]))?;
        parse_lsblk(&text)
    }

    const CRITICAL_MOUNTS: &[&str] = &["/", "/boot", "/boot/efi", "/home", "/usr", "/var", "/etc", "/opt", "/nix", "[SWAP]"];

    fn collect_mounts(node: &Value, out: &mut Vec<String>) {
        if let Some(m) = node["mountpoint"].as_str() {
            out.push(m.to_owned());
        }
        for m in node["mountpoints"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            out.push(m.to_owned());
        }
        for child in node["children"].as_array().into_iter().flatten() {
            collect_mounts(child, out);
        }
    }

    pub(crate) fn parse_lsblk(json: &str) -> EngineResult<Vec<Device>> {
        let root: Value = serde_json::from_str(json).map_err(|e| EngineError::Other(format!("bad lsblk output: {e}")))?;
        let mut devices = Vec::new();
        for d in root["blockdevices"].as_array().into_iter().flatten() {
            if d["type"].as_str() != Some("disk") {
                continue;
            }
            let bus = d["tran"].as_str().unwrap_or("").to_owned();
            let removable = json_bool(&d["rm"]) || json_bool(&d["hotplug"]) || bus == "usb";
            if !removable {
                continue;
            }
            let Some(name) = d["name"].as_str() else { continue };
            let mut mounts = Vec::new();
            collect_mounts(d, &mut mounts);
            let is_system = mounts.iter().any(|m| CRITICAL_MOUNTS.contains(&m.as_str()));
            let label = format!(
                "{} {}",
                d["vendor"].as_str().unwrap_or("").trim(),
                d["model"].as_str().unwrap_or("").trim()
            );
            devices.push(Device {
                id: name.to_owned(),
                node: d["path"].as_str().map(str::to_owned).unwrap_or_else(|| format!("/dev/{name}")),
                name: if label.trim().is_empty() { "Unknown drive".into() } else { label.trim().to_owned() },
                size: json_u64(&d["size"]),
                bus,
                mounted: mounts.into_iter().filter(|m| m != "[SWAP]").collect(),
                is_system,
                is_virtual: false,
            });
        }
        Ok(devices)
    }
}

// -------------------------------------------------------------- Windows

#[allow(dead_code)] // only called on windows; parsers are tested everywhere
pub(crate) mod windows {
    use super::*;
    use std::process::Command;

    pub(crate) fn list() -> EngineResult<Vec<Device>> {
        let mut cmd = Command::new("powershell");
        cmd.args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Get-Disk | Select-Object Number,FriendlyName,Size,BusType,IsBoot,IsSystem | ConvertTo-Json -Compress",
        ]);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        parse_get_disk(&run_capture(&mut cmd)?)
    }

    pub(crate) fn parse_get_disk(json: &str) -> EngineResult<Vec<Device>> {
        let trimmed = json.trim();
        if trimmed.is_empty() {
            return Ok(Vec::new());
        }
        let root: Value = serde_json::from_str(trimmed).map_err(|e| EngineError::Other(format!("bad Get-Disk output: {e}")))?;
        // ConvertTo-Json emits a bare object, not an array, for one disk.
        let items: Vec<Value> = match root {
            Value::Array(a) => a,
            other => vec![other],
        };
        let mut devices = Vec::new();
        for d in items {
            let bus = d["BusType"].as_str().map(str::to_owned).or_else(|| d["BusType"].as_u64().map(|n| n.to_string())).unwrap_or_default();
            // Only USB / SD / MMC bus types: never SATA, NVMe, SCSI, RAID...
            if !matches!(bus.as_str(), "USB" | "SD" | "MMC" | "7" | "12" | "13") {
                continue;
            }
            let Some(number) = d["Number"].as_u64() else { continue };
            devices.push(Device {
                id: format!("Disk {number}"),
                node: format!(r"\\.\PhysicalDrive{number}"),
                name: d["FriendlyName"].as_str().unwrap_or("Unknown drive").trim().to_owned(),
                size: json_u64(&d["Size"]),
                bus,
                mounted: Vec::new(),
                is_system: json_bool(&d["IsBoot"]) || json_bool(&d["IsSystem"]),
                is_virtual: false,
            });
        }
        Ok(devices)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lsblk_only_offers_removable_disks_and_flags_system_ones() {
        let json = r#"{"blockdevices":[
          {"name":"nvme0n1","path":"/dev/nvme0n1","size":512110190592,"rm":false,"hotplug":false,"tran":"nvme","type":"disk","model":"Samsung","vendor":null,"mountpoint":null,
           "children":[{"name":"nvme0n1p2","type":"part","mountpoint":"/"}]},
          {"name":"sdb","path":"/dev/sdb","size":16008609792,"rm":true,"hotplug":true,"tran":"usb","type":"disk","model":"Ultra Fit","vendor":"SanDisk ","mountpoint":null,
           "children":[{"name":"sdb1","type":"part","mountpoint":"/media/me/USB"}]},
          {"name":"sdc","path":"/dev/sdc","size":8000000000,"rm":"1","hotplug":"1","tran":"usb","type":"disk","model":"Boot stick","vendor":"","mountpoint":null,
           "children":[{"name":"sdc1","type":"part","mountpoint":"/"}]},
          {"name":"loop0","path":"/dev/loop0","size":1000,"rm":false,"type":"loop","mountpoint":"/snap/core"}
        ]}"#;
        let devs = linux::parse_lsblk(json).unwrap();
        assert_eq!(devs.len(), 2, "internal NVMe and loop devices must not be listed");
        let sdb = devs.iter().find(|d| d.id == "sdb").unwrap();
        assert!(sdb.is_safe_target());
        assert_eq!(sdb.name, "SanDisk Ultra Fit");
        assert_eq!(sdb.mounted, vec!["/media/me/USB"]);
        let sdc = devs.iter().find(|d| d.id == "sdc").unwrap();
        assert!(sdc.is_system && !sdc.is_safe_target(), "a USB disk holding / must be refused");
    }

    #[test]
    fn get_disk_only_offers_usb_and_refuses_boot_disks() {
        let one = r#"{"Number":1,"FriendlyName":"SanDisk Ultra","Size":16008609792,"BusType":"USB","IsBoot":false,"IsSystem":false}"#;
        let devs = windows::parse_get_disk(one).unwrap();
        assert_eq!(devs.len(), 1);
        assert_eq!(devs[0].node, r"\\.\PhysicalDrive1");
        assert!(devs[0].is_safe_target());

        let many = r#"[{"Number":0,"FriendlyName":"NVMe","Size":512110190592,"BusType":"NVMe","IsBoot":true,"IsSystem":true},
                       {"Number":2,"FriendlyName":"USB boot","Size":8000000000,"BusType":"USB","IsBoot":true,"IsSystem":false}]"#;
        let devs = windows::parse_get_disk(many).unwrap();
        assert_eq!(devs.len(), 1, "non-USB bus types are never listed");
        assert!(!devs[0].is_safe_target(), "a USB disk Windows booted from must be refused");
    }

    #[test]
    fn macos_info_flags_boot_disk_and_virtual_images() {
        let ext: Value = serde_json::from_str(
            r#"{"WholeDisk":true,"Internal":false,"RemovableMedia":true,"DeviceIdentifier":"disk4","MediaName":"Cruzer","Size":16008609792,"BusProtocol":"USB","VirtualOrPhysical":"Physical"}"#,
        ).unwrap();
        let d = macos::parse_info(&ext, vec![], Some("disk3")).unwrap();
        assert_eq!(d.node, "/dev/rdisk4");
        assert!(d.is_safe_target());

        let boot: Value = serde_json::from_str(
            r#"{"WholeDisk":true,"Internal":false,"RemovableMedia":true,"DeviceIdentifier":"disk4","Size":1000,"BusProtocol":"USB"}"#,
        ).unwrap();
        assert!(!macos::parse_info(&boot, vec![], Some("disk4")).unwrap().is_safe_target());

        let internal: Value = serde_json::from_str(
            r#"{"WholeDisk":true,"Internal":true,"RemovableMedia":false,"DeviceIdentifier":"disk0","Size":500000000000}"#,
        ).unwrap();
        assert!(macos::parse_info(&internal, vec![], None).is_none(), "internal fixed disks are never listed");

        let image: Value = serde_json::from_str(
            r#"{"WholeDisk":true,"Internal":false,"RemovableMedia":true,"DeviceIdentifier":"disk8","Size":1000,"BusProtocol":"Disk Image","VirtualOrPhysical":"Virtual"}"#,
        ).unwrap();
        let img = macos::parse_info(&image, vec![], None).unwrap();
        assert!(img.is_virtual);
    }
}

#[cfg(test)]
mod live {
    use super::*;

    /// Prints what this machine's discovery sees. Not part of the normal
    /// run (needs real hardware state): `cargo test -p engine live -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn print_devices_on_this_machine() {
        for d in list_devices().unwrap() {
            println!("{d:?}  -> safe target: {}", d.is_safe_target());
        }
    }
}
