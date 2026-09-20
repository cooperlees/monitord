//! # varlink_fallback module
//!
//! The single enforcement point for `[varlink] no_fallback` (see #37).
//!
//! Every varlink-first collector funnels its failure through
//! [`report_varlink_failure`]: by default it logs the familiar `falling back`
//! warning and the caller proceeds to its D-Bus (or, for networkd,
//! file-based) fallback; with `[varlink] no_fallback = true` it returns an
//! error instead, so the collector fails loudly and CI proves the collector
//! set is varlink-clean rather than silently D-Bus-served.
//!
//! The `collector` argument names the collector for the log line (e.g.
//! `"units"`, `"networkd"`, `"container <name> units"`); `fallback` names
//! what would have served the data (`"D-Bus"`, `"file-based"`). Keeping both
//! as plain strings keeps this module dependency-free and lets container
//! paths interpolate the machine name without a structured type.

use thiserror::Error;

#[derive(Error, Debug)]
pub enum NoFallbackError {
    #[error(
        "varlink {collector} failed and [varlink] no_fallback=true forbids the {fallback} fallback: {source:?}"
    )]
    FallbackForbidden {
        collector: String,
        fallback: String,
        source: anyhow::Error,
    },
}

/// Record a varlink failure and decide whether the caller may fall back.
///
/// Returns `Ok(())` (after logging the `falling back` warning) by default,
/// so the caller proceeds to its fallback. Returns `Err` when `no_fallback`
/// is true, turning the failure into the collector's error — which
/// `stat_collector` logs per-collector and counts in `collector_timings`
/// `success`, so a no-fallback CI run fails visibly rather than earning a
/// quiet `varlink_usage.* = 0` gauge.
///
/// `source` is taken as `anyhow::Error` because every varlink call site
/// already produces one (sockets, protocol and version-gate failures all
/// funnel through `anyhow`).
pub fn report_varlink_failure(
    no_fallback: bool,
    collector: &str,
    fallback: &str,
    source: anyhow::Error,
) -> Result<(), NoFallbackError> {
    if no_fallback {
        return Err(NoFallbackError::FallbackForbidden {
            collector: collector.to_string(),
            fallback: fallback.to_string(),
            source,
        });
    }
    tracing::warn!(
        "Varlink {} failed, falling back to {}: {:?}",
        collector,
        fallback.to_lowercase(),
        source,
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fallback_allowed_by_default() {
        assert!(report_varlink_failure(false, "units", "D-Bus", anyhow::anyhow!("boom")).is_ok());
    }

    #[test]
    fn test_no_fallback_forbids_fallback_with_context() {
        let err = report_varlink_failure(
            true,
            "container demo units",
            "D-Bus",
            anyhow::anyhow!("boom"),
        )
        .expect_err("no_fallback must forbid the fallback");
        let rendered = err.to_string();
        assert!(
            rendered.contains("container demo units"),
            "error names the collector: {rendered}"
        );
        assert!(
            rendered.contains("D-Bus"),
            "error names the forbidden fallback: {rendered}"
        );
    }
}
