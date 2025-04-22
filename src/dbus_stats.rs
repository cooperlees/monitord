//! # dbus_stats module
//!
//! Handle getting statistics of our Dbus daemon/broker

use anyhow::Result;

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, Eq, PartialEq)]
pub struct DbusStats {
    foo: u32,
}

pub async fn get_dbus_stats(connection: &zbus::Connection) -> Result<DbusStats> {
    let p = zbus::fdo::StatsProxy::new(&connection).await?;
    println!("COOPER 1");
    let stats = p.get_stats().await?;

    println!("DBUS STATS:\n{:#?}", stats);

    Ok(DbusStats { foo: 69 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_get_dbus_stats() -> Result<()> {
        let connection = zbus::Connection::system().await?;
        let foo = get_dbus_stats(&connection).await?;
        assert!(foo.foo == 70 as u32);
        Ok(())
    }
}
