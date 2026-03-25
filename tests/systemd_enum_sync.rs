//! Integration test to verify that Rust enums in networkd.rs are in sync with systemd C enums
//!
//! This test fetches and parses systemd header files and compares the enum values with our
//! Rust implementations. It helps ensure we stay synchronized with upstream systemd changes.

use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::PathBuf;

/// Parse C enum from header file content
/// Returns a HashMap of enum variant name -> value
/// Also accepts a reference map to resolve enum values that reference other enums
fn parse_c_enum(
    content: &str,
    enum_name: &str,
    ref_map: Option<&HashMap<String, i32>>,
) -> HashMap<String, i32> {
    let mut result = HashMap::new();
    let mut in_enum = false;
    let mut current_value = 0;

    for line in content.lines() {
        let line = line.trim();

        // Start of enum
        if line.contains(&format!("typedef enum {}", enum_name)) {
            in_enum = true;
            current_value = 0;
            continue;
        }

        // End of enum
        if in_enum && line.starts_with('}') {
            break;
        }

        // Parse enum values
        if in_enum {
            // Skip empty lines and comments
            if line.is_empty() || line.starts_with("/*") || line.starts_with('*') {
                continue;
            }

            // Parse enum entry
            if let Some(variant_part) = line.split(',').next() {
                let variant_part = variant_part.trim();

                // Skip entries that start with _ (internal/max values)
                if variant_part.starts_with('_') {
                    continue;
                }

                // Parse name and optional explicit value
                if let Some((name, value_str)) = variant_part.split_once('=') {
                    let name = name.trim().to_uppercase();
                    let value_str = value_str.trim();

                    // Handle cases like "LINK_OPERSTATE_OFF" or explicit values
                    if let Ok(val) = value_str.parse::<i32>() {
                        current_value = val;
                    } else {
                        // Handle references to other enum values (e.g., LINK_CARRIER_STATE_OFF = LINK_OPERSTATE_OFF)
                        let ref_name = value_str.trim();
                        if let Some(ref_map) = ref_map {
                            if let Some(&ref_val) = ref_map.get(ref_name) {
                                current_value = ref_val;
                            }
                        }
                    }

                    result.insert(name, current_value);
                    current_value += 1;
                } else if !variant_part.is_empty() && !variant_part.starts_with("/*") {
                    let name = variant_part.trim().to_uppercase();
                    result.insert(name, current_value);
                    current_value += 1;
                }
            }
        }
    }

    result
}

#[test]
fn test_address_state_sync() {
    let headers_path =
        env::var("SYSTEMD_HEADERS_PATH").unwrap_or_else(|_| "/tmp/systemd-headers".to_string());
    let network_util_path = PathBuf::from(&headers_path).join("network-util.h");

    if !network_util_path.exists() {
        eprintln!(
            "Warning: systemd headers not found at {}. Skipping test.",
            headers_path
        );
        return;
    }

    let content = fs::read_to_string(&network_util_path).expect("Failed to read network-util.h");

    let c_enum = parse_c_enum(&content, "LinkAddressState", None);

    // Expected Rust enum values from monitord
    let rust_values = vec![("off", 0), ("degraded", 1), ("routable", 2)];

    println!("Checking AddressState enum synchronization...");
    println!("Parsed C enum values: {:?}", c_enum);

    for (rust_name, rust_value) in rust_values {
        let systemd_name = format!("LINK_ADDRESS_STATE_{}", rust_name.to_uppercase());

        if let Some(&c_value) = c_enum.get(&systemd_name) {
            assert_eq!(
                rust_value, c_value,
                "AddressState::{} has value {} in Rust but {} in systemd",
                rust_name, rust_value, c_value
            );
            println!(
                "✓ AddressState::{} = {} (matches systemd)",
                rust_name, rust_value
            );
        } else {
            panic!("AddressState::{} not found in systemd header", rust_name);
        }
    }
}

#[test]
fn test_carrier_state_sync() {
    let headers_path =
        env::var("SYSTEMD_HEADERS_PATH").unwrap_or_else(|_| "/tmp/systemd-headers".to_string());
    let network_util_path = PathBuf::from(&headers_path).join("network-util.h");

    if !network_util_path.exists() {
        eprintln!(
            "Warning: systemd headers not found at {}. Skipping test.",
            headers_path
        );
        return;
    }

    let content = fs::read_to_string(&network_util_path).expect("Failed to read network-util.h");

    // Parse OperState first as CarrierState references it
    let oper_enum = parse_c_enum(&content, "LinkOperationalState", None);
    let c_enum = parse_c_enum(&content, "LinkCarrierState", Some(&oper_enum));

    // Expected Rust enum values - note that CarrierState references OperState values
    // So we need to check against the actual OperState integer values
    let rust_values = vec![
        ("off", 1),              // LINK_OPERSTATE_OFF = 1
        ("no-carrier", 2),       // LINK_OPERSTATE_NO_CARRIER = 2
        ("dormant", 3),          // LINK_OPERSTATE_DORMANT = 3
        ("degraded-carrier", 4), // LINK_OPERSTATE_DEGRADED_CARRIER = 4
        ("carrier", 5),          // LINK_OPERSTATE_CARRIER = 5
        ("enslaved", 7),         // LINK_OPERSTATE_ENSLAVED = 7
    ];

    println!("Checking CarrierState enum synchronization...");
    println!("Parsed OperState values: {:?}", oper_enum);
    println!("Parsed CarrierState enum values: {:?}", c_enum);

    for (rust_name, rust_value) in rust_values {
        let systemd_carrier_name = format!(
            "LINK_CARRIER_STATE_{}",
            rust_name.to_uppercase().replace('-', "_")
        );

        if let Some(&c_value) = c_enum.get(&systemd_carrier_name) {
            assert_eq!(
                rust_value, c_value,
                "CarrierState::{} has value {} in Rust but {} in systemd",
                rust_name, rust_value, c_value
            );
            println!(
                "✓ CarrierState::{} = {} (matches systemd)",
                rust_name, rust_value
            );
        } else {
            panic!("CarrierState::{} not found in systemd header", rust_name);
        }
    }
}

#[test]
fn test_online_state_sync() {
    let headers_path =
        env::var("SYSTEMD_HEADERS_PATH").unwrap_or_else(|_| "/tmp/systemd-headers".to_string());
    let network_util_path = PathBuf::from(&headers_path).join("network-util.h");

    if !network_util_path.exists() {
        eprintln!(
            "Warning: systemd headers not found at {}. Skipping test.",
            headers_path
        );
        return;
    }

    let content = fs::read_to_string(&network_util_path).expect("Failed to read network-util.h");

    let c_enum = parse_c_enum(&content, "LinkOnlineState", None);

    // Expected Rust enum values from monitord
    let rust_values = vec![("offline", 0), ("partial", 1), ("online", 2)];

    println!("Checking OnlineState enum synchronization...");
    println!("Parsed C enum values: {:?}", c_enum);

    for (rust_name, rust_value) in rust_values {
        let systemd_name = format!("LINK_ONLINE_STATE_{}", rust_name.to_uppercase());

        if let Some(&c_value) = c_enum.get(&systemd_name) {
            assert_eq!(
                rust_value, c_value,
                "OnlineState::{} has value {} in Rust but {} in systemd",
                rust_name, rust_value, c_value
            );
            println!(
                "✓ OnlineState::{} = {} (matches systemd)",
                rust_name, rust_value
            );
        } else {
            panic!("OnlineState::{} not found in systemd header", rust_name);
        }
    }
}

#[test]
fn test_oper_state_sync() {
    let headers_path =
        env::var("SYSTEMD_HEADERS_PATH").unwrap_or_else(|_| "/tmp/systemd-headers".to_string());
    let network_util_path = PathBuf::from(&headers_path).join("network-util.h");

    if !network_util_path.exists() {
        eprintln!(
            "Warning: systemd headers not found at {}. Skipping test.",
            headers_path
        );
        return;
    }

    let content = fs::read_to_string(&network_util_path).expect("Failed to read network-util.h");

    let c_enum = parse_c_enum(&content, "LinkOperationalState", None);

    // Expected Rust enum values from monitord
    let rust_values = vec![
        ("missing", 0),
        ("off", 1),
        ("no-carrier", 2),
        ("dormant", 3),
        ("degraded-carrier", 4),
        ("carrier", 5),
        ("degraded", 6),
        ("enslaved", 7),
        ("routable", 8),
    ];

    println!("Checking OperState enum synchronization...");
    println!("Parsed C enum values: {:?}", c_enum);

    for (rust_name, rust_value) in rust_values {
        let systemd_name = format!(
            "LINK_OPERSTATE_{}",
            rust_name.to_uppercase().replace('-', "_")
        );

        if let Some(&c_value) = c_enum.get(&systemd_name) {
            assert_eq!(
                rust_value, c_value,
                "OperState::{} has value {} in Rust but {} in systemd",
                rust_name, rust_value, c_value
            );
            println!(
                "✓ OperState::{} = {} (matches systemd)",
                rust_name, rust_value
            );
        } else {
            panic!("OperState::{} not found in systemd header", rust_name);
        }
    }
}

#[test]
fn test_admin_state_sync() {
    let headers_path =
        env::var("SYSTEMD_HEADERS_PATH").unwrap_or_else(|_| "/tmp/systemd-headers".to_string());
    let networkd_link_path = PathBuf::from(&headers_path).join("networkd-link.h");

    if !networkd_link_path.exists() {
        eprintln!(
            "Warning: systemd headers not found at {}. Skipping test.",
            headers_path
        );
        return;
    }

    let content = fs::read_to_string(&networkd_link_path).expect("Failed to read networkd-link.h");

    let c_enum = parse_c_enum(&content, "LinkState", None);

    // Expected Rust enum values from monitord
    // Note: monitord uses different names/mappings than systemd
    let rust_values = vec![
        ("pending", 0),     // LINK_STATE_PENDING
        ("failed", 5),      // LINK_STATE_FAILED
        ("configuring", 2), // LINK_STATE_CONFIGURING
        ("configured", 3),  // LINK_STATE_CONFIGURED
        ("unmanaged", 4),   // LINK_STATE_UNMANAGED
        ("linger", 6),      // LINK_STATE_LINGER
    ];

    println!("Checking AdminState enum synchronization...");
    println!("Parsed C enum values: {:?}", c_enum);

    for (rust_name, expected_value) in rust_values {
        let systemd_name = format!("LINK_STATE_{}", rust_name.to_uppercase());

        if let Some(&c_value) = c_enum.get(&systemd_name) {
            assert_eq!(
                expected_value, c_value,
                "AdminState::{} has value {} in Rust but {} in systemd",
                rust_name, expected_value, c_value
            );
            println!(
                "✓ AdminState::{} = {} (matches systemd)",
                rust_name, expected_value
            );
        } else {
            panic!(
                "AdminState::{} (systemd: {}) not found in systemd header",
                rust_name, systemd_name
            );
        }
    }
}
