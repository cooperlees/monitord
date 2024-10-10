use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;
use tracing::{debug, error};

use crate::MachineStats;
use crate::MonitordStats;

pub async fn get_machines(
    connection: &zbus::Connection,
    config: &crate::config::Config,
) -> Result<HashMap<String, u32>, zbus::Error> {
    let c = crate::dbus::zbus_machines::ManagerProxy::new(connection).await?;
    let mut results = HashMap::<String, u32>::new();

    let machines = c.list_machines().await?;

    for machine in machines.iter().filter(|c| &c.1 == "container") {
        let m = c.get_machine(&machine.0).await?;
        let leader = m.leader().await?;

        if config.machines.auto_discover || config.machines.machines_list.contains(&machine.0) {
            results.insert(machine.0.to_string(), leader);
        }
    }

    return Ok(results);
}
pub async fn update_machines_stats(
    config: crate::config::Config,
    connection: zbus::Connection,
    locked_monitord_stats: Arc<RwLock<MonitordStats>>,
) -> anyhow::Result<()> {
    let locked_machine_stats: Arc<RwLock<MachineStats>> =
        Arc::new(RwLock::new(MachineStats::default()));

    for (machine, leader) in get_machines(&connection, &config).await?.into_iter() {
        debug!(
            "Collecting container: machine: {} leader: {}",
            machine, leader
        );
        let container_address =
            format!("unix:path=/proc/{}/root/run/dbus/system_bus_socket", leader);
        let sdc = zbus::connection::Builder::address(container_address.as_str())?
            .build()
            .await?;
        let mut join_set = tokio::task::JoinSet::new();

        if config.pid1.enabled {
            join_set.spawn(crate::pid1::update_pid1_stats(
                leader as i32,
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

        let mut monitord_stats = locked_monitord_stats.write().await;
        let machine_stats = locked_machine_stats.read().await;
        monitord_stats
            .machines
            .insert(machine.clone(), machine_stats.clone());
    }

    Ok(())
}
