use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;

use thiserror::Error;
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, error, warn};

use crate::MachineStats;
use crate::MonitordStats;

/// Cached D-Bus connections to containers, keyed by machine name.
/// The u32 is the leader PID at the time the connection was established.
/// A connection is only reused if the current leader PID matches.
pub type MachineConnections = HashMap<String, (u32, zbus::Connection)>;

/// What action to take for a machine's cached connection.
#[derive(Debug, PartialEq)]
enum CacheAction {
    /// Cached connection exists with matching leader PID — reuse it.
    Reuse,
    /// Cached connection exists but leader PID changed — drop old and create new.
    Replace,
    /// No cached connection — create new.
    Create,
}

#[derive(Error, Debug)]
pub enum MonitordMachinesError {
    #[error("Machines D-Bus error: {0}")]
    ZbusError(#[from] zbus::Error),
}

pub fn filter_machines(
    machines: Vec<crate::dbus::zbus_machines::ListedMachine>,
    allowlist: &HashSet<String>,
    blocklist: &HashSet<String>,
) -> Vec<crate::dbus::zbus_machines::ListedMachine> {
    machines
        .into_iter()
        .filter(|c| c.class == "container")
        .filter(|c| !blocklist.contains(&c.name))
        .filter(|c| allowlist.is_empty() || allowlist.contains(&c.name))
        .collect()
}

pub async fn get_machines(
    connection: &zbus::Connection,
    config: &crate::config::Config,
) -> Result<HashMap<String, u32>, MonitordMachinesError> {
    let c = crate::dbus::zbus_machines::ManagerProxy::builder(connection)
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await?;
    let mut results = HashMap::<String, u32>::new();

    let machines = c.list_machines().await?;

    for machine in filter_machines(
        machines,
        &config.machines.allowlist,
        &config.machines.blocklist,
    ) {
        let m = c.get_machine(&machine.name).await?;
        let leader_pid = m.leader().await?;
        results.insert(machine.name, leader_pid);
    }

    Ok(results)
}

/// Determine the cache action for a machine based on its cached and current leader PID.
fn decide_cache_action(cached_pid: Option<u32>, leader_pid: u32) -> CacheAction {
    match cached_pid {
        Some(pid) if pid == leader_pid => CacheAction::Reuse,
        Some(_) => CacheAction::Replace,
        None => CacheAction::Create,
    }
}

/// Remove cached connections for machines that no longer exist.
async fn evict_stale_connections(
    cached_connections: &Mutex<MachineConnections>,
    current_machines: &HashMap<String, u32>,
) {
    let mut cache = cached_connections.lock().await;
    cache.retain(|name, _| current_machines.contains_key(name));
}

/// Evict a cached connection for a machine that experienced errors.
async fn evict_failed_connection(cached_connections: &Mutex<MachineConnections>, machine: &str) {
    debug!(
        "Evicting cached D-Bus connection for {} due to errors",
        machine
    );
    let mut cache = cached_connections.lock().await;
    cache.remove(machine);
}

/// Return a cached D-Bus connection if one exists for the same leader PID,
/// otherwise create a new connection to the container's system bus.
async fn get_or_create_connection(
    config: &crate::config::Config,
    cached_connections: &Mutex<MachineConnections>,
    machine: &str,
    leader_pid: u32,
) -> anyhow::Result<zbus::Connection> {
    // Check cache and return if hit; drop the lock before any async work
    {
        let mut cache = cached_connections.lock().await;
        match decide_cache_action(cache.get(machine).map(|(pid, _)| *pid), leader_pid) {
            CacheAction::Reuse => {
                debug!("Reusing cached D-Bus connection for {}", machine);
                let (_, conn) = cache.get(machine).unwrap();
                return Ok(conn.clone());
            }
            CacheAction::Replace => {
                debug!(
                    "Leader PID changed for {}, dropping stale connection",
                    machine
                );
                cache.remove(machine);
            }
            CacheAction::Create => {}
        }
    }

    // Build connection without holding the lock
    debug!("Creating new D-Bus connection for {}", machine);
    let container_address = format!(
        "unix:path=/proc/{}/root/run/dbus/system_bus_socket",
        leader_pid
    );
    let conn = zbus::connection::Builder::address(container_address.as_str())?
        .method_timeout(std::time::Duration::from_secs(config.monitord.dbus_timeout))
        .build()
        .await?;

    // Re-lock to insert
    {
        let mut cache = cached_connections.lock().await;
        cache.insert(machine.to_string(), (leader_pid, conn.clone()));
    }

    Ok(conn)
}

pub async fn update_machines_stats(
    config: Arc<crate::config::Config>,
    connection: zbus::Connection,
    locked_monitord_stats: Arc<RwLock<MonitordStats>>,
    cached_connections: Arc<Mutex<MachineConnections>>,
) -> anyhow::Result<()> {
    let locked_machine_stats: Arc<RwLock<MachineStats>> =
        Arc::new(RwLock::new(MachineStats::default()));

    let current_machines = get_machines(&connection, &config).await?;

    evict_stale_connections(&cached_connections, &current_machines).await;

    for (machine, leader_pid) in current_machines.into_iter() {
        debug!(
            "Collecting container: machine: {} leader_pid: {}",
            machine, leader_pid
        );

        let sdc = match get_or_create_connection(&config, &cached_connections, &machine, leader_pid)
            .await
        {
            Ok(conn) => conn,
            Err(e) => {
                error!("Failed to connect to container {}: {:?}", machine, e);
                continue;
            }
        };

        let mut join_set = tokio::task::JoinSet::new();

        if config.pid1.enabled {
            join_set.spawn(crate::pid1::update_pid1_stats(
                leader_pid as i32,
                locked_machine_stats.clone(),
            ));
        }

        if config.networkd.enabled {
            join_set.spawn(crate::networkd::update_networkd_stats(
                config.networkd.link_state_dir.clone(),
                None,
                sdc.clone(),
                locked_machine_stats.clone(),
            ));
        }

        if config.system_state.enabled {
            join_set.spawn(crate::system::update_system_stats(
                sdc.clone(),
                locked_machine_stats.clone(),
            ));
        }

        join_set.spawn(crate::system::update_version(
            sdc.clone(),
            locked_machine_stats.clone(),
        ));

        if config.units.enabled {
            if config.varlink.enabled {
                let config_clone = Arc::clone(&config);
                let sdc_clone = sdc.clone();
                let stats_clone = locked_machine_stats.clone();
                let container_socket_path = format!(
                    "/proc/{}/root{}",
                    leader_pid,
                    crate::varlink_units::METRICS_SOCKET_PATH
                );
                join_set.spawn(async move {
                    match crate::varlink_units::update_unit_stats(
                        Arc::clone(&config_clone),
                        stats_clone.clone(),
                        container_socket_path,
                    )
                    .await
                    {
                        Ok(()) => Ok(()),
                        Err(err) => {
                            warn!(
                                "Varlink units stats failed, falling back to D-Bus: {:?}",
                                err
                            );
                            crate::units::update_unit_stats(config_clone, sdc_clone, stats_clone)
                                .await
                        }
                    }
                });
            } else {
                join_set.spawn(crate::units::update_unit_stats(
                    Arc::clone(&config),
                    sdc.clone(),
                    locked_machine_stats.clone(),
                ));
            }
        }

        if config.dbus_stats.enabled {
            join_set.spawn(crate::dbus_stats::update_dbus_stats(
                Arc::clone(&config),
                sdc.clone(),
                locked_machine_stats.clone(),
            ));
        }

        let mut had_error = false;
        while let Some(res) = join_set.join_next().await {
            match res {
                Ok(r) => match r {
                    Ok(_) => (),
                    Err(e) => {
                        had_error = true;
                        error!(
                            "Collection specific failure (container {}): {:?}",
                            machine, e
                        );
                    }
                },
                Err(e) => {
                    had_error = true;
                    error!("Join error (container {}): {:?}", machine, e);
                }
            }
        }

        if had_error {
            evict_failed_connection(&cached_connections, &machine).await;
        }

        {
            let mut monitord_stats = locked_monitord_stats.write().await;
            let machine_stats = locked_machine_stats.read().await;
            monitord_stats
                .machines
                .insert(machine, machine_stats.clone());
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use zbus::zvariant::OwnedObjectPath;

    use super::{decide_cache_action, CacheAction};

    #[test]
    fn test_filter_machines() {
        let machines = vec![
            crate::dbus::zbus_machines::ListedMachine {
                name: "foo".to_string(),
                class: "container".to_string(),
                service: "".to_string(),
                path: OwnedObjectPath::try_from("/sample/object").unwrap(),
            },
            crate::dbus::zbus_machines::ListedMachine {
                name: "bar".to_string(),
                class: "container".to_string(),
                service: "".to_string(),
                path: OwnedObjectPath::try_from("/sample/object").unwrap(),
            },
            crate::dbus::zbus_machines::ListedMachine {
                name: "baz".to_string(),
                class: "container".to_string(),
                service: "".to_string(),
                path: OwnedObjectPath::try_from("/sample/object").unwrap(),
            },
        ];
        let allowlist = HashSet::from(["foo".to_string(), "baz".to_string()]);
        let blocklist = HashSet::from(["bar".to_string()]);

        let filtered = super::filter_machines(machines, &allowlist, &blocklist);

        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].name, "foo");
        assert_eq!(filtered[1].name, "baz");
    }

    #[test]
    fn test_decide_cache_action_reuse_on_same_pid() {
        assert_eq!(decide_cache_action(Some(42), 42), CacheAction::Reuse);
    }

    #[test]
    fn test_decide_cache_action_replace_on_pid_change() {
        assert_eq!(decide_cache_action(Some(42), 99), CacheAction::Replace);
    }

    #[test]
    fn test_decide_cache_action_create_on_miss() {
        assert_eq!(decide_cache_action(None, 42), CacheAction::Create);
    }
}
