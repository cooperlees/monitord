pub mod json;
pub mod networkd;

// TODO: Add other components as support is added
#[derive(serde::Serialize, serde::Deserialize, Debug, Eq, PartialEq)]
pub struct MonitordStats {
    pub networkd: networkd::NetworkdState,
}
