use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use thiserror::Error;
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, error, warn};

use crate::varlink::endpoint::VarlinkEndpoint;
use crate::varlink::machine_connector::{self, MachineConnector, MachineSocket};
use crate::MachineStats;
use crate::MonitordStats;

/// A machine's varlink connector, or why it could not be spawned. Errors are
/// cached too, so a machine costs at most one spawn attempt per leader PID.
type ConnectorSlot = Result<Arc<MachineConnector>, Arc<anyhow::Error>>;

/// Everything cached for one machine, valid while its leader PID is unchanged.
pub struct MachineConnection {
    leader_pid: u32,
    dbus: zbus::Connection,
    /// Spawned on first varlink use, see [`machine_varlink_connector`].
    varlink: Option<ConnectorSlot>,
}

/// Cached per-machine connections, keyed by machine name.
/// An entry is only reused if the machine's current leader PID matches.
pub type MachineConnections = HashMap<String, MachineConnection>;

/// Set once joining machine PID namespaces failed for lack of privileges.
/// That won't change at runtime, so from then on machines are collected over
/// D-Bus without retrying (or logging) for every machine and cycle.
static MACHINE_VARLINK_UNAVAILABLE: OnceLock<Arc<anyhow::Error>> = OnceLock::new();

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
    debug!("Evicting cached connections for {} due to errors", machine);
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
        match decide_cache_action(cache.get(machine).map(|c| c.leader_pid), leader_pid) {
            CacheAction::Reuse => {
                debug!("Reusing cached D-Bus connection for {}", machine);
                return Ok(cache[machine].dbus.clone());
            }
            CacheAction::Replace => {
                debug!(
                    "Leader PID changed for {}, dropping stale connections",
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
        cache.insert(
            machine.to_string(),
            MachineConnection {
                leader_pid,
                dbus: conn.clone(),
                varlink: None,
            },
        );
    }

    Ok(conn)
}

/// The varlink connector for a machine, spawning it on first use.
///
/// `None` means don't try varlink for this machine at all: it is disabled, or
/// monitord lacks the privileges to join machine namespaces (warned about
/// once). With `[varlink] no_fallback` the latter is returned as an error
/// instead, so it fails loudly rather than quietly using D-Bus.
async fn machine_varlink_connector(
    config: &crate::config::Config,
    cached_connections: &Mutex<MachineConnections>,
    machine: &str,
    leader_pid: u32,
) -> Option<ConnectorSlot> {
    if !config.use_varlink(&[config.machines.varlink]) {
        return None;
    }
    if let Some(err) = MACHINE_VARLINK_UNAVAILABLE.get() {
        return config.varlink.no_fallback.then(|| Err(Arc::clone(err)));
    }
    {
        let cache = cached_connections.lock().await;
        match cache
            .get(machine)
            .filter(|c| c.leader_pid == leader_pid)
            .and_then(|c| c.varlink.clone())
        {
            Some(Ok(connector)) if !connector.is_alive() => {
                debug!("Varlink connector for {} exited, respawning", machine);
            }
            Some(slot) => return Some(slot),
            None => {}
        }
    }

    let timeout = Duration::from_secs(config.monitord.dbus_timeout);
    let slot: ConnectorSlot =
        match tokio::task::spawn_blocking(move || MachineConnector::spawn(leader_pid, timeout))
            .await
        {
            Ok(Ok(connector)) => {
                debug!(
                    "Spawned varlink connector for machine {} (leader {})",
                    machine, leader_pid
                );
                Ok(Arc::new(connector))
            }
            Ok(Err(err)) => Err(Arc::new(err.into())),
            Err(err) => Err(Arc::new(err.into())),
        };

    if let Err(err) = &slot {
        if machine_connector::is_permission_denied(err) {
            if MACHINE_VARLINK_UNAVAILABLE.set(Arc::clone(err)).is_ok() {
                warn!("{:#}; collecting machines over D-Bus instead", err);
            }
            return config.varlink.no_fallback.then_some(slot);
        }
    }

    let mut cache = cached_connections.lock().await;
    if let Some(entry) = cache
        .get_mut(machine)
        .filter(|c| c.leader_pid == leader_pid)
    {
        entry.varlink = Some(slot.clone());
    }
    Some(slot)
}

/// The endpoint for `socket` in a machine, if varlink is to be tried there.
fn machine_endpoint(
    connector: &Option<ConnectorSlot>,
    socket: MachineSocket,
) -> Option<anyhow::Result<VarlinkEndpoint>> {
    connector.as_ref().map(|slot| match slot {
        Ok(connector) => Ok(VarlinkEndpoint::Machine {
            connector: Arc::clone(connector),
            socket,
        }),
        Err(err) => Err(anyhow::anyhow!("{:#}", err)),
    })
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

        let connector =
            machine_varlink_connector(&config, &cached_connections, &machine, leader_pid).await;

        let mut join_set = tokio::task::JoinSet::new();

        if config.pid1.enabled {
            join_set.spawn(crate::pid1::update_pid1_stats(
                leader_pid as i32,
                locked_machine_stats.clone(),
            ));
        }

        if config.networkd.enabled {
            let config_clone = Arc::clone(&config);
            let stats_clone = locked_machine_stats.clone();
            let machine_name = machine.clone();
            let no_fallback = config_clone.varlink.no_fallback;
            let endpoint = config_clone
                .use_varlink(&[config_clone.networkd.varlink])
                .then(|| machine_endpoint(&connector, MachineSocket::Network))
                .flatten();
            join_set.spawn(async move {
                if let Some(endpoint) = endpoint {
                    let result = match endpoint {
                        Ok(endpoint) => {
                            crate::varlink_networkd::get_networkd_state(&endpoint).await
                        }
                        Err(err) => Err(err),
                    };
                    match result {
                        Ok(networkd_stats) => {
                            let mut machine_stats = stats_clone.write().await;
                            machine_stats.networkd = networkd_stats;
                            machine_stats.varlink_usage.networkd =
                                Some(crate::CollectorTransport::Varlink);
                            return Ok(());
                        }
                        Err(err) => {
                            crate::varlink_fallback::report_varlink_failure(
                                no_fallback,
                                &format!("container {machine_name} networkd"),
                                "file-based",
                                err,
                            )?;
                        }
                    }
                }
                stats_clone.write().await.varlink_usage.networkd =
                    Some(crate::CollectorTransport::Dbus);
                // Same fs-root prefixing the units/cgroup collectors use:
                // both the link state files AND the sysfs ifindex map
                // come from inside the container, so host ifindexes are
                // never labelled with container interface names (or vice
                // versa). A container sharing the host netns simply sees
                // identical trees, which is why CI never caught the old
                // host-files/host-bus pairing being self-consistent.
                let container_root = format!("/proc/{leader_pid}/root");
                let container_sysfs = std::path::PathBuf::from(format!("{container_root}/sys"));
                let container_links = std::path::PathBuf::from(format!(
                    "{container_root}{}",
                    config_clone.networkd.link_state_dir.display()
                ));
                crate::networkd::update_networkd_stats(
                    container_links,
                    None,
                    container_sysfs,
                    None,
                    stats_clone,
                )
                .await
            });
        }

        // One Describe per container, shared by its version and system state
        // collectors, against the container's PID 1 varlink socket.
        let manager_describe = config
            .use_varlink(&[config.system_state.varlink])
            .then(|| machine_endpoint(&connector, MachineSocket::Manager))
            .flatten()
            .map(|endpoint| endpoint.map(crate::varlink_system::shared_describe));

        if config.system_state.enabled {
            let sdc_clone = sdc.clone();
            let stats_clone = locked_machine_stats.clone();
            let machine_name = machine.clone();
            let no_fallback = config.varlink.no_fallback;
            let describe = clone_describe(&manager_describe);
            join_set.spawn(async move {
                if let Some(describe) = describe {
                    let result = match describe {
                        Ok(describe) => {
                            crate::varlink_system::update_system_stats(
                                describe,
                                stats_clone.clone(),
                            )
                            .await
                        }
                        Err(err) => Err(err),
                    };
                    match result {
                        Ok(()) => {
                            stats_clone.write().await.varlink_usage.system_state =
                                Some(crate::CollectorTransport::Varlink);
                            return Ok(());
                        }
                        Err(err) => {
                            crate::varlink_fallback::report_varlink_failure(
                                no_fallback,
                                &format!("container {machine_name} system state"),
                                "D-Bus",
                                err,
                            )?;
                        }
                    }
                }
                stats_clone.write().await.varlink_usage.system_state =
                    Some(crate::CollectorTransport::Dbus);
                crate::system::update_system_stats(sdc_clone, stats_clone.clone()).await
            });
        }

        {
            let sdc_clone = sdc.clone();
            let stats_clone = locked_machine_stats.clone();
            let machine_name = machine.clone();
            let no_fallback = config.varlink.no_fallback;
            let describe = clone_describe(&manager_describe);
            join_set.spawn(async move {
                if let Some(describe) = describe {
                    let result = match describe {
                        Ok(describe) => {
                            crate::varlink_system::update_version(describe, stats_clone.clone())
                                .await
                        }
                        Err(err) => Err(err),
                    };
                    match result {
                        Ok(()) => {
                            stats_clone.write().await.varlink_usage.version =
                                Some(crate::CollectorTransport::Varlink);
                            return Ok(());
                        }
                        Err(err) => {
                            crate::varlink_fallback::report_varlink_failure(
                                no_fallback,
                                &format!("container {machine_name} version"),
                                "D-Bus",
                                err,
                            )?;
                        }
                    }
                }
                stats_clone.write().await.varlink_usage.version =
                    Some(crate::CollectorTransport::Dbus);
                crate::system::update_version(sdc_clone, stats_clone.clone()).await
            });
        }

        if config.units.enabled {
            let config_clone = Arc::clone(&config);
            let sdc_clone = sdc.clone();
            let stats_clone = locked_machine_stats.clone();
            let no_fallback = config_clone.varlink.no_fallback;
            let machine_name = machine.clone();
            let container_root = format!("/proc/{}/root", leader_pid);
            let endpoints = config
                .use_varlink(&[config.units.varlink])
                .then(|| {
                    let metrics = machine_endpoint(&connector, MachineSocket::Metrics)?;
                    let manager = machine_endpoint(&connector, MachineSocket::Manager)?;
                    Some(metrics.and_then(|metrics| manager.map(|manager| (metrics, manager))))
                })
                .flatten();
            join_set.spawn(async move {
                if let Some(endpoints) = endpoints {
                    match collect_container_units_varlink(
                        &config_clone,
                        &stats_clone,
                        endpoints,
                        &container_root,
                    )
                    .await
                    {
                        Ok(()) => {
                            stats_clone.write().await.varlink_usage.units =
                                Some(crate::CollectorTransport::Varlink);
                            return Ok(());
                        }
                        Err(err) => {
                            crate::varlink_fallback::report_varlink_failure(
                                no_fallback,
                                &format!("container {machine_name} units"),
                                "D-Bus",
                                err,
                            )?;
                        }
                    }
                }
                // Set before the call (the lib.rs ordering): if the D-Bus
                // collection errors, the gauge still says D-Bus rather than
                // going stale or absent.
                stats_clone.write().await.varlink_usage.units =
                    Some(crate::CollectorTransport::Dbus);
                crate::units::update_unit_stats(
                    config_clone,
                    sdc_clone,
                    stats_clone,
                    container_root,
                )
                .await
            });
        }

        if config.dbus_stats.enabled {
            join_set.spawn(crate::dbus_stats::update_machine_dbus_stats(
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

type MachineDescribe = Option<anyhow::Result<crate::varlink_system::SharedDescribe>>;

/// Clone the per-container shared `Describe` for one more collector; an
/// error is re-rendered, since `anyhow::Error` itself is not `Clone`.
fn clone_describe(describe: &MachineDescribe) -> MachineDescribe {
    describe.as_ref().map(|describe| match describe {
        Ok(describe) => Ok(describe.clone()),
        Err(err) => Err(anyhow::anyhow!("{:#}", err)),
    })
}

/// Collect a container's units over varlink the way the host does: unit and
/// timer metrics from `io.systemd.Metrics`, then per-service stats, timer
/// properties and service types from `io.systemd.Unit.List`, with cgroup and
/// unit file data read below the container's root. Any failure redoes the
/// whole collection over D-Bus (see the host path in `lib.rs` for why).
async fn collect_container_units_varlink(
    config: &Arc<crate::config::Config>,
    stats: &Arc<RwLock<MachineStats>>,
    endpoints: anyhow::Result<(VarlinkEndpoint, VarlinkEndpoint)>,
    container_root: &str,
) -> anyhow::Result<()> {
    let (metrics, manager) = endpoints?;
    let timer_names =
        crate::varlink_units::update_unit_stats(Arc::clone(config), stats.clone(), metrics).await?;
    crate::varlink_units::apply_unit_details(&manager, stats, config, container_root, &timer_names)
        .await?;
    if config.units.unit_files {
        let unit_files = crate::units::collect_unit_files_stats(container_root).await;
        stats.write().await.units.unit_files = unit_files;
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
