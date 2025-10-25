//! Helpers for overriding network activation heights at runtime.

use std::sync::OnceLock;

use tracing::warn;
use zcash_protocol::consensus::{self, BlockHeight};

static SAPLING_MANUAL_OVERRIDE: OnceLock<Option<BlockHeight>> = OnceLock::new();
static SAPLING_ENV_OVERRIDE: OnceLock<Option<BlockHeight>> = OnceLock::new();
static ORCHARD_MANUAL_OVERRIDE: OnceLock<Option<BlockHeight>> = OnceLock::new();
static ORCHARD_ENV_OVERRIDE: OnceLock<Option<BlockHeight>> = OnceLock::new();

fn parse_override(var_name: &str) -> Option<BlockHeight> {
    match std::env::var(var_name) {
        Ok(value) => match value.parse::<u32>() {
            Ok(height) => Some(BlockHeight::from_u32(height)),
            Err(_) => {
                warn!("Ignoring invalid value '{value}' for {var_name}");
                None
            }
        },
        Err(std::env::VarError::NotPresent) => None,
        Err(error) => {
            warn!("Unable to read {var_name}: {error}");
            None
        }
    }
}

fn sapling_override() -> Option<BlockHeight> {
    if let Some(Some(height)) = SAPLING_MANUAL_OVERRIDE.get().copied() {
        return Some(height);
    }

    SAPLING_ENV_OVERRIDE
        .get_or_init(|| parse_override("ZINGO_SAPLING_ACTIVATION_HEIGHT"))
        .clone()
}

fn orchard_override() -> Option<BlockHeight> {
    if let Some(Some(height)) = ORCHARD_MANUAL_OVERRIDE.get().copied() {
        return Some(height);
    }

    ORCHARD_ENV_OVERRIDE
        .get_or_init(|| parse_override("ZINGO_ORCHARD_ACTIVATION_HEIGHT"))
        .clone()
}

/// Returns the activation height for Sapling, applying runtime overrides when present.
pub(crate) fn sapling_activation_height(
    consensus_parameters: &impl consensus::Parameters,
) -> BlockHeight {
    sapling_override().unwrap_or_else(|| {
        consensus_parameters
            .activation_height(consensus::NetworkUpgrade::Sapling)
            .expect("sapling activation height should always return Some")
    })
}

/// Returns the activation height for NU5 (Orchard), applying runtime overrides when present.
pub(crate) fn orchard_activation_height(
    consensus_parameters: &impl consensus::Parameters,
) -> BlockHeight {
    orchard_override().unwrap_or_else(|| {
        consensus_parameters
            .activation_height(consensus::NetworkUpgrade::Nu5)
            .expect("nu5 activation height should always return Some")
    })
}

/// Sets a runtime override for the Sapling activation height.
pub fn set_sapling_activation_height(height: BlockHeight) {
    if let Some(existing) = SAPLING_MANUAL_OVERRIDE.get().copied().flatten() {
        if existing != height {
            warn!(
                "Sapling activation height override already set to {:?}, ignoring new value {:?}",
                existing, height
            );
        }
        return;
    }

    if let Some(env_height) = SAPLING_ENV_OVERRIDE
        .get_or_init(|| parse_override("ZINGO_SAPLING_ACTIVATION_HEIGHT"))
        .clone()
    {
        if env_height != height {
            warn!(
                "Environment override for sapling activation height ({env_height}) present; ignoring server value {height}"
            );
        }
        return;
    }

    if SAPLING_MANUAL_OVERRIDE.set(Some(height)).is_err() {
        warn!("Failed to set sapling activation height override");
    }
}

/// Sets a runtime override for the NU5 (Orchard) activation height.
pub fn set_orchard_activation_height(height: BlockHeight) {
    if let Some(existing) = ORCHARD_MANUAL_OVERRIDE.get().copied().flatten() {
        if existing != height {
            warn!(
                "Orchard activation height override already set to {:?}, ignoring new value {:?}",
                existing, height
            );
        }
        return;
    }

    if let Some(env_height) = ORCHARD_ENV_OVERRIDE
        .get_or_init(|| parse_override("ZINGO_ORCHARD_ACTIVATION_HEIGHT"))
        .clone()
    {
        if env_height != height {
            warn!(
                "Environment override for orchard activation height ({env_height}) present; ignoring server value {height}"
            );
        }
        return;
    }

    if ORCHARD_MANUAL_OVERRIDE.set(Some(height)).is_err() {
        warn!("Failed to set orchard activation height override");
    }
}
