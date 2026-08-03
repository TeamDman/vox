package org.facet.vox;

import java.util.Arrays;
import org.facet.phon.CompatibilityPlan;
import org.facet.phon.PhonCodec;
import org.facet.phon.PhonLimits;
import org.facet.phon.SchemaClosure;
import org.facet.vox.generated.DivideByZero;
import org.facet.vox.generated.DivideResponse;
import org.facet.vox.generated.JavaFixtureDivideResponse;
import org.facet.vox.generated.JavaFixtureEchoResponse;
import org.facet.vox.generated.TerminalCapabilities;
import org.facet.vox.generated.TerminalConnectArgs;
import org.facet.vox.generated.TerminalConnectRequest;
import org.facet.vox.generated.TerminalError;
import org.facet.vox.generated.TerminalErrorCode;
import org.facet.vox.generated.TerminalFrameEncoding;
import org.facet.vox.generated.TerminalFrameEvent;
import org.facet.vox.generated.TerminalGetContentResponse;
import org.facet.vox.generated.TerminalSnapshot;
import org.facet.vox.generated.TerminalSnapshotResponse;

public final class GeneratedResponseIntegrationTest {
    private GeneratedResponseIntegrationTest() {}

    public static void main(String[] args) throws Exception {
        VoxResult<String, Void> success = VoxResult.success("ok");
        byte[] successBytes =
                PhonCodec.encode(JavaFixtureEchoResponse.ADAPTER, success, PhonLimits.defaults());
        check(Arrays.equals(successBytes,
                new byte[] {0, 0, 0, 0, 2, 0, 0, 0, 'o', 'k'}), "success wire bytes");
        VoxResult<String, Void> successBack =
                PhonCodec.decode(JavaFixtureEchoResponse.ADAPTER, successBytes, PhonLimits.defaults());
        check(successBack.isSuccess() && successBack.success().equals("ok"), "success roundtrip");

        VoxResult<DivideResponse, DivideByZero> application =
                VoxResult.applicationError(DivideByZero.ZERO);
        byte[] applicationBytes =
                PhonCodec.encode(JavaFixtureDivideResponse.ADAPTER, application, PhonLimits.defaults());
        check(Arrays.equals(applicationBytes,
                new byte[] {1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0}),
                "application error uses nested Result/VoxError discriminants");
        VoxResult<DivideResponse, DivideByZero> applicationBack =
                PhonCodec.decode(JavaFixtureDivideResponse.ADAPTER, applicationBytes,
                        PhonLimits.defaults());
        check(applicationBack.isApplicationError()
                        && applicationBack.applicationError() == DivideByZero.ZERO,
                "application error roundtrip");

        VoxResult<String, Void> invalid =
                VoxResult.infrastructure(VoxResult.Kind.INVALID_PAYLOAD, "bad");
        VoxResult<String, Void> invalidBack = PhonCodec.decode(
                JavaFixtureEchoResponse.ADAPTER,
                PhonCodec.encode(JavaFixtureEchoResponse.ADAPTER, invalid, PhonLimits.defaults()),
                PhonLimits.defaults());
        check(invalidBack.kind() == VoxResult.Kind.INVALID_PAYLOAD
                        && invalidBack.detail().equals("bad"),
                "infrastructure error roundtrip");
        terminalGeneratedSchemasInitialize();
        terminalRustPayloadTranscodesThroughGeneratedResponseSchema();
        terminalRustApplicationErrorTranscodesThroughGeneratedResponseSchema();
        terminalRustSnapshotPayloadTranscodesThroughGeneratedResponseSchema();
        terminalFrameChannelElementSchemaIsTransitivelySelfContained();
        System.out.println("GeneratedResponseIntegrationTest: PASS");
    }

    private static void terminalGeneratedSchemasInitialize() throws Exception {
        TerminalCapabilities capabilities = new TerminalCapabilities(
                true, true, true, false, true, false, false, false, 240, 120, 8_000_000L);
        TerminalConnectRequest request = new TerminalConnectRequest(
                "127.0.0.1:63946", 120, 40, capabilities, 1L);
        byte[] encoded = PhonCodec.encode(
                TerminalConnectArgs.ADAPTER,
                new TerminalConnectArgs(request),
                PhonLimits.defaults());
        check(encoded.length > 0, "terminal connect request encoding");
        check(TerminalConnectArgs.ADAPTER.schema().id().asLong()
                        == TerminalConnectArgs.SCHEMA.id().asLong(),
                "terminal connect schema identity");
    }

    /** Payload captured from the Rust Facet/Phon encoder for a successful get_content response. */
    private static void terminalRustPayloadTranscodesThroughGeneratedResponseSchema()
            throws Exception {
        byte[] rustPayload = hexBytes(
                "000000000e00000073666d2d7465726d696e616c2d31000007000000000000001300000050532043"
                        + "3a5c776f726b3e2068656c6c6f5c6e010001000000000d0000000000000000000000000000000000"
                        + "00000000");
        SchemaClosure writer = SchemaClosure.fromBundleBytes(
                TerminalGetContentResponse.ADAPTER.schema().bundleBytes(),
                PhonLimits.defaults());
        byte[] localPayload = PhonCodec.transcode(
                writer,
                TerminalGetContentResponse.ADAPTER.schema(),
                rustPayload,
                PhonLimits.defaults());
        VoxResult<?, ?> response = PhonCodec.decode(
                TerminalGetContentResponse.ADAPTER, localPayload, PhonLimits.defaults());
        check(response.isSuccess(), "Rust get_content success payload transcode");
    }

    /** Payload captured from the Rust Facet/Phon encoder for a get_content application error. */
    private static void terminalRustApplicationErrorTranscodesThroughGeneratedResponseSchema()
            throws Exception {
        byte[] rustPayload = hexBytes(
                "01000000000000000600000016000000707479206f757470757420756e617661696c61626c650100"
                        + "0800000000000000");
        SchemaClosure writer = SchemaClosure.fromBundleBytes(
                TerminalGetContentResponse.ADAPTER.schema().bundleBytes(),
                PhonLimits.defaults());
        byte[] localPayload = PhonCodec.transcode(
                writer,
                TerminalGetContentResponse.ADAPTER.schema(),
                rustPayload,
                PhonLimits.defaults());
        VoxResult<?, ?> response = PhonCodec.decode(
                TerminalGetContentResponse.ADAPTER, localPayload, PhonLimits.defaults());
        check(response.isApplicationError(), "Rust get_content application error transcode");
        TerminalError error = (TerminalError) response.applicationError();
        check(error.code() == TerminalErrorCode.INTERNAL
                        && error.message().equals("pty output unavailable")
                        && error.retryable()
                        && error.serverSequence() == 8,
                "Rust get_content application error fields");
    }

    /** Payload captured from the Rust Facet/Phon encoder for a successful snapshot response. */
    private static void terminalRustSnapshotPayloadTranscodesThroughGeneratedResponseSchema()
            throws Exception {
        byte[] rustPayload = hexBytes(
                "000000000a00000073657373696f6e2d30310000000000002a0000000000000029000000000000005000180050001800800280010800100010000b000000736e617073686f742d3031010000000000000000000040010000000000005000180008000000001020ffff8040ff01000300040001010100020005000200010000000000030000000100030000000c0000000100000000000000010000000000000002000000000000000300000000000000040000000000000005000000000000000f00000000000000");
        SchemaClosure writer = SchemaClosure.fromBundleBytes(
                TerminalSnapshotResponse.ADAPTER.schema().bundleBytes(),
                PhonLimits.defaults());
        byte[] localPayload = PhonCodec.transcode(
                writer,
                TerminalSnapshotResponse.ADAPTER.schema(),
                rustPayload,
                PhonLimits.defaults());
        VoxResult<?, ?> response = PhonCodec.decode(
                TerminalSnapshotResponse.ADAPTER, localPayload, PhonLimits.defaults());
        check(response.isSuccess(), "Rust snapshot success payload transcode");
        TerminalSnapshot snapshot = (TerminalSnapshot) response.success();
        check(snapshot.sequence() == 42
                        && snapshot.requestSequence() == 41
                        && snapshot.payload().length == 8
                        && snapshot.logicalColumns() == 80
                        && snapshot.logicalRows() == 24
                        && snapshot.timing().ptyDrainUs() == 1
                        && snapshot.timing().terminalSnapshotUs() == 2
                        && snapshot.timing().fontLoadUs() == 3
                        && snapshot.timing().rasterUs() == 4
                        && snapshot.timing().pngEncodeUs() == 5
                        && snapshot.timing().totalUs() == 15,
                "Rust snapshot success payload fields");
    }

    private static void terminalFrameChannelElementSchemaIsTransitivelySelfContained()
            throws Exception {
        SchemaClosure reader = TerminalFrameEvent.ADAPTER.schema();
        check(reader.schemas().stream().anyMatch(schema ->
                        schema.id().equals(TerminalFrameEncoding.SCHEMA.id())),
                "terminal frame closure carries nested frame encoding schema");
        check(reader.schemas().stream().anyMatch(schema ->
                        schema.id().asLong() == 0xaa0667df4299d151L),
                "terminal frame reader closure carries nested byte payload schema");
        SchemaClosure writer = SchemaClosure.fromBundleBytes(
                reader.bundleBytes(), PhonLimits.defaults());
        check(writer.schemas().stream().anyMatch(schema ->
                        schema.id().asLong() == 0xaa0667df4299d151L),
                "terminal frame serialized closure carries nested byte payload schema");
        CompatibilityPlan.plan(writer, reader, PhonLimits.defaults());
    }

    private static byte[] hexBytes(String hex) {
        if ((hex.length() & 1) != 0) throw new AssertionError("odd hex payload length");
        byte[] bytes = new byte[hex.length() / 2];
        for (int index = 0; index < bytes.length; index++) {
            bytes[index] = (byte) Integer.parseInt(hex.substring(index * 2, index * 2 + 2), 16);
        }
        return bytes;
    }

    private static void check(boolean condition, String description) {
        if (!condition) throw new AssertionError(description);
    }
}
