//! UART transport and bounded, incremental telemetry decoding, independent of UI.

mod csv;
pub mod pd_power;
mod protocol;
pub use csv::{CsvDelimiter, CsvReader};
pub use protocol::*;
pub use serialport::{DataBits, FlowControl, Parity, StopBits};
use std::io::{ErrorKind, Read};
use std::time::Duration;
use wavecraft_core::{
    CaptureBatch, CaptureContext, CaptureError, CaptureSource, DeviceDescriptor, DeviceId,
    InstrumentKind,
};

pub fn available_ports() -> Result<Vec<String>, CaptureError> {
    serialport::available_ports()
        .map(|ports| ports.into_iter().map(|port| port.port_name).collect())
        .map_err(|error| CaptureError::new(format!("Cannot enumerate serial ports: {error}")))
}

#[derive(Clone, Debug)]
pub struct SerialConfiguration {
    pub port: String,
    pub baud_rate: u32,
    pub data_bits: DataBits,
    pub parity: Parity,
    pub stop_bits: StopBits,
    pub flow_control: FlowControl,
}

impl Default for SerialConfiguration {
    fn default() -> Self {
        Self {
            port: String::new(),
            baud_rate: 115_200,
            data_bits: DataBits::Eight,
            parity: Parity::None,
            stop_bits: StopBits::One,
            flow_control: FlowControl::None,
        }
    }
}

pub struct SerialSource(pub SerialConfiguration);

impl CaptureSource for SerialSource {
    fn descriptor(&self) -> DeviceDescriptor {
        DeviceDescriptor {
            id: DeviceId::new(format!("serial:{}", self.0.port)),
            name: self.0.port.clone(),
            kind: InstrumentKind::Serial,
        }
    }

    fn run(self: Box<Self>, context: CaptureContext) -> Result<(), CaptureError> {
        let configuration = &self.0;
        let port = serialport::new(&configuration.port, configuration.baud_rate)
            .data_bits(configuration.data_bits)
            .parity(configuration.parity)
            .stop_bits(configuration.stop_bits)
            .flow_control(configuration.flow_control)
            .timeout(Duration::from_millis(50))
            .open()
            .map_err(|error| {
                CaptureError::new(format!("Cannot open {}: {error}", configuration.port))
            })?;
        read_stream(port, context)
    }
}

fn read_stream(mut reader: impl Read, context: CaptureContext) -> Result<(), CaptureError> {
    context.synchronize_start()?;
    let mut buffer = [0; 4096];
    let mut sequence = 0;
    while !context.is_cancelled() {
        match reader.read(&mut buffer) {
            Ok(0) => return Err(CaptureError::new("Serial port closed by peer")),
            Ok(count) => {
                context.emit(
                    sequence,
                    context.elapsed(),
                    CaptureBatch::Serial(buffer[..count].to_vec()),
                )?;
                sequence += 1;
            }
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::TimedOut | ErrorKind::WouldBlock | ErrorKind::Interrupted
                ) => {}
            Err(error) => return Err(CaptureError::new(format!("Serial read failed: {error}"))),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, VecDeque};
    use std::io;
    use std::time::Instant;
    use wavecraft_core::{CaptureCoordinator, CaptureEvent};

    struct Reader {
        chunks: VecDeque<io::Result<Vec<u8>>>,
    }
    impl Read for Reader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            match self.chunks.pop_front() {
                Some(chunk) => {
                    let bytes = chunk?;
                    buffer[..bytes.len()].copy_from_slice(&bytes);
                    Ok(bytes.len())
                }
                None => {
                    std::thread::sleep(Duration::from_millis(1));
                    Err(ErrorKind::TimedOut.into())
                }
            }
        }
    }
    struct Source {
        id: &'static str,
        reader: Reader,
    }
    impl CaptureSource for Source {
        fn descriptor(&self) -> DeviceDescriptor {
            DeviceDescriptor {
                id: DeviceId::new(self.id),
                name: self.id.into(),
                kind: InstrumentKind::Serial,
            }
        }
        fn run(self: Box<Self>, context: CaptureContext) -> Result<(), CaptureError> {
            read_stream(self.reader, context)
        }
    }

    #[test]
    fn coordinates_two_serial_sources_preserves_bytes_and_order_and_cancels() {
        let sources: Vec<Box<dyn CaptureSource>> = ["uart1", "uart2"]
            .into_iter()
            .map(|id| {
                Box::new(Source {
                    id,
                    reader: Reader {
                        chunks: VecDeque::from([
                            Ok(vec![0, 0xff]),
                            Err(ErrorKind::TimedOut.into()),
                            Ok(vec![1, 2]),
                        ]),
                    },
                }) as Box<dyn CaptureSource>
            })
            .collect();
        let session = CaptureCoordinator::start(sources).unwrap();
        let mut received = BTreeMap::<String, Vec<_>>::new();
        for _ in 0..4 {
            let CaptureEvent::Frame(frame) = session.recv_timeout(Duration::from_secs(2)).unwrap()
            else {
                panic!("expected bytes");
            };
            received
                .entry(frame.device_id.to_string())
                .or_default()
                .push(frame);
        }
        for frames in received.values() {
            assert_eq!(frames.len(), 2);
            assert_eq!(frames[0].sequence, 0);
            assert_eq!(frames[1].sequence, 1);
            assert!(frames[1].captured_at >= frames[0].captured_at);
            assert_eq!(frames[0].payload, CaptureBatch::Serial(vec![0, 0xff]));
            assert_eq!(frames[1].payload, CaptureBatch::Serial(vec![1, 2]));
        }
        let before = Instant::now();
        session.stop().unwrap();
        assert!(before.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn propagates_read_failure_and_peer_disconnect() {
        for result in [
            Err(io::Error::new(ErrorKind::BrokenPipe, "unplugged")),
            Ok(vec![]),
        ] {
            let session = CaptureCoordinator::start(vec![Box::new(Source {
                id: "uart",
                reader: Reader {
                    chunks: VecDeque::from([result]),
                },
            })])
            .unwrap();
            let CaptureEvent::SourceFailed { error, .. } =
                session.recv_timeout(Duration::from_secs(2)).unwrap()
            else {
                panic!("expected read error");
            };
            assert!(error.to_string().contains("Serial"));
            session.stop().unwrap();
        }
    }
}
