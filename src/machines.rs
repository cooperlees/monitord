use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;
use tracing::{debug, error};

use crate::MachineStats;
use crate::MonitordStats;

pub fn filter_machines(
    machines: Vec<crate::dbus::zbus_machines::ListedMachine>,
    allowlist: Vec<String>,
    blocklist: Vec<String>,
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
) -> Result<HashMap<String, u32>, zbus::Error> {
    let c = crate::dbus::zbus_machines::ManagerProxy::new(connection).await?;
    let mut results = HashMap::<String, u32>::new();

    let machines = c.list_machines().await?;

    for machine in filter_machines(
        machines,
        config.machines.allowlist.clone(),
        config.machines.blocklist.clone(),
    ) {
        let m = c.get_machine(&machine.name).await?;
        let leader_pid = m.leader().await?;
        results.insert(machine.name.to_string(), leader_pid);
    }

    Ok(results)
}

pub async fn update_machines_stats(
    config: crate::config::Config,
    connection: zbus::Connection,
    locked_monitord_stats: Arc<RwLock<MonitordStats>>,
) -> anyhow::Result<()> {
    let locked_machine_stats: Arc<RwLock<MachineStats>> =
        Arc::new(RwLock::new(MachineStats::default()));

    for (machine, leader_pid) in get_machines(&connection, &config).await?.into_iter() {
        debug!(
            "Collecting container: machine: {} leader_pid: {}",
            machine, leader_pid
        );
        let container_address = format!(
            "unix:path=/proc/{}/root/run/dbus/system_bus_socket",
            leader_pid
        );
        let sdc = zbus::connection::Builder::address(container_address.as_str())?
            .method_timeout(std::time::Duration::from_secs(config.monitord.dbus_timeout))
            .build()
            .await?;
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
            join_set.spawn(crate::units::update_unit_stats(
                config.clone(),
                sdc.clone(),
                locked_machine_stats.clone(),
            ));
        }

        while let Some(res) = join_set.join_next().await {
            match res {
                Ok(r) => match r {
                    Ok(_) => (),
                    Err(e) => {
                        error!(
                            "Collection specific failure (container {}): {:?}",
                            machine, e
                        );
                    }
                },
                Err(e) => {
                    error!("Join error (container {}): {:?}", machine, e);
                }
            }
        }

        {
            let mut monitord_stats = locked_monitord_stats.write().await;
            let machine_stats = locked_machine_stats.read().await;
            monitord_stats
                .machines
                .insert(machine.clone(), machine_stats.clone());
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use zbus::zvariant::OwnedObjectPath;

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
        let allowlist = vec!["foo".to_string(), "baz".to_string()];
        let blocklist = vec!["bar".to_string()];

        let filtered = super::filter_machines(machines, allowlist, blocklist);

        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].name, "foo");
        assert_eq!(filtered[1].name, "baz");
    }
}
