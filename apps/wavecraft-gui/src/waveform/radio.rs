use std::collections::VecDeque;
use wavecraft_dsp::SpectrumFrame;

const DEFAULT_WATERFALL_CAPACITY: usize = 256;

/// Bounded derived state for the RF spectrum and waterfall views.
#[derive(Debug)]
pub struct RadioDisplayStore {
    newest_spectrum: Option<SpectrumFrame>,
    waterfall: VecDeque<SpectrumFrame>,
    waterfall_capacity: usize,
    dropped_rows: u64,
}

impl Default for RadioDisplayStore {
    fn default() -> Self {
        Self {
            newest_spectrum: None,
            waterfall: VecDeque::with_capacity(DEFAULT_WATERFALL_CAPACITY),
            waterfall_capacity: DEFAULT_WATERFALL_CAPACITY,
            dropped_rows: 0,
        }
    }
}

impl RadioDisplayStore {
    pub fn new(waterfall_capacity: usize) -> Result<Self, String> {
        if waterfall_capacity == 0 {
            return Err("waterfall capacity must be greater than zero".to_owned());
        }
        Ok(Self {
            newest_spectrum: None,
            waterfall: VecDeque::with_capacity(waterfall_capacity),
            waterfall_capacity,
            dropped_rows: 0,
        })
    }

    pub fn ingest(&mut self, frame: SpectrumFrame) {
        self.newest_spectrum = Some(frame.clone());
        if self.waterfall.len() == self.waterfall_capacity {
            self.waterfall.pop_front();
            self.dropped_rows = self.dropped_rows.saturating_add(1);
        }
        self.waterfall.push_back(frame);
    }

    pub fn clear_for_retune(&mut self) {
        self.newest_spectrum = None;
        self.waterfall.clear();
    }

    pub fn newest_spectrum(&self) -> Option<&SpectrumFrame> {
        self.newest_spectrum.as_ref()
    }

    pub fn waterfall(&self) -> &VecDeque<SpectrumFrame> {
        &self.waterfall
    }

    pub fn dropped_rows(&self) -> u64 {
        self.dropped_rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(index: u64, frequency: u64) -> SpectrumFrame {
        SpectrumFrame {
            source_range: index..index + 8,
            center_frequency_hz: frequency,
            bin_width_hz: 1_000.0,
            power_dbfs: vec![-80.0; 8],
        }
    }

    #[test]
    fn waterfall_keeps_only_the_newest_bounded_rows() {
        let mut store = RadioDisplayStore::new(2).unwrap();
        store.ingest(frame(0, 100_000_000));
        store.ingest(frame(8, 100_000_000));
        store.ingest(frame(16, 100_000_000));

        assert_eq!(store.waterfall().len(), 2);
        assert_eq!(store.waterfall()[0].source_range, 8..16);
        assert_eq!(store.newest_spectrum().unwrap().source_range, 16..24);
        assert_eq!(store.dropped_rows(), 1);
    }

    #[test]
    fn retune_clears_rows_that_no_longer_share_a_frequency_axis() {
        let mut store = RadioDisplayStore::new(4).unwrap();
        store.ingest(frame(0, 100_000_000));
        store.clear_for_retune();

        assert!(store.waterfall().is_empty());
        assert!(store.newest_spectrum().is_none());
    }
}
