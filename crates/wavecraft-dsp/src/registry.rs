use crate::{
    Demodulator, DspError, MAX_SOFT_SYMBOL_ALPHABET_SIZE, MAX_SPARSE_SYMBOL_ALPHABET_SIZE,
};
use std::any::Any;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DemodulatorOutputKind {
    SoftSymbols { alphabet_size: u16 },
    SparseSymbols { maximum_alphabet_size: u32 },
    Analog,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DemodulatorDescriptor {
    pub id: String,
    pub display_name: String,
    pub output: DemodulatorOutputKind,
}

impl DemodulatorDescriptor {
    fn validate(&self) -> Result<(), DspError> {
        if self.id.is_empty()
            || self.id.len() > 64
            || !self.id.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'.' | b'-' | b'_')
            })
        {
            return Err(DspError::new(
                "demodulator ID must contain 1 to 64 lowercase ASCII letters, digits, dots, dashes, or underscores",
            ));
        }
        if self.display_name.trim().is_empty() || self.display_name.len() > 128 {
            return Err(DspError::new(
                "demodulator display name must contain 1 to 128 characters",
            ));
        }
        match self.output {
            DemodulatorOutputKind::SoftSymbols { alphabet_size } => {
                if !(2..=MAX_SOFT_SYMBOL_ALPHABET_SIZE).contains(&alphabet_size) {
                    return Err(DspError::new(
                        "demodulator soft-symbol alphabet size must be between 2 and 256",
                    ));
                }
            }
            DemodulatorOutputKind::SparseSymbols {
                maximum_alphabet_size,
            } => {
                if !(2..=MAX_SPARSE_SYMBOL_ALPHABET_SIZE).contains(&maximum_alphabet_size) {
                    return Err(DspError::new(format!(
                        "demodulator sparse-symbol alphabet limit must be between 2 and {MAX_SPARSE_SYMBOL_ALPHABET_SIZE}"
                    )));
                }
            }
            DemodulatorOutputKind::Analog => {}
        }
        Ok(())
    }
}

/// Creates one configured demodulator for a stable registry ID.
///
/// Factories downcast the modulation-specific configuration supplied through
/// `Any`. Application-facing tagged profiles select the ID and retain the
/// corresponding typed configuration; unrelated modulation fields never enter
/// a shared settings structure.
pub trait DemodulatorFactory: Send + Sync {
    fn descriptor(&self) -> &DemodulatorDescriptor;

    fn create(&self, config: &dyn Any) -> Result<Box<dyn Demodulator>, DspError>;
}

#[derive(Default)]
pub struct DemodulatorRegistry {
    factories: BTreeMap<String, Box<dyn DemodulatorFactory>>,
}

impl DemodulatorRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_builtins() -> Result<Self, DspError> {
        let mut registry = Self::new();
        crate::fsk::register_builtin_fsk_demodulators(&mut registry)?;
        Ok(registry)
    }

    pub fn register(&mut self, factory: impl DemodulatorFactory + 'static) -> Result<(), DspError> {
        factory.descriptor().validate()?;
        let id = factory.descriptor().id.clone();
        if self.factories.contains_key(&id) {
            return Err(DspError::new(format!(
                "demodulator ID '{id}' is already registered"
            )));
        }
        self.factories.insert(id, Box::new(factory));
        Ok(())
    }

    pub fn descriptors(&self) -> impl Iterator<Item = &DemodulatorDescriptor> {
        self.factories.values().map(|factory| factory.descriptor())
    }

    pub fn descriptor(&self, id: &str) -> Option<&DemodulatorDescriptor> {
        self.factories.get(id).map(|factory| factory.descriptor())
    }

    pub fn create(&self, id: &str, config: &dyn Any) -> Result<Box<dyn Demodulator>, DspError> {
        let factory = self
            .factories
            .get(id)
            .ok_or_else(|| DspError::new(format!("unknown demodulator ID '{id}'")))?;
        factory.create(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FskConfig, FskModulation, FskPolarity, FskTiming};

    fn fsk_config(modulation: FskModulation) -> FskConfig {
        FskConfig {
            symbol_rate_hz: 100_000,
            deviation_hz: 50_000,
            modulation,
            polarity: FskPolarity::Normal,
            timing: FskTiming::Adaptive {
                loop_bandwidth: 0.1,
                max_clock_error_ppm: 10_000,
            },
        }
    }

    #[test]
    fn builtins_have_stable_sorted_ids_and_typed_outputs() {
        let registry = DemodulatorRegistry::with_builtins().unwrap();
        let descriptors = registry.descriptors().cloned().collect::<Vec<_>>();

        assert_eq!(
            descriptors
                .iter()
                .map(|descriptor| descriptor.id.as_str())
                .collect::<Vec<_>>(),
            ["fsk2", "gfsk2"]
        );
        assert!(descriptors.iter().all(|descriptor| {
            descriptor.output == DemodulatorOutputKind::SoftSymbols { alphabet_size: 2 }
        }));
    }

    #[test]
    fn creates_the_selected_builtin_and_rejects_mismatched_configuration() {
        let registry = DemodulatorRegistry::with_builtins().unwrap();

        assert!(registry
            .create("fsk2", &fsk_config(FskModulation::Fsk))
            .is_ok());
        assert!(registry
            .create("gfsk2", &fsk_config(FskModulation::Gfsk { bt: 0.5 }))
            .is_ok());
        assert!(registry
            .create("gfsk2", &fsk_config(FskModulation::Fsk))
            .is_err());
        assert!(registry
            .create("unknown", &fsk_config(FskModulation::Fsk))
            .is_err());
    }

    #[test]
    fn registration_rejects_duplicate_builtin_ids_without_replacing_them() {
        let mut registry = DemodulatorRegistry::with_builtins().unwrap();

        assert!(crate::fsk::register_builtin_fsk_demodulators(&mut registry).is_err());
        assert_eq!(registry.descriptors().count(), 2);
        assert_eq!(
            registry.descriptor("fsk2").unwrap().display_name,
            "Binary FSK"
        );
    }

    #[test]
    fn sparse_symbol_descriptors_support_lora_sized_alphabets() {
        let descriptor = DemodulatorDescriptor {
            id: "lora".to_owned(),
            display_name: "LoRa".to_owned(),
            output: DemodulatorOutputKind::SparseSymbols {
                maximum_alphabet_size: 4_096,
            },
        };

        assert!(descriptor.validate().is_ok());
    }
}
