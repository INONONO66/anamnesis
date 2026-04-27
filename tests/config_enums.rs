//! Tests for EnergyModel and SpreadingModel configuration enums.

use anamnesis::{EnergyModel, EngineConfig, SpreadingModel};

#[test]
fn test_energy_model_default() {
    let cfg = EngineConfig::default();
    assert!(matches!(cfg.energy_model, EnergyModel::WeightedSum));
}

#[test]
fn test_spreading_model_default() {
    let cfg = EngineConfig::default();
    assert!(matches!(
        cfg.spreading_model,
        SpreadingModel::PriorityQueueBfs
    ));
}
