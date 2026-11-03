//! UI-independent transport and custom-protocol decoding.

mod decode;
mod framing;
mod schema;

pub use decode::{
    ByteStatus, CustomProtocolDecoder, DecodeDiagnostic, DecodeError, DecodedField, DecodedMessage,
    DecodedValue, PacketCompleteness, SampleRange, Severity, TimedByte, TransportPacket,
    TransportValue,
};
pub use framing::{ByteStreamFramer, FramedBytes, FramingOutput};
pub use schema::{
    load_protocol_schema, BitDefinition, ByteMatch, ChecksumAlgorithm, ChecksumValidation,
    DisplayOptions, Endian, ExpectedValue, FieldKind, FieldSchema, FramingSchema, InputSchema,
    IntegerType, LengthField, MessageSchema, ProtocolMetadata, ProtocolSchema, ResyncStrategy,
    SchemaError, StringEncoding, TransportKind, PROTOCOL_SCHEMA_VERSION,
};
