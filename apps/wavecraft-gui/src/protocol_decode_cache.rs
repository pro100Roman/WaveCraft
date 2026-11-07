//! Cache of protocol decoding results for the displayed capture, so painting
//! does not decode again until the capture, analyzers, or protocols change.

use crate::{
    decode_protocol_samples, decode_transport_samples, Arc, ProtocolAnalyzerInstance,
    ProtocolDecodeOutcome, ProtocolDecodeSnapshot, WaveformStore,
};

#[derive(Debug)]
pub(crate) struct ProtocolDecodeCacheKey {
    pub(crate) waveform_revision: u64,
    pub(crate) sample_offset: u64,
    pub(crate) sample_count: usize,
    pub(crate) analyzers: Vec<ProtocolAnalyzerInstance>,
}

impl ProtocolDecodeCacheKey {
    /// Compares in place so a cache hit allocates nothing; the key is only built
    /// when the inputs change.
    pub(crate) fn matches(
        &self,
        waveforms: &WaveformStore,
        analyzers: &[ProtocolAnalyzerInstance],
    ) -> bool {
        self.waveform_revision == waveforms.logic_revision
            && self.sample_offset == waveforms.logic_sample_offset
            && self.sample_count == waveforms.logic_sample_count
            && self.analyzers == analyzers
    }
}

#[derive(Default)]
pub(crate) struct ProtocolDecodeCache {
    pub(crate) key: Option<ProtocolDecodeCacheKey>,
    pub(crate) outcomes: Arc<Vec<ProtocolDecodeOutcome>>,
    pub(crate) waveform_outcomes: Arc<Vec<ProtocolDecodeOutcome>>,
}

impl ProtocolDecodeCache {
    #[cfg(test)]
    pub(crate) fn outcomes<'a>(
        &'a mut self,
        waveforms: &WaveformStore,
        analyzers: &[ProtocolAnalyzerInstance],
        streamed: Option<&'a ProtocolDecodeSnapshot>,
    ) -> &'a [ProtocolDecodeOutcome] {
        if let Some(streamed) = streamed.filter(|decoder| decoder.matches_analyzers(analyzers)) {
            return streamed.outcomes();
        }
        self.refresh(waveforms, analyzers);
        &self.outcomes
    }

    /// Like [`Self::outcomes`], but returns a shared handle so the caller can
    /// keep the result while mutably borrowing the cache again, without copying
    /// every decoded event.
    pub(crate) fn shared_outcomes(
        &mut self,
        waveforms: &WaveformStore,
        analyzers: &[ProtocolAnalyzerInstance],
        streamed: Option<&ProtocolDecodeSnapshot>,
    ) -> Arc<Vec<ProtocolDecodeOutcome>> {
        if let Some(streamed) = streamed.filter(|decoder| decoder.matches_analyzers(analyzers)) {
            return streamed.shared_outcomes();
        }
        self.refresh(waveforms, analyzers);
        Arc::clone(&self.outcomes)
    }

    pub(crate) fn waveform_outcomes<'a>(
        &'a mut self,
        waveforms: &WaveformStore,
        analyzers: &[ProtocolAnalyzerInstance],
        streamed: Option<&'a ProtocolDecodeSnapshot>,
    ) -> &'a [ProtocolDecodeOutcome] {
        if let Some(streamed) = streamed.filter(|decoder| decoder.matches_analyzers(analyzers)) {
            return streamed.waveform_outcomes();
        }
        self.refresh(waveforms, analyzers);
        &self.waveform_outcomes
    }

    pub(crate) fn refresh(
        &mut self,
        waveforms: &WaveformStore,
        analyzers: &[ProtocolAnalyzerInstance],
    ) {
        if self
            .key
            .as_ref()
            .is_some_and(|key| key.matches(waveforms, analyzers))
        {
            return;
        }
        if waveforms.logic_sample_stride != 1
            || waveforms
                .logic_samples
                .iter()
                .any(|bucket| bucket.sample_count != 1)
        {
            let unavailable = Arc::new(
                analyzers
                    .iter()
                    .map(|_| {
                        ProtocolDecodeOutcome::Unavailable(
                            "Raw samples are unavailable for this display capture".to_owned(),
                        )
                    })
                    .collect::<Vec<_>>(),
            );
            self.waveform_outcomes = Arc::clone(&unavailable);
            self.outcomes = unavailable;
        } else {
            let samples = waveforms
                .logic_samples
                .iter()
                .map(|bucket| bucket.first)
                .collect::<Vec<_>>();
            self.outcomes = Arc::new(
                analyzers
                    .iter()
                    .map(|analyzer| {
                        decode_protocol_samples(
                            analyzer,
                            &samples,
                            waveforms.logic_sample_offset,
                            waveforms.logic_rate_hz,
                        )
                    })
                    .collect(),
            );
            self.waveform_outcomes = Arc::new(
                analyzers
                    .iter()
                    .map(|analyzer| {
                        decode_transport_samples(
                            analyzer,
                            &samples,
                            waveforms.logic_sample_offset,
                            waveforms.logic_rate_hz,
                        )
                    })
                    .collect(),
            );
        }
        self.key = Some(ProtocolDecodeCacheKey {
            waveform_revision: waveforms.logic_revision,
            sample_offset: waveforms.logic_sample_offset,
            sample_count: waveforms.logic_sample_count,
            analyzers: analyzers.to_vec(),
        });
    }
}
