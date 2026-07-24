package org.facet.vox;

import java.math.BigInteger;
import java.util.Arrays;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import org.facet.phon.CompatibilityPlan;
import org.facet.phon.PhonCodec;
import org.facet.phon.PhonException;
import org.facet.phon.PhonLimits;
import org.facet.phon.SchemaClosure;
import org.facet.phon.SchemaId;
import org.facet.phon.Value;
import org.facet.vox.generated.HandshakeWireSchemas;
import org.facet.vox.generated.MessageWireSchemas;

/** Schema-driven codec for the first Java TCP wire slice. */
final class WireCodec {
    enum Direction { ARGS, RESPONSE }

    private final PhonLimits limits;
    private final SchemaClosure handshakeSchema;
    private final SchemaClosure messageSchema;
    private SchemaClosure peerMessageSchema;
    private CompatibilityPlan peerMessagePlan;

    WireCodec(ConnectionOptions options) throws VoxException {
        limits = new PhonLimits(
                options.maxFrameBytes(),
                options.maxSchemaBytes(),
                128,
                1_000_000,
                options.maxFrameBytes(),
                options.maxSchemas(),
                1_000_000);
        try {
            handshakeSchema = SchemaClosure.fromCanonicalBytes(
                    SchemaId.fromLong(HandshakeWireSchemas.HANDSHAKE_MESSAGE_SCHEMA_ID),
                    HandshakeWireSchemas.CANONICAL_SCHEMAS,
                    limits);
            messageSchema = SchemaClosure.fromCanonicalBytes(
                    SchemaId.fromLong(MessageWireSchemas.MESSAGE_SCHEMA_ID),
                    MessageWireSchemas.CANONICAL_SCHEMAS,
                    limits);
        } catch (PhonException failure) {
            throw new VoxException("generated Java wire schemas are invalid", failure);
        }
    }

    byte[] localMessageSchemaBytes() throws VoxException {
        try {
            return messageSchema.bundleBytes();
        } catch (PhonException failure) {
            throw new VoxException("cannot encode local Message schema closure", failure);
        }
    }

    void bindPeerMessageSchema(byte[] bundle) throws VoxException {
        try {
            SchemaClosure writer = SchemaClosure.fromBundleBytes(bundle, limits);
            CompatibilityPlan plan =
                    CompatibilityPlan.plan(writer, messageSchema, limits);
            peerMessageSchema = writer;
            peerMessagePlan = plan;
        } catch (PhonException failure) {
            throw new VoxException("peer Message schema is incompatible", failure);
        }
    }

    byte[] encodeHello(boolean odd) throws VoxException {
        Value settings = connectionSettings(odd);
        return encodeSelfDescribing(enumValue("Hello", map(
                "parity", parity(odd),
                "connection_settings", settings,
                "message_payload_schema", byteList(localMessageSchemaBytes()),
                "metadata", Value.nullValue())));
    }

    byte[] encodeHelloYourself(boolean odd) throws VoxException {
        return encodeSelfDescribing(enumValue("HelloYourself", map(
                "connection_settings", connectionSettings(odd),
                "message_payload_schema", byteList(localMessageSchemaBytes()),
                "metadata", Value.nullValue())));
    }

    byte[] encodeLetsGo() throws VoxException {
        return encodeSelfDescribing(enumValue("LetsGo", Value.map(Map.of())));
    }

    Value decodeHandshake(byte[] frame) throws VoxException {
        if (frame.length < 4) throw new VoxException("truncated self-describing handshake");
        long schemaLength = u32(frame, 0);
        if (schemaLength > limits.schemaBytes() || schemaLength > frame.length - 4L) {
            throw new VoxException("invalid self-describing handshake schema length "
                    + schemaLength);
        }
        int split = 4 + (int) schemaLength;
        byte[] schemaBytes = Arrays.copyOfRange(frame, 4, split);
        byte[] valueBytes = Arrays.copyOfRange(frame, split, frame.length);
        try {
            SchemaClosure writer = SchemaClosure.fromBundleBytes(schemaBytes, limits);
            return PhonCodec.decodeCompatibleValue(
                    writer, handshakeSchema, valueBytes, limits);
        } catch (PhonException failure) {
            throw new VoxException("cannot decode self-describing handshake", failure);
        }
    }

    byte[] encodeMessage(long laneId, Value payload) throws VoxException {
        try {
            return PhonCodec.encodeValue(
                    messageSchema,
                    map("lane_id", unsigned(laneId), "payload", payload),
                    limits);
        } catch (PhonException failure) {
            throw new VoxException("cannot encode compact Message", failure);
        }
    }

    Value decodeMessage(byte[] frame) throws VoxException {
        if (peerMessageSchema == null || peerMessagePlan == null) {
            throw new VoxException("peer Message schema is not bound");
        }
        try {
            Value writer = PhonCodec.decodeValue(peerMessageSchema, frame, limits);
            return peerMessagePlan.translate(writer);
        } catch (PhonException failure) {
            throw new VoxException("cannot decode compact Message", failure);
        }
    }

    Value laneOpen(String service, boolean odd) {
        Map<String, Value> metadata = new LinkedHashMap<>();
        metadata.put("vox-service", Value.string(service));
        return enumValue("LaneOpen", map(
                "connection_settings", connectionSettings(odd),
                "metadata", Value.map(metadata)));
    }

    Value laneAccept(boolean odd) {
        return enumValue("LaneAccept", map(
                "connection_settings", connectionSettings(odd),
                "metadata", Value.nullValue()));
    }

    Value schemaMessage(long methodId, Direction direction, byte[] schemas) {
        return enumValue("SchemaMessage", map(
                "method_id", unsigned(methodId),
                "direction", enumValue(
                        direction == Direction.ARGS ? "Args" : "Response",
                        Value.nullValue()),
                "schemas", byteList(schemas)));
    }

    Value requestCall(
            long requestId, long methodId, byte[] arguments) {
        Value call = map(
                "method_id", unsigned(methodId),
                "channels", Value.list(List.of()),
                "metadata", Value.nullValue(),
                "args", Value.bytes(arguments),
                "schemas", Value.list(List.of()));
        return enumValue("RequestMessage", map(
                "id", unsigned(requestId),
                "body", enumValue("Call", call)));
    }

    Value laneClose() {
        return enumValue("LaneClose", map("metadata", Value.nullValue()));
    }

    Value requestCancel(long requestId) {
        return enumValue("RequestMessage", map(
                "id", unsigned(requestId),
                "body", enumValue("Cancel", map("metadata", Value.nullValue()))));
    }

    byte[] transcode(
            SchemaClosure writer, SchemaClosure reader, byte[] payload) throws VoxException {
        try {
            return PhonCodec.transcode(writer, reader, payload, limits);
        } catch (PhonException failure) {
            throw new VoxException("method payload schema is incompatible", failure);
        }
    }

    SchemaClosure parseBinding(byte[] bundle) throws VoxException {
        try {
            return SchemaClosure.fromBundleBytes(bundle, limits);
        } catch (PhonException failure) {
            throw new VoxException("invalid method schema binding", failure);
        }
    }

    static long laneId(Value message) throws VoxException {
        return required(message, "lane_id").asInteger().longValue();
    }

    static Value payload(Value message) throws VoxException {
        return required(message, "payload");
    }

    static String variant(Value value) throws VoxException {
        if (value.type() != Value.Type.ENUM) throw new VoxException("expected enum value");
        return value.asEnum().variant();
    }

    static Value variantPayload(Value value) throws VoxException {
        if (value.type() != Value.Type.ENUM) throw new VoxException("expected enum value");
        return value.asEnum().payload();
    }

    static Value required(Value value, String field) throws VoxException {
        if (value.type() != Value.Type.MAP) throw new VoxException("expected record value");
        Value result = value.asMap().get(field);
        if (result == null) throw new VoxException("missing wire field " + field);
        return result;
    }

    static long unsignedLong(Value value) throws VoxException {
        if (value.type() != Value.Type.UNSIGNED) {
            throw new VoxException("expected unsigned wire integer");
        }
        return value.asInteger().longValue();
    }

    static byte[] byteList(Value value) throws VoxException {
        if (value.type() != Value.Type.LIST) {
            throw new VoxException("expected wire byte list");
        }
        byte[] result = new byte[value.asList().size()];
        for (int index = 0; index < result.length; index++) {
            Value item = value.asList().get(index);
            if (item.type() != Value.Type.UNSIGNED
                    || item.asInteger().signum() < 0
                    || item.asInteger().compareTo(BigInteger.valueOf(255)) > 0) {
                throw new VoxException("wire byte list contains non-u8 value");
            }
            result[index] = item.asInteger().byteValue();
        }
        return result;
    }

    private byte[] encodeSelfDescribing(Value value) throws VoxException {
        try {
            byte[] schema = handshakeSchema.bundleBytes();
            byte[] encoded = PhonCodec.encodeValue(handshakeSchema, value, limits);
            byte[] result = new byte[4 + schema.length + encoded.length];
            putU32(result, 0, schema.length);
            System.arraycopy(schema, 0, result, 4, schema.length);
            System.arraycopy(encoded, 0, result, 4 + schema.length, encoded.length);
            return result;
        } catch (PhonException failure) {
            throw new VoxException("cannot encode self-describing handshake", failure);
        }
    }

    private static Value connectionSettings(boolean odd) {
        return map(
                "parity", parity(odd),
                "max_concurrent_requests", Value.unsigned(64),
                "initial_channel_credit", Value.unsigned(16));
    }

    private static Value parity(boolean odd) {
        return enumValue(odd ? "Odd" : "Even", Value.nullValue());
    }

    private static Value enumValue(String variant, Value payload) {
        return Value.enumValue(variant, payload);
    }

    private static Value map(Object... entries) {
        LinkedHashMap<String, Value> result = new LinkedHashMap<>();
        for (int index = 0; index < entries.length; index += 2) {
            result.put((String) entries[index], (Value) entries[index + 1]);
        }
        return Value.map(result);
    }

    private static Value unsigned(long value) {
        return Value.unsigned(new BigInteger(Long.toUnsignedString(value)));
    }

    private static Value byteList(byte[] bytes) {
        java.util.ArrayList<Value> result = new java.util.ArrayList<>(bytes.length);
        for (byte value : bytes) result.add(Value.unsigned(Byte.toUnsignedInt(value)));
        return Value.list(result);
    }

    private static long u32(byte[] bytes, int offset) {
        return Integer.toUnsignedLong(
                (bytes[offset] & 0xff)
                        | (bytes[offset + 1] & 0xff) << 8
                        | (bytes[offset + 2] & 0xff) << 16
                        | (bytes[offset + 3] & 0xff) << 24);
    }

    private static void putU32(byte[] bytes, int offset, long value) {
        for (int index = 0; index < 4; index++) {
            bytes[offset + index] = (byte) (value >>> (8 * index));
        }
    }
}
