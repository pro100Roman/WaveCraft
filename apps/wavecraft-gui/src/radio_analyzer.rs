use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::thread::{self, JoinHandle};
use wavecraft_core::IqBatch;
use wavecraft_dsp::SpectrumProcessor;

pub use wavecraft_radio::{
    process_radio_iq, RadioAnalyzerRuntime, RadioAnalyzerTelemetry, RadioDataDecoderConfig,
    RadioDspOutput,
};

const RADIO_DSP_INPUT_CAPACITY: usize = 8;

struct RadioDspInput {
    iq: IqBatch,
    squelch_threshold_dbfs: f32,
}

/// Runs the device-independent radio pipeline away from the UI thread.
pub struct RadioDspWorker {
    sender: Option<SyncSender<RadioDspInput>>,
    receiver: Receiver<RadioDspOutput>,
    worker: Option<JoinHandle<()>>,
}

impl RadioDspWorker {
    pub fn start(
        mut spectrum: SpectrumProcessor,
        mut analyzer: Option<RadioAnalyzerRuntime>,
    ) -> Self {
        let (input_sender, input_receiver) =
            mpsc::sync_channel::<RadioDspInput>(RADIO_DSP_INPUT_CAPACITY);
        let (output_sender, output_receiver) = mpsc::channel();
        let worker = thread::spawn(move || {
            while let Ok(input) = input_receiver.recv() {
                let output = process_radio_iq(
                    &mut spectrum,
                    &mut analyzer,
                    &input.iq,
                    input.squelch_threshold_dbfs,
                );
                if output_sender.send(output).is_err() {
                    break;
                }
            }
        });
        Self {
            sender: Some(input_sender),
            receiver: output_receiver,
            worker: Some(worker),
        }
    }

    pub fn push(&self, iq: IqBatch, squelch_threshold_dbfs: f32) -> Result<(), String> {
        self.sender
            .as_ref()
            .ok_or_else(|| "radio DSP worker is stopped".to_owned())?
            .send(RadioDspInput {
                iq,
                squelch_threshold_dbfs,
            })
            .map_err(|_| "radio DSP worker disconnected".to_owned())
    }

    pub fn try_recv(&self) -> Result<RadioDspOutput, TryRecvError> {
        self.receiver.try_recv()
    }

    pub fn finish(&mut self) -> Result<Vec<RadioDspOutput>, String> {
        self.sender.take();
        if self
            .worker
            .take()
            .is_some_and(|worker| worker.join().is_err())
        {
            return Err("radio DSP worker panicked".to_owned());
        }
        Ok(self.receiver.try_iter().collect())
    }
}

impl Drop for RadioDspWorker {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wavecraft_core::Iq8;

    #[test]
    fn radio_dsp_worker_processes_all_queued_iq_before_finishing() {
        let spectrum = SpectrumProcessor::new(8, wavecraft_dsp::SpectrumWindow::Hann).unwrap();
        let mut worker = RadioDspWorker::start(spectrum, None);
        for first_sample_index in [0, 8] {
            worker
                .push(
                    IqBatch {
                        sample_rate_hz: 8_000,
                        center_frequency_hz: 100_000_000,
                        first_sample_index,
                        discontinuity: first_sample_index == 0,
                        samples: vec![Iq8 { i: 64, q: 0 }; 8],
                    },
                    -50.0,
                )
                .unwrap();
        }

        let outputs = worker.finish().unwrap();

        assert_eq!(outputs.len(), 2);
        assert!(outputs.iter().all(|output| output.spectrum.len() == 1));
        assert!(outputs.iter().all(|output| output.analyses.is_empty()));
        assert_eq!(outputs[0].spectrum[0].source_range, 0..8);
        assert_eq!(outputs[1].spectrum[0].source_range, 8..16);
    }
}
