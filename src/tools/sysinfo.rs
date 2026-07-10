//! Read-only system information gathered through a fixed set of macOS system
//! binaries with hardcoded arguments. Unlike `run_shell`, there is no
//! user-supplied command, no shell interpretation, and no untrusted input, so
//! this is always available and cannot be steered by prompt injection.

use std::process::Stdio;
use std::time::Duration;

use anyhow::{Result, bail};
use tokio::process::Command;
use tokio::time::timeout;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

/// Sections the model may request. `all` returns everything.
const SECTIONS: &[&str] = &["all", "os", "cpu", "memory", "disk"];

pub async fn report(section: &str) -> Result<String> {
    if !SECTIONS.contains(&section) {
        bail!(
            "unsupported section '{section}'; expected one of: {}",
            SECTIONS.join(", ")
        );
    }
    if !cfg!(target_os = "macos") {
        bail!("system_info requires macOS.");
    }

    let want = |name: &str| section == "all" || section == name;
    let mut out = String::new();

    if want("os") {
        let product = run("/usr/bin/sw_vers", &["-productName"]).await;
        let version = run("/usr/bin/sw_vers", &["-productVersion"]).await;
        let build = run("/usr/bin/sw_vers", &["-buildVersion"]).await;
        let arch = run("/usr/bin/uname", &["-m"]).await;
        let uptime = run("/usr/bin/uptime", &[]).await;
        out.push_str("[os]\n");
        out.push_str(&format!("name: {product} {version} ({build})\n"));
        out.push_str(&format!("architecture: {arch}\n"));
        out.push_str(&format!("uptime: {uptime}\n\n"));
    }

    if want("cpu") {
        let brand = run("/usr/sbin/sysctl", &["-n", "machdep.cpu.brand_string"]).await;
        let physical = run("/usr/sbin/sysctl", &["-n", "hw.physicalcpu"]).await;
        let logical = run("/usr/sbin/sysctl", &["-n", "hw.logicalcpu"]).await;
        out.push_str("[cpu]\n");
        out.push_str(&format!("model: {brand}\n"));
        out.push_str(&format!("physical_cores: {physical}\n"));
        out.push_str(&format!("logical_cores: {logical}\n\n"));
    }

    if want("memory") {
        let bytes_text = run("/usr/sbin/sysctl", &["-n", "hw.memsize"]).await;
        out.push_str("[memory]\n");
        match bytes_text.trim().parse::<u64>() {
            Ok(bytes) => out.push_str(&format!(
                "total: {} ({bytes} bytes)\n\n",
                human_bytes(bytes)
            )),
            Err(_) => out.push_str(&format!("total_raw: {bytes_text}\n\n")),
        }
    }

    if want("disk") {
        // `df -k /` reports the root APFS container in 1024-byte blocks.
        // Parsing the fixed columns avoids relying on locale-specific human
        // formatting.
        let df = run("/bin/df", &["-k", "/"]).await;
        out.push_str("[disk]\n");
        out.push_str(&format!("{}\n", format_disk(&df)));
    }

    Ok(out.trim_end().to_owned())
}

fn format_disk(df_output: &str) -> String {
    let Some(data) = df_output.lines().nth(1) else {
        return format!("raw: {df_output}");
    };
    let fields: Vec<&str> = data.split_whitespace().collect();
    // filesystem, 1024-blocks, used, available, capacity, ... mounted-on
    if fields.len() >= 5 {
        let kib = |value: &str| value.parse::<u64>().ok().map(|blocks| blocks * 1024);
        let total = kib(fields[1]);
        let available = kib(fields[3]);
        let mut lines = Vec::new();
        if let Some(total) = total {
            lines.push(format!("volume: / total: {}", human_bytes(total)));
        }
        // On macOS, `/` is a sealed APFS system volume. Its `df` Used column
        // only describes that volume (mostly the OS), while total and
        // available reflect the shared APFS container. Derive occupied space
        // from the container-wide values so Data, VM, snapshots, and the
        // other volumes are included.
        let used = total
            .zip(available)
            .map(|(total, available)| total.saturating_sub(available));
        if let Some(used) = used {
            lines.push(format!("used: {}", human_bytes(used)));
        }
        if let Some(available) = available {
            lines.push(format!("available: {}", human_bytes(available)));
        }
        if let (Some(used), Some(total)) = (used, total) {
            let percent = if total == 0 {
                0
            } else {
                ((u128::from(used) * 100).div_ceil(u128::from(total))) as u64
            };
            lines.push(format!("capacity: {percent}%"));
        } else {
            lines.push(format!("capacity: {}", fields[4]));
        }
        return lines.join("\n");
    }
    format!("raw: {df_output}")
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB", "PB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

async fn run(program: &str, args: &[&str]) -> String {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let output = match timeout(COMMAND_TIMEOUT, command.output()).await {
        Ok(Ok(output)) if output.status.success() => output,
        _ => return "unavailable".to_owned(),
    };
    let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if text.is_empty() {
        "unavailable".to_owned()
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_human_readable_bytes() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1024), "1.0 KB");
        assert_eq!(human_bytes(17_179_869_184), "16.0 GB");
    }

    #[test]
    fn parses_df_columns() {
        let df = "Filesystem 1024-blocks      Used Available Capacity  iused ifree %iused  Mounted on\n/dev/disk3s1s1 971350180 20000000 900000000    3%    500 4000000    0%   /";
        let formatted = format_disk(df);
        assert!(formatted.contains("volume: / total: 926.4 GB"));
        assert!(formatted.contains("used: 68.0 GB"));
        assert!(formatted.contains("available: 858.3 GB"));
        assert!(formatted.contains("capacity: 8%"));
    }

    #[test]
    fn derives_apfs_container_usage_instead_of_root_volume_usage() {
        let df = "Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/disk3s5s1 239362496 12150368 77294952 14% /";
        let formatted = format_disk(df);

        assert!(formatted.contains("used: 154.6 GB"));
        assert!(formatted.contains("capacity: 68%"));
        assert!(!formatted.contains("used: 11.6 GB"));
    }

    #[test]
    fn falls_back_when_df_is_unexpected() {
        assert!(format_disk("garbage").contains("raw:"));
    }

    #[tokio::test]
    async fn rejects_unknown_section() {
        assert!(report("network").await.is_err());
    }

    #[tokio::test]
    #[cfg(target_os = "macos")]
    async fn gathers_a_report_on_macos() {
        let report = report("all").await.unwrap();
        assert!(report.contains("[os]"));
        assert!(report.contains("[cpu]"));
        assert!(report.contains("[memory]"));
        assert!(report.contains("[disk]"));
    }
}
