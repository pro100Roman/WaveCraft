//! UI-independent transport and custom-protocol decoding.

mod decode;
mod framing;
mod radio;
mod runtime;
mod schema;
mod transport;

pub use decode::{
    ByteStatus, CustomProtocolDecoder, DecodeDiagnostic, DecodeError, DecodedField, DecodedMessage,
    DecodedValue, PacketCompleteness, SampleRange, Severity, TimedByte, TransportPacket,
    TransportValue,
};
pub use framing::{ByteStreamFramer, FramedBytes, FramingOutput};
pub use radio::{
    BitPolarity, BitTransformConfig, BitTransformOutput, BitTransformer, LineCode, TimedBit,
    TimedBitBatch,
};
pub use runtime::{CustomProtocolRuntime, ProtocolRuntimeOutput};
pub use schema::{
    load_protocol_schema, BitDefinition, ByteMatch, ChecksumAlgorithm, ChecksumValidation,
    DisplayOptions, Endian, ExpectedValue, FieldKind, FieldSchema, FramingSchema, InputSchema,
    IntegerType, LengthField, MessageSchema, ProtocolMetadata, ProtocolSchema, RecordStreamSchema,
    ResyncStrategy, SchemaError, SegmentedTransactionSchema, StringEncoding, TransportKind,
    PROTOCOL_SCHEMA_VERSION,
};
pub use transport::{
    decode_i2c_samples, decode_spi_samples, decode_uart_samples, I2cConfig, I2cDecoder, I2cEvent,
    SpiConfig, SpiDecoder, SpiTransactionEnd, SpiTransfer, TransportDecodeError, UartConfig,
    UartDecode, UartDecoder,
};
