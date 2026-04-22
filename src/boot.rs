//! # boot module
//!
//! Collects boot blame metrics showing the slowest units at boot.
//! Similar to `systemd-analyze blame` but stores N slowest units.

use std::array::TryFromSliceError;
use std::collections::HashMap;
use std::io::ErrorKind;
use std::num::TryFromIntError;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::RwLock;
use tracing::debug;
use zbus::zvariant::ObjectPath;

use crate::config::Config;
use crate::dbus::zbus_systemd::ManagerProxy;
use crate::dbus::zbus_unit::UnitProxy;
use crate::MachineStats;

/// Boot blame statistics: maps unit name to activation time in seconds
pub type BootBlameStats = HashMap<String, f64>;

const BOOT_ID_PATH: &str = "/proc/sys/kernel/random/boot_id";
const BOOT_BLAME_CACHE_DIR: &str = "/run/monitord";
const BOOT_BLAME_CACHE_SUFFIX: &str = "boot_blame.bin";

type BootCacheResult<T> = std::result::Result<T, BootCacheError>;

#[derive(Debug, thiserror::Error)]
enum BootCacheError {
    #[error("boot cache I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("boot id from {BOOT_ID_PATH} was empty")]
    EmptyBootId,
    #[error("boot cache payload decode error: {0}")]
    InvalidPayload(&'static str),
    #[error("boot cache UTF-8 decode error: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
    #[error("boot cache integer conversion error: {0}")]
    IntConversion(#[from] TryFromIntError),
    #[error("boot cache slice conversion error: {0}")]
    SliceConversion(#[from] TryFromSliceError),
}

fn cache_file_path(cache_dir: &Path, boot_id: &str) -> PathBuf {
    cache_dir.join(format!("{boot_id}.{BOOT_BLAME_CACHE_SUFFIX}"))
}

async fn get_boot_id() -> BootCacheResult<String> {
    let boot_id = tokio::fs::read_to_string(BOOT_ID_PATH).await?;
    let boot_id = boot_id.trim().to_string();
    if boot_id.is_empty() {
        return Err(BootCacheError::EmptyBootId);
    }
    Ok(boot_id)
}

fn encode_boot_blame_stats(stats: &BootBlameStats) -> BootCacheResult<Vec<u8>> {
    let mut out = Vec::new();
    let entry_count = u32::try_from(stats.len())?;
    out.extend_from_slice(&entry_count.to_le_bytes());

    for (unit_name, activation_time) in stats {
        let unit_name_bytes = unit_name.as_bytes();
        let unit_name_len = u32::try_from(unit_name_bytes.len())?;
        out.extend_from_slice(&unit_name_len.to_le_bytes());
        out.extend_from_slice(unit_name_bytes);
        out.extend_from_slice(&activation_time.to_le_bytes());
    }

    Ok(out)
}

fn decode_boot_blame_stats(content: &[u8]) -> BootCacheResult<BootBlameStats> {
    const U32_BYTES: usize = std::mem::size_of::<u32>();
    const F64_BYTES: usize = std::mem::size_of::<f64>();
    fn read_u32(bytes: &[u8], offset: &mut usize) -> BootCacheResult<u32> {
        if *offset + std::mem::size_of::<u32>() > bytes.len() {
            return Err(BootCacheError::InvalidPayload("unexpected end of payload"));
        }
        let value =
            u32::from_le_bytes(bytes[*offset..*offset + std::mem::size_of::<u32>()].try_into()?);
        *offset += std::mem::size_of::<u32>();
        Ok(value)
    }

    if content.len() < U32_BYTES {
        return Err(BootCacheError::InvalidPayload("payload too small"));
    }

    let mut offset = 0usize;
    let entry_count = read_u32(content, &mut offset)? as usize;
    let mut stats = BootBlameStats::with_capacity(entry_count);

    for _ in 0..entry_count {
        let name_len = read_u32(content, &mut offset)? as usize;
        if offset + name_len + F64_BYTES > content.len() {
            return Err(BootCacheError::InvalidPayload("invalid payload size"));
        }
        let unit_name = String::from_utf8(content[offset..offset + name_len].to_vec())?;
        offset += name_len;
        let activation_time = f64::from_le_bytes(content[offset..offset + F64_BYTES].try_into()?);
        offset += F64_BYTES;
        stats.insert(unit_name, activation_time);
    }

    if offset != content.len() {
        return Err(BootCacheError::InvalidPayload("trailing bytes in payload"));
    }

    Ok(stats)
}

async fn read_cached_boot_blame_from_dir(
    cache_dir: &Path,
    boot_id: &str,
) -> BootCacheResult<Option<BootBlameStats>> {
    let cache_path = cache_file_path(cache_dir, boot_id);
    let content = match tokio::fs::read(&cache_path).await {
        Ok(content) => content,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    Ok(Some(decode_boot_blame_stats(&content)?))
}

async fn write_cached_boot_blame_to_dir(
    cache_dir: &Path,
    boot_id: &str,
    stats: &BootBlameStats,
) -> BootCacheResult<()> {
    tokio::fs::create_dir_all(cache_dir).await?;
    let cache_path = cache_file_path(cache_dir, boot_id);
    let encoded = encode_boot_blame_stats(stats)?;
    tokio::fs::write(cache_path, encoded).await?;
    Ok(())
}

async fn read_cached_boot_blame(boot_id: &str) -> BootCacheResult<Option<BootBlameStats>> {
    read_cached_boot_blame_from_dir(Path::new(BOOT_BLAME_CACHE_DIR), boot_id).await
}

async fn write_cached_boot_blame(boot_id: &str, stats: &BootBlameStats) -> BootCacheResult<()> {
    write_cached_boot_blame_to_dir(Path::new(BOOT_BLAME_CACHE_DIR), boot_id, stats).await
}

/// Calculate the activation time for a unit
/// Returns the time in seconds from InactiveExitTimestamp to ActiveEnterTimestamp
async fn get_unit_activation_time(
    connection: &zbus::Connection,
    unit_path: &ObjectPath<'_>,
) -> Result<f64> {
    let unit_proxy = UnitProxy::builder(connection)
        .cache_properties(zbus::proxy::CacheProperties::No)
        .path(unit_path)?
        .build()
        .await?;

    let inactive_exit = unit_proxy.inactive_exit_timestamp().await?;
    let active_enter = unit_proxy.active_enter_timestamp().await?;

    // If either timestamp is 0, the unit hasn't been activated or the timing is invalid
    if inactive_exit == 0 || active_enter == 0 {
        return Ok(0.0);
    }

    // Calculate activation time in seconds (timestamps are in microseconds)
    let activation_time_usec = active_enter.saturating_sub(inactive_exit);
    let activation_time_sec = activation_time_usec as f64 / 1_000_000.0;

    Ok(activation_time_sec)
}

/// Update boot blame statistics with the N slowest units at boot
pub async fn update_boot_blame_stats(
    config: Arc<Config>,
    connection: zbus::Connection,
    machine_stats: Arc<RwLock<MachineStats>>,
) -> Result<()> {
    debug!("Starting boot blame stats collection");

    let mut maybe_boot_id = None;
    if config.boot_blame.cache_enabled {
        let cached_stats = machine_stats.read().await.boot_blame.clone();
        if cached_stats.is_some() {
            debug!("Using in-memory cached boot blame stats");
            return Ok(());
        }

        match get_boot_id().await {
            Ok(boot_id) => {
                match read_cached_boot_blame(&boot_id).await {
                    Ok(Some(cached_boot_blame)) => {
                        let cache_path = cache_file_path(Path::new(BOOT_BLAME_CACHE_DIR), &boot_id);
                        debug!(
                            "Using cached boot blame stats from {}",
                            cache_path.display()
                        );
                        machine_stats.write().await.boot_blame = Some(cached_boot_blame);
                        return Ok(());
                    }
                    Ok(None) => {
                        debug!("No cached boot blame stats found for boot id {}", boot_id);
                    }
                    Err(err) => {
                        debug!(
                            "Failed to load boot blame cache for boot id {}: {}",
                            boot_id, err
                        );
                    }
                }
                maybe_boot_id = Some(boot_id);
            }
            Err(err) => {
                debug!("Failed to retrieve boot id for boot blame cache: {}", err);
            }
        }
    }

    let systemd_proxy = ManagerProxy::builder(&connection)
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await?;
    let units = systemd_proxy.list_units().await?;

    let mut unit_times: Vec<(String, f64)> = Vec::new();

    // Collect activation times for all units
    for unit_info in units {
        let unit_name = unit_info.0;
        let unit_path = unit_info.6;

        // Apply blocklist: skip units explicitly excluded
        if config.boot_blame.blocklist.contains(&unit_name) {
            debug!("Skipping boot blame for {} due to blocklist", &unit_name);
            continue;
        }
        // Apply allowlist: if non-empty, only include listed units
        if !config.boot_blame.allowlist.is_empty()
            && !config.boot_blame.allowlist.contains(&unit_name)
        {
            continue;
        }

        match get_unit_activation_time(&connection, &unit_path).await {
            Ok(time) if time > 0.0 => {
                unit_times.push((unit_name, time));
            }
            Ok(_) => {
                // Unit has no activation time (0.0), skip it
            }
            Err(e) => {
                debug!("Failed to get activation time for {}: {}", unit_name, e);
            }
        }
    }

    // Sort by activation time in descending order (slowest first)
    unit_times.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    // Take only the N slowest units
    let num_slowest = config.boot_blame.num_slowest_units as usize;
    unit_times.truncate(num_slowest);

    // Convert to HashMap
    let boot_blame_stats: BootBlameStats = unit_times.into_iter().collect();

    debug!("Collected {} boot blame stats", boot_blame_stats.len());

    // Update machine stats
    let mut stats = machine_stats.write().await;
    stats.boot_blame = Some(boot_blame_stats);
    if config.boot_blame.cache_enabled {
        if let Some(boot_id) = maybe_boot_id {
            if let Some(cached_stats) = stats.boot_blame.as_ref() {
                if let Err(err) = write_cached_boot_blame(&boot_id, cached_stats).await {
                    debug!(
                        "Failed to write boot blame cache for boot id {} to {}: {}",
                        boot_id, BOOT_BLAME_CACHE_DIR, err
                    );
                } else {
                    debug!("Updated boot blame cache for boot id {}", boot_id);
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_boot_blame_cache_encode_decode_roundtrip() {
        let mut stats = BootBlameStats::new();
        stats.insert("foo.service".to_string(), 12.3);
        stats.insert("bar.service".to_string(), 45.6);

        let encoded = encode_boot_blame_stats(&stats).expect("encode should succeed");
        let decoded = decode_boot_blame_stats(&encoded).expect("decode should succeed");
        assert_eq!(stats, decoded);
    }

    #[test]
    fn test_boot_blame_cache_decode_invalid_payload() {
        let invalid_payload = vec![0, 1, 2];
        assert!(decode_boot_blame_stats(&invalid_payload).is_err());
    }

    #[tokio::test]
    async fn test_boot_blame_cache_read_write_roundtrip() {
        let temp_dir = tempfile::tempdir().expect("create temp dir");
        let boot_id = "00000000-0000-0000-0000-000000000001";
        let mut stats = BootBlameStats::new();
        stats.insert("foo.service".to_string(), 1.25);

        write_cached_boot_blame_to_dir(temp_dir.path(), boot_id, &stats)
            .await
            .expect("write cache");
        let read_stats = read_cached_boot_blame_from_dir(temp_dir.path(), boot_id)
            .await
            .expect("read cache");
        assert_eq!(Some(stats), read_stats);
    }

    #[tokio::test]
    async fn test_boot_blame_cache_read_missing_file() {
        let temp_dir = tempfile::tempdir().expect("create temp dir");
        let missing = read_cached_boot_blame_from_dir(
            temp_dir.path(),
            "00000000-0000-0000-0000-000000000002",
        )
        .await
        .expect("missing cache should not error");
        assert!(missing.is_none());
    }
}
