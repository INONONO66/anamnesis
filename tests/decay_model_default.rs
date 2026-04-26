use anamnesis::api::{DecayModel, EngineConfig};

#[test]
fn decay_model_default_is_exponential() {
    let cfg = EngineConfig::default();
    assert!(matches!(cfg.decay_model, DecayModel::Exponential));
}

#[test]
fn decay_model_can_be_set_to_power_law() {
    let mut cfg = EngineConfig::default();
    cfg.decay_model = DecayModel::PowerLaw;
    assert!(matches!(cfg.decay_model, DecayModel::PowerLaw));
}
