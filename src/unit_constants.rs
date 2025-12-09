//! # Unit Constants Module
//!
//! Shared constants and enums for systemd unit states and operations.
//! Reference: <https://www.freedesktop.org/software/systemd/man/org.freedesktop.systemd1.html>

use int_enum::IntEnum;
use serde_repr::*;
use strum_macros::EnumIter;
use strum_macros::EnumString;

/// Possible systemd unit active states enumerated
#[allow(non_camel_case_types)]
#[derive(
    Serialize_repr,
    Deserialize_repr,
    Clone,
    Copy,
    Debug,
    Default,
    Eq,
    PartialEq,
    EnumIter,
    EnumString,
    IntEnum,
    strum_macros::Display,
)]
#[repr(u8)]
pub enum SystemdUnitActiveState {
    #[default]
    unknown = 0,
    active = 1,
    reloading = 2,
    inactive = 3,
    failed = 4,
    activating = 5,
    deactivating = 6,
}

/// Possible systemd unit load states enumerated
#[allow(non_camel_case_types)]
#[derive(
    Serialize_repr,
    Deserialize_repr,
    Clone,
    Copy,
    Debug,
    Default,
    Eq,
    PartialEq,
    EnumIter,
    EnumString,
    IntEnum,
    strum_macros::Display,
)]
#[repr(u8)]
pub enum SystemdUnitLoadState {
    #[default]
    unknown = 0,
    loaded = 1,
    error = 2,
    masked = 3,
    not_found = 4,
}

/// Check if we're a loaded unit and if so evaluate if we're active or not
/// If we're not
/// Only potentially mark unhealthy for LOADED units that are not active
pub fn is_unit_unhealthy(
    active_state: SystemdUnitActiveState,
    load_state: SystemdUnitLoadState,
) -> bool {
    match load_state {
        // We're loaded so let's see if we're active or not
        SystemdUnitLoadState::loaded => !matches!(active_state, SystemdUnitActiveState::active),
        // An admin can change a unit to be masked on purpose
        // so we are going to ignore all masked units due to that
        SystemdUnitLoadState::masked => false,
        // Otherwise, we're unhealthy
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;
    use strum::IntoEnumIterator;

    #[test]
    fn test_is_unit_unhealthy() {
        // Obvious active/loaded is healthy
        assert!(!is_unit_unhealthy(
            SystemdUnitActiveState::active,
            SystemdUnitLoadState::loaded
        ));
        // Not active + loaded is not healthy
        assert!(is_unit_unhealthy(
            SystemdUnitActiveState::activating,
            SystemdUnitLoadState::loaded
        ));
        // Not loaded + anything is just marked healthy as we're not expecting it to ever be healthy
        assert!(!is_unit_unhealthy(
            SystemdUnitActiveState::activating,
            SystemdUnitLoadState::masked
        ));
        // Make error + not_found unhealthy too
        assert!(is_unit_unhealthy(
            SystemdUnitActiveState::deactivating,
            SystemdUnitLoadState::not_found
        ));
        assert!(is_unit_unhealthy(
            // Can never really be active here with error, but check we ignore it
            SystemdUnitActiveState::active,
            SystemdUnitLoadState::error,
        ));
    }

    #[test]
    fn test_iterators() {
        assert!(SystemdUnitActiveState::iter().collect::<Vec<_>>().len() > 0);
        assert!(SystemdUnitLoadState::iter().collect::<Vec<_>>().len() > 0);
    }

    #[test]
    fn test_active_state_from_str() {
        assert_eq!(
            SystemdUnitActiveState::from_str("active").unwrap(),
            SystemdUnitActiveState::active
        );
        assert_eq!(
            SystemdUnitActiveState::from_str("reloading").unwrap(),
            SystemdUnitActiveState::reloading
        );
        assert_eq!(
            SystemdUnitActiveState::from_str("inactive").unwrap(),
            SystemdUnitActiveState::inactive
        );
        assert_eq!(
            SystemdUnitActiveState::from_str("failed").unwrap(),
            SystemdUnitActiveState::failed
        );
        assert_eq!(
            SystemdUnitActiveState::from_str("activating").unwrap(),
            SystemdUnitActiveState::activating
        );
        assert_eq!(
            SystemdUnitActiveState::from_str("deactivating").unwrap(),
            SystemdUnitActiveState::deactivating
        );
        assert_eq!(
            SystemdUnitActiveState::from_str("unknown").unwrap(),
            SystemdUnitActiveState::unknown
        );
    }

    #[test]
    fn test_active_state_from_str_invalid() {
        assert!(SystemdUnitActiveState::from_str("invalid").is_err());
        assert!(SystemdUnitActiveState::from_str("").is_err());
        assert!(SystemdUnitActiveState::from_str("ACTIVE").is_err());
    }

    #[test]
    fn test_load_state_from_str() {
        assert_eq!(
            SystemdUnitLoadState::from_str("loaded").unwrap(),
            SystemdUnitLoadState::loaded
        );
        assert_eq!(
            SystemdUnitLoadState::from_str("error").unwrap(),
            SystemdUnitLoadState::error
        );
        assert_eq!(
            SystemdUnitLoadState::from_str("masked").unwrap(),
            SystemdUnitLoadState::masked
        );
        assert_eq!(
            SystemdUnitLoadState::from_str("not_found").unwrap(),
            SystemdUnitLoadState::not_found
        );
        assert_eq!(
            SystemdUnitLoadState::from_str("unknown").unwrap(),
            SystemdUnitLoadState::unknown
        );
    }

    #[test]
    fn test_load_state_from_str_invalid() {
        assert!(SystemdUnitLoadState::from_str("invalid").is_err());
        assert!(SystemdUnitLoadState::from_str("").is_err());
        assert!(SystemdUnitLoadState::from_str("LOADED").is_err());
    }

    #[test]
    fn test_active_state_display() {
        assert_eq!(format!("{}", SystemdUnitActiveState::active), "active");
        assert_eq!(
            format!("{}", SystemdUnitActiveState::reloading),
            "reloading"
        );
        assert_eq!(format!("{}", SystemdUnitActiveState::inactive), "inactive");
        assert_eq!(format!("{}", SystemdUnitActiveState::failed), "failed");
        assert_eq!(
            format!("{}", SystemdUnitActiveState::activating),
            "activating"
        );
        assert_eq!(
            format!("{}", SystemdUnitActiveState::deactivating),
            "deactivating"
        );
        assert_eq!(format!("{}", SystemdUnitActiveState::unknown), "unknown");
    }

    #[test]
    fn test_load_state_display() {
        assert_eq!(format!("{}", SystemdUnitLoadState::loaded), "loaded");
        assert_eq!(format!("{}", SystemdUnitLoadState::error), "error");
        assert_eq!(format!("{}", SystemdUnitLoadState::masked), "masked");
        assert_eq!(format!("{}", SystemdUnitLoadState::not_found), "not_found");
        assert_eq!(format!("{}", SystemdUnitLoadState::unknown), "unknown");
    }

    #[test]
    fn test_active_state_default() {
        let state: SystemdUnitActiveState = Default::default();
        assert_eq!(state, SystemdUnitActiveState::unknown);
    }

    #[test]
    fn test_load_state_default() {
        let state: SystemdUnitLoadState = Default::default();
        assert_eq!(state, SystemdUnitLoadState::unknown);
    }

    #[test]
    fn test_active_state_clone() {
        let state = SystemdUnitActiveState::active;
        let cloned = state.clone();
        assert_eq!(state, cloned);
    }

    #[test]
    fn test_load_state_clone() {
        let state = SystemdUnitLoadState::loaded;
        let cloned = state.clone();
        assert_eq!(state, cloned);
    }

    #[test]
    fn test_active_state_debug() {
        let state = SystemdUnitActiveState::active;
        let debug_str = format!("{:?}", state);
        assert!(debug_str.contains("active"));
    }

    #[test]
    fn test_load_state_debug() {
        let state = SystemdUnitLoadState::loaded;
        let debug_str = format!("{:?}", state);
        assert!(debug_str.contains("loaded"));
    }

    #[test]
    fn test_active_state_equality() {
        assert_eq!(
            SystemdUnitActiveState::active,
            SystemdUnitActiveState::active
        );
        assert_ne!(
            SystemdUnitActiveState::active,
            SystemdUnitActiveState::inactive
        );
    }

    #[test]
    fn test_load_state_equality() {
        assert_eq!(SystemdUnitLoadState::loaded, SystemdUnitLoadState::loaded);
        assert_ne!(SystemdUnitLoadState::loaded, SystemdUnitLoadState::masked);
    }

    #[test]
    fn test_active_state_int_enum() {
        assert_eq!(SystemdUnitActiveState::unknown as u8, 0);
        assert_eq!(SystemdUnitActiveState::active as u8, 1);
        assert_eq!(SystemdUnitActiveState::reloading as u8, 2);
        assert_eq!(SystemdUnitActiveState::inactive as u8, 3);
        assert_eq!(SystemdUnitActiveState::failed as u8, 4);
        assert_eq!(SystemdUnitActiveState::activating as u8, 5);
        assert_eq!(SystemdUnitActiveState::deactivating as u8, 6);
    }

    #[test]
    fn test_load_state_int_enum() {
        assert_eq!(SystemdUnitLoadState::unknown as u8, 0);
        assert_eq!(SystemdUnitLoadState::loaded as u8, 1);
        assert_eq!(SystemdUnitLoadState::error as u8, 2);
        assert_eq!(SystemdUnitLoadState::masked as u8, 3);
        assert_eq!(SystemdUnitLoadState::not_found as u8, 4);
    }

    #[test]
    fn test_active_state_from_int() {
        assert_eq!(
            SystemdUnitActiveState::try_from(0).unwrap(),
            SystemdUnitActiveState::unknown
        );
        assert_eq!(
            SystemdUnitActiveState::try_from(1).unwrap(),
            SystemdUnitActiveState::active
        );
        assert_eq!(
            SystemdUnitActiveState::try_from(2).unwrap(),
            SystemdUnitActiveState::reloading
        );
        assert_eq!(
            SystemdUnitActiveState::try_from(3).unwrap(),
            SystemdUnitActiveState::inactive
        );
        assert_eq!(
            SystemdUnitActiveState::try_from(4).unwrap(),
            SystemdUnitActiveState::failed
        );
        assert_eq!(
            SystemdUnitActiveState::try_from(5).unwrap(),
            SystemdUnitActiveState::activating
        );
        assert_eq!(
            SystemdUnitActiveState::try_from(6).unwrap(),
            SystemdUnitActiveState::deactivating
        );
    }

    #[test]
    fn test_load_state_from_int() {
        assert_eq!(
            SystemdUnitLoadState::try_from(0).unwrap(),
            SystemdUnitLoadState::unknown
        );
        assert_eq!(
            SystemdUnitLoadState::try_from(1).unwrap(),
            SystemdUnitLoadState::loaded
        );
        assert_eq!(
            SystemdUnitLoadState::try_from(2).unwrap(),
            SystemdUnitLoadState::error
        );
        assert_eq!(
            SystemdUnitLoadState::try_from(3).unwrap(),
            SystemdUnitLoadState::masked
        );
        assert_eq!(
            SystemdUnitLoadState::try_from(4).unwrap(),
            SystemdUnitLoadState::not_found
        );
    }

    #[test]
    fn test_active_state_from_int_invalid() {
        assert!(SystemdUnitActiveState::try_from(255).is_err());
        assert!(SystemdUnitActiveState::try_from(100).is_err());
    }

    #[test]
    fn test_load_state_from_int_invalid() {
        assert!(SystemdUnitLoadState::try_from(255).is_err());
        assert!(SystemdUnitLoadState::try_from(100).is_err());
    }

    #[test]
    fn test_is_unit_unhealthy_all_combinations() {
        // Test loaded state with all active states
        assert!(!is_unit_unhealthy(
            SystemdUnitActiveState::active,
            SystemdUnitLoadState::loaded
        ));
        assert!(is_unit_unhealthy(
            SystemdUnitActiveState::reloading,
            SystemdUnitLoadState::loaded
        ));
        assert!(is_unit_unhealthy(
            SystemdUnitActiveState::inactive,
            SystemdUnitLoadState::loaded
        ));
        assert!(is_unit_unhealthy(
            SystemdUnitActiveState::failed,
            SystemdUnitLoadState::loaded
        ));
        assert!(is_unit_unhealthy(
            SystemdUnitActiveState::activating,
            SystemdUnitLoadState::loaded
        ));
        assert!(is_unit_unhealthy(
            SystemdUnitActiveState::deactivating,
            SystemdUnitLoadState::loaded
        ));
        assert!(is_unit_unhealthy(
            SystemdUnitActiveState::unknown,
            SystemdUnitLoadState::loaded
        ));

        // Test masked state with all active states (always healthy)
        assert!(!is_unit_unhealthy(
            SystemdUnitActiveState::active,
            SystemdUnitLoadState::masked
        ));
        assert!(!is_unit_unhealthy(
            SystemdUnitActiveState::reloading,
            SystemdUnitLoadState::masked
        ));
        assert!(!is_unit_unhealthy(
            SystemdUnitActiveState::inactive,
            SystemdUnitLoadState::masked
        ));
        assert!(!is_unit_unhealthy(
            SystemdUnitActiveState::failed,
            SystemdUnitLoadState::masked
        ));
        assert!(!is_unit_unhealthy(
            SystemdUnitActiveState::activating,
            SystemdUnitLoadState::masked
        ));
        assert!(!is_unit_unhealthy(
            SystemdUnitActiveState::deactivating,
            SystemdUnitLoadState::masked
        ));
        assert!(!is_unit_unhealthy(
            SystemdUnitActiveState::unknown,
            SystemdUnitLoadState::masked
        ));

        // Test error state with all active states (always unhealthy)
        assert!(is_unit_unhealthy(
            SystemdUnitActiveState::active,
            SystemdUnitLoadState::error
        ));
        assert!(is_unit_unhealthy(
            SystemdUnitActiveState::reloading,
            SystemdUnitLoadState::error
        ));
        assert!(is_unit_unhealthy(
            SystemdUnitActiveState::inactive,
            SystemdUnitLoadState::error
        ));
        assert!(is_unit_unhealthy(
            SystemdUnitActiveState::failed,
            SystemdUnitLoadState::error
        ));
        assert!(is_unit_unhealthy(
            SystemdUnitActiveState::activating,
            SystemdUnitLoadState::error
        ));
        assert!(is_unit_unhealthy(
            SystemdUnitActiveState::deactivating,
            SystemdUnitLoadState::error
        ));
        assert!(is_unit_unhealthy(
            SystemdUnitActiveState::unknown,
            SystemdUnitLoadState::error
        ));

        // Test not_found state with all active states (always unhealthy)
        assert!(is_unit_unhealthy(
            SystemdUnitActiveState::active,
            SystemdUnitLoadState::not_found
        ));
        assert!(is_unit_unhealthy(
            SystemdUnitActiveState::reloading,
            SystemdUnitLoadState::not_found
        ));
        assert!(is_unit_unhealthy(
            SystemdUnitActiveState::inactive,
            SystemdUnitLoadState::not_found
        ));
        assert!(is_unit_unhealthy(
            SystemdUnitActiveState::failed,
            SystemdUnitLoadState::not_found
        ));
        assert!(is_unit_unhealthy(
            SystemdUnitActiveState::activating,
            SystemdUnitLoadState::not_found
        ));
        assert!(is_unit_unhealthy(
            SystemdUnitActiveState::deactivating,
            SystemdUnitLoadState::not_found
        ));
        assert!(is_unit_unhealthy(
            SystemdUnitActiveState::unknown,
            SystemdUnitLoadState::not_found
        ));

        // Test unknown state with all active states (always unhealthy)
        assert!(is_unit_unhealthy(
            SystemdUnitActiveState::active,
            SystemdUnitLoadState::unknown
        ));
        assert!(is_unit_unhealthy(
            SystemdUnitActiveState::reloading,
            SystemdUnitLoadState::unknown
        ));
        assert!(is_unit_unhealthy(
            SystemdUnitActiveState::inactive,
            SystemdUnitLoadState::unknown
        ));
        assert!(is_unit_unhealthy(
            SystemdUnitActiveState::failed,
            SystemdUnitLoadState::unknown
        ));
        assert!(is_unit_unhealthy(
            SystemdUnitActiveState::activating,
            SystemdUnitLoadState::unknown
        ));
        assert!(is_unit_unhealthy(
            SystemdUnitActiveState::deactivating,
            SystemdUnitLoadState::unknown
        ));
        assert!(is_unit_unhealthy(
            SystemdUnitActiveState::unknown,
            SystemdUnitLoadState::unknown
        ));
    }

    #[test]
    fn test_active_state_serialization() {
        let state = SystemdUnitActiveState::active;
        let serialized = serde_json::to_string(&state).unwrap();
        assert_eq!(serialized, "1");

        let deserialized: SystemdUnitActiveState = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized, state);
    }

    #[test]
    fn test_load_state_serialization() {
        let state = SystemdUnitLoadState::loaded;
        let serialized = serde_json::to_string(&state).unwrap();
        assert_eq!(serialized, "1");

        let deserialized: SystemdUnitLoadState = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized, state);
    }

    #[test]
    fn test_active_state_all_variants_serialization() {
        for state in SystemdUnitActiveState::iter() {
            let serialized = serde_json::to_string(&state).unwrap();
            let deserialized: SystemdUnitActiveState = serde_json::from_str(&serialized).unwrap();
            assert_eq!(deserialized, state);
        }
    }

    #[test]
    fn test_load_state_all_variants_serialization() {
        for state in SystemdUnitLoadState::iter() {
            let serialized = serde_json::to_string(&state).unwrap();
            let deserialized: SystemdUnitLoadState = serde_json::from_str(&serialized).unwrap();
            assert_eq!(deserialized, state);
        }
    }
}
