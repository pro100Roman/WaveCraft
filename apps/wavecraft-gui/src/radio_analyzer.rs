use wavecraft_core::IqBatch;
use wavecraft_decode::{
    RawRadioConfig, RawRadioDecoder, RawRadioOutput, SampleRange, TimedBit, TimedBitBatch,
};
use wavecraft_dsp::{
    DemodulatedBatch, DemodulatorRegistry, SignalAnalyzer, SignalAnalyzerProfile,
    TimedSoftSymbolBatch,
};

pub struct RadioAnalyzerOutput {
    pub demodulated: DemodulatedBatch,
    pub raw: RawRadioOutput,
}

/// Composition of the configured signal analyzer and raw-byte acquisition.
pub struct RadioAnalyzerRuntime {
    signal: SignalAnalyzer,
    raw: RawRadioDecoder,
}

impl RadioAnalyzerRuntime {
    pub fn new(profile: SignalAnalyzerProfile, raw_config: RawRadioConfig) -> Result<Self, String> {
        let registry = DemodulatorRegistry::with_builtins()
            .map_err(|error| format!("failed to initialize demodulators: {error}"))?;
        let signal = SignalAnalyzer::new(profile, &registry)
            .map_err(|error| format!("invalid radio analyzer profile: {error}"))?;
        let raw = RawRadioDecoder::new(raw_config).map_err(|error| error.to_string())?;
        Ok(Self { signal, raw })
    }

    pub fn push(&mut self, input: &IqBatch) -> Result<RadioAnalyzerOutput, String> {
        let demodulated = self.signal.push(input).map_err(|error| error.to_string())?;
        let DemodulatedBatch::SoftSymbols(symbols) = &demodulated else {
            return Err("raw radio bytes require digital soft-symbol output".to_owned());
        };
        let bits = soft_symbols_to_bits(symbols)?;
        let raw = self.raw.push(&bits).map_err(|error| error.to_string())?;
        Ok(RadioAnalyzerOutput { demodulated, raw })
    }
}

fn soft_symbols_to_bits(symbols: &TimedSoftSymbolBatch) -> Result<TimedBitBatch, String> {
    symbols.validate().map_err(|error| error.to_string())?;
    if symbols.alphabet_size != 2 {
        return Err(format!(
            "raw binary decoding requires a two-symbol alphabet, got {}",
            symbols.alphabet_size
        ));
    }
    Ok(TimedBitBatch {
        discontinuity: symbols.discontinuity,
        bits: symbols
            .symbols
            .iter()
            .map(|symbol| TimedBit {
                value: symbol.decision() == Some(1),
                confidence: symbol.confidence().unwrap_or(0.0),
                range: SampleRange {
                    start: symbol.source_range.start,
                    end: symbol.source_range.end,
                },
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wavecraft_dsp::TimedSoftSymbol;

    #[test]
    fn maps_binary_soft_symbols_to_timed_bits() {
        let bits = soft_symbols_to_bits(&TimedSoftSymbolBatch {
            alphabet_size: 2,
            discontinuity: true,
            symbols: vec![
                TimedSoftSymbol {
                    likelihoods: vec![0.9, 0.1],
                    source_range: 10..20,
                },
                TimedSoftSymbol {
                    likelihoods: vec![0.2, 0.8],
                    source_range: 20..30,
                },
            ],
        })
        .unwrap();

        assert!(bits.discontinuity);
        assert_eq!(
            bits.bits.iter().map(|bit| bit.value).collect::<Vec<_>>(),
            [false, true]
        );
        assert_eq!(bits.bits[1].range, SampleRange { start: 20, end: 30 });
        assert!((bits.bits[1].confidence - 0.8).abs() < f32::EPSILON);
    }

    #[test]
    fn rejects_non_binary_soft_symbols() {
        let error = soft_symbols_to_bits(&TimedSoftSymbolBatch {
            alphabet_size: 4,
            discontinuity: false,
            symbols: vec![],
        })
        .unwrap_err();

        assert!(error.contains("two-symbol alphabet"));
    }
}
