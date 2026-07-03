use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use reqwest::header::{CONTENT_LENGTH, LOCATION};
use reqwest::{Client, StatusCode, Url};
use tokio::io::AsyncWriteExt;

const MAX_DOWNLOAD_BYTES: u64 = 25 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;

pub async fn download_url(raw_url: &str, destination: &Path, overwrite: bool) -> Result<String> {
    if destination.exists() && !overwrite {
        bail!(
            "destination already exists and overwrite is false: {}",
            destination.display()
        );
    }
    let parent = destination
        .parent()
        .context("download destination has no parent directory")?;
    if !parent.is_dir() {
        bail!(
            "download destination directory does not exist: {}",
            parent.display()
        );
    }

    let mut url = Url::parse(raw_url).context("invalid download URL")?;
    let mut response = None;
    for redirect_count in 0..=MAX_REDIRECTS {
        let (client, checked_url) = client_for_public_url(url).await?;
        let current = client
            .get(checked_url.clone())
            .send()
            .await
            .with_context(|| format!("download request failed for {checked_url}"))?;
        if current.status().is_redirection() {
            if redirect_count == MAX_REDIRECTS {
                bail!("download exceeded {MAX_REDIRECTS} redirects");
            }
            let location = current
                .headers()
                .get(LOCATION)
                .context("download redirect omitted the Location header")?
                .to_str()
                .context("download redirect Location is not valid text")?;
            url = checked_url
                .join(location)
                .context("download redirect Location is invalid")?;
            continue;
        }
        response = Some(current);
        break;
    }
    let response = response.context("download did not produce a response")?;
    if response.status() != StatusCode::OK {
        bail!("download returned HTTP {}", response.status());
    }
    if response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|length| length > MAX_DOWNLOAD_BYTES)
    {
        bail!("download exceeds the 25 MiB size limit");
    }

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let file_name = destination
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("download");
    let temporary = parent.join(format!(
        ".{file_name}.finn-{}-{nonce}.tmp",
        std::process::id()
    ));
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .await
        .with_context(|| format!("cannot create temporary download {}", temporary.display()))?;

    let result = async {
        let mut bytes_written = 0_u64;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("download stream failed")?;
            bytes_written = bytes_written
                .checked_add(chunk.len() as u64)
                .context("download size overflow")?;
            if bytes_written > MAX_DOWNLOAD_BYTES {
                bail!("download exceeds the 25 MiB size limit");
            }
            file.write_all(&chunk)
                .await
                .context("cannot write download")?;
        }
        file.flush().await.context("cannot flush download")?;
        drop(file);
        tokio::fs::rename(&temporary, destination)
            .await
            .with_context(|| format!("cannot finalize download to {}", destination.display()))?;
        Ok::<u64, anyhow::Error>(bytes_written)
    }
    .await;

    match result {
        Ok(bytes_written) => Ok(format!(
            "downloaded: true\npath: {}\nsize_bytes: {bytes_written}",
            destination.display()
        )),
        Err(error) => {
            let _ = tokio::fs::remove_file(&temporary).await;
            Err(error)
        }
    }
}

async fn client_for_public_url(url: Url) -> Result<(Client, Url)> {
    if url.scheme() != "https" {
        bail!("download_url accepts HTTPS URLs only");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("download URL must not contain credentials");
    }
    let host = url.host_str().context("download URL has no host")?;
    if host.eq_ignore_ascii_case("localhost") || host.ends_with(".localhost") {
        bail!("download URL must use a public host");
    }
    let port = url.port_or_known_default().unwrap_or(443);
    let addresses = tokio::net::lookup_host((host, port))
        .await
        .with_context(|| format!("cannot resolve download host {host}"))?
        .collect::<Vec<_>>();
    if addresses.is_empty() || addresses.iter().any(|address| !is_public_ip(address.ip())) {
        bail!("download URL resolved to a non-public network address");
    }
    let pinned = SocketAddr::new(addresses[0].ip(), port);
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .resolve(host, pinned)
        .build()
        .context("cannot initialize HTTPS downloader")?;
    Ok((client, url))
}

fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_public_ipv4(ip),
        IpAddr::V6(ip) => is_public_ipv6(ip),
    }
}

fn is_public_ipv4(ip: Ipv4Addr) -> bool {
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_unspecified()
        || ip.octets()[0] == 0
        || ip.octets()[0] >= 240
        || ip.octets()[0] == 100 && (64..=127).contains(&ip.octets()[1])
        || ip.octets()[0] == 192 && ip.octets()[1] == 0 && ip.octets()[2] == 0)
}

fn is_public_ipv6(ip: Ipv6Addr) -> bool {
    let segments = ip.segments();
    !(ip.is_loopback()
        || ip.is_multicast()
        || ip.is_unspecified()
        || (segments[0] & 0xfe00) == 0xfc00
        || (segments[0] & 0xffc0) == 0xfe80
        || (segments[0] == 0x2001 && segments[1] == 0x0db8))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_public_addresses() {
        for address in [
            "127.0.0.1",
            "10.0.0.1",
            "169.254.1.1",
            "192.168.1.1",
            "100.64.0.1",
            "::1",
            "fc00::1",
            "fe80::1",
            "2001:db8::1",
        ] {
            assert!(!is_public_ip(address.parse().unwrap()), "{address}");
        }
        assert!(is_public_ip("8.8.8.8".parse().unwrap()));
        assert!(is_public_ip("2606:4700:4700::1111".parse().unwrap()));
    }
}
