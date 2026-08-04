package org.facet.vox;

import java.util.Arrays;
import java.time.Duration;
import org.facet.phon.CompatibilityPlan;
import org.facet.phon.PhonCodec;
import org.facet.phon.PhonException;
import org.facet.phon.PhonLimits;
import org.facet.phon.Schema;
import org.facet.phon.SchemaClosure;
import org.facet.vox.generated.DivideByZero;
import org.facet.vox.generated.DivideResponse;
import org.facet.vox.generated.JavaFixtureDivideResponse;
import org.facet.vox.generated.JavaFixtureEchoResponse;
import org.facet.vox.generated.TerminalAlphaMode;
import org.facet.vox.generated.TerminalCapabilities;
import org.facet.vox.generated.TerminalColorSpace;
import org.facet.vox.generated.TerminalConnectArgs;
import org.facet.vox.generated.TerminalConnectRequest;
import org.facet.vox.generated.TerminalError;
import org.facet.vox.generated.TerminalErrorCode;
import org.facet.vox.generated.TerminalFrameEncoding;
import org.facet.vox.generated.TerminalFrameEvent;
import org.facet.vox.generated.TerminalFrameOrigin;
import org.facet.vox.generated.TerminalGetContentResponse;
import org.facet.vox.generated.TerminalPresentationCapabilitiesResult;
import org.facet.vox.generated.TerminalPresentationMode;
import org.facet.vox.generated.TerminalPresentationUnavailable;
import org.facet.vox.generated.TerminalRasterFrame;
import org.facet.vox.generated.TerminalRasterFrameEvent;
import org.facet.vox.generated.TerminalRasterFrameKind;
import org.facet.vox.generated.TerminalRasterRendererTelemetry;
import org.facet.vox.generated.TerminalRasterSubscribeRequest;
import org.facet.vox.generated.TerminalRasterizationOwner;
import org.facet.vox.generated.TerminalCursor;
import org.facet.vox.generated.TerminalPromptMetadata;
import org.facet.vox.generated.TerminalRange;
import org.facet.vox.generated.TerminalRasterSnapshotTiming;
import org.facet.vox.generated.TerminalSelection;
import org.facet.vox.generated.TerminalSurfaceMetrics;
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
        largeRasterChannelItemUsesByteRunLimit();
        terminalPresentationV3FieldsRoundTripAndRejectUnknownOwner();
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

    private static void largeRasterChannelItemUsesByteRunLimit() throws Exception {
        byte[] pixels = new byte[1_100_000];
        for (int index = 0; index < pixels.length; index++) pixels[index] = (byte) (index * 31);
        TerminalRange emptyRange = new TerminalRange(0, 0, 0, 0);
        TerminalRasterFrame frame = new TerminalRasterFrame(
                110, 50, 550, 500,
                new TerminalSurfaceMetrics(110, 50, 550, 500, 5, 10, 8),
                TerminalFrameEncoding.RGBA8,
                TerminalRasterFrameKind.FULL,
                2_200,
                TerminalFrameOrigin.TOP_LEFT,
                TerminalAlphaMode.STRAIGHT,
                TerminalColorSpace.SRGB,
                "test-font", "test-font-sha256", pixels, java.util.List.of(), true,
                new TerminalCursor(0, 0, true),
                false,
                new TerminalSelection(0, 0, 0, 0),
                new TerminalPromptMetadata(false, emptyRange, false, emptyRange, false, 0),
                new TerminalRasterSnapshotTiming(0, 0, 0, 0, 0, 0, 0, 0)
        );
        TerminalRasterFrameEvent event = new TerminalRasterFrameEvent(
                "session", "connection", "epoch", "presentation", 1, 1, 0, true,
                "rust-gpu-slug", "full", "full-raw-rgba", 1, 1,
                16L * 1024L * 1024L, 64, "large-raster", frame
        );
        byte[] payload = PhonCodec.encode(
                TerminalRasterFrameEvent.ADAPTER, event, PhonLimits.defaults());
        SchemaClosure writer = SchemaClosure.fromBundleBytes(
                TerminalRasterFrameEvent.ADAPTER.schema().bundleBytes(), PhonLimits.defaults());
        ChannelRuntime.Transport transport = new ChannelRuntime.Transport() {
            public boolean item(long laneId, long channelId, byte[] bytes) { return true; }
            public boolean close(long laneId, long channelId) { return true; }
            public boolean reset(long laneId, long channelId) { return true; }
            public boolean grant(long laneId, long channelId, int additional) { return true; }
        };
        ChannelRuntime.Receiver<TerminalRasterFrameEvent> receiver = new ChannelRuntime.Receiver<>(
                1, 2, TerminalRasterFrameEvent.ADAPTER, transport, 1);

        receiver.item(payload, writer);
        TerminalRasterFrameEvent decoded = receiver.receive(Duration.ZERO);

        check(decoded != null && Arrays.equals(pixels, decoded.frame().payload()),
                "large generated raster survives the real channel compatibility path");
    }

    private static void terminalPresentationV3FieldsRoundTripAndRejectUnknownOwner()
            throws Exception {
        java.util.List<TerminalPresentationMode> modes = new java.util.ArrayList<>();
        modes.add(terminalPresentationMode("rust-cpu-fontdue", "full", "full-png",
                TerminalFrameEncoding.PNG, TerminalRasterFrameKind.FULL));
        modes.add(terminalPresentationMode("rust-cpu-fontdue", "full", "full-raw-rgba",
                TerminalFrameEncoding.RGBA8, TerminalRasterFrameKind.FULL));
        modes.add(terminalPresentationMode("rust-cpu-fontdue", "dirty", "dirty-raw-rgba",
                TerminalFrameEncoding.RGBA8, TerminalRasterFrameKind.DIRTY_REGIONS));
        TerminalError gpuError = new TerminalError(
                TerminalErrorCode.UNSUPPORTED_CAPABILITY,
                "rust-gpu-slug unavailable: no Vulkan 1.2 compute device was found",
                false,
                7L);
        java.util.List<TerminalPresentationUnavailable> unavailable = java.util.List.of(
                terminalPresentationUnavailable("full", "full-png", gpuError),
                terminalPresentationUnavailable("full", "full-raw-rgba", gpuError),
                terminalPresentationUnavailable("dirty", "dirty-raw-rgba", gpuError));
        TerminalPresentationCapabilitiesResult capabilities =
                new TerminalPresentationCapabilitiesResult(
                        "session-v5", "rust-cpu-fontdue", "full-png", modes,
                        unavailable, 7L);
        TerminalPresentationCapabilitiesResult capabilitiesBack = PhonCodec.decode(
                TerminalPresentationCapabilitiesResult.ADAPTER,
                PhonCodec.encode(TerminalPresentationCapabilitiesResult.ADAPTER,
                        capabilities, PhonLimits.defaults()),
                PhonLimits.defaults());
        check(capabilities.equals(capabilitiesBack),
                "presentation V3 capabilities roundtrip");
        check(capabilitiesBack.defaultRendererId().equals("rust-cpu-fontdue")
                        && capabilitiesBack.modes().size() == 3
                        && capabilitiesBack.modes().stream().allMatch(mode ->
                                mode.rendererId().equals("rust-cpu-fontdue"))
                        && capabilitiesBack.unavailablePresentations().size() == 3,
                "presentation V3 keeps selectable and unavailable tuples separate");
        check(capabilitiesBack.modes().stream().anyMatch(mode ->
                        mode.rendererId().equals(capabilitiesBack.defaultRendererId())
                                && mode.transportId().equals(
                                        capabilitiesBack.defaultTransportId())),
                "presentation defaults identify a selectable mode");
        check(capabilitiesBack.unavailablePresentations().stream().allMatch(entry ->
                        entry.rendererId().equals("rust-gpu-slug")
                                && entry.rasterizationOwner()
                                        == TerminalRasterizationOwner.SERVER
                                && entry.transportVersion() == 1
                                && entry.error().code()
                                        == TerminalErrorCode.UNSUPPORTED_CAPABILITY
                                && entry.error().message().contains(
                                        "Vulkan 1.2 compute device")
                                && !entry.error().retryable()
                                && entry.error().serverSequence() == 7L),
                "unavailable tuples preserve exact identity and actionable error");

        TerminalRasterSubscribeRequest request = new TerminalRasterSubscribeRequest(
                "session-v5", "rust-gpu-slug", "dirty", "dirty-raw-rgba", 1,
                "panel-2-presentation-3", 18L, 3L, 16L * 1024L * 1024L, 64,
                20L, "subscribe-v5-3");
        TerminalRasterSubscribeRequest requestBack = PhonCodec.decode(
                TerminalRasterSubscribeRequest.ADAPTER,
                PhonCodec.encode(TerminalRasterSubscribeRequest.ADAPTER,
                        request, PhonLimits.defaults()),
                PhonLimits.defaults());
        check(request.equals(requestBack)
                        && requestBack.presentationGeneration()
                                .equals("panel-2-presentation-3"),
                "presentation generation request roundtrip");
        check(recordHasField(TerminalPresentationCapabilitiesResult.SCHEMA,
                        "default_renderer_id"),
                "generated capability result carries default_renderer_id");
        check(recordHasField(TerminalPresentationCapabilitiesResult.SCHEMA,
                        "unavailable_presentations"),
                "generated capability result carries unavailable_presentations");
        check(recordHasField(TerminalPresentationMode.SCHEMA, "rasterization_owner"),
                "generated capability mode carries rasterization_owner");
        check(recordHasField(TerminalPresentationUnavailable.SCHEMA,
                        "rasterization_owner")
                        && recordHasField(TerminalPresentationUnavailable.SCHEMA,
                                "error"),
                "generated unavailable presentation carries closed owner and error");
        check(recordHasField(TerminalRasterSubscribeRequest.SCHEMA,
                        "presentation_generation")
                        && recordHasField(TerminalRasterFrameEvent.SCHEMA,
                                "presentation_generation"),
                "generated raster request and event carry presentation_generation");
        check(!recordHasField(TerminalRasterSubscribeRequest.SCHEMA,
                        "transport_generation")
                        && !recordHasField(TerminalRasterFrameEvent.SCHEMA,
                                "transport_generation"),
                "generated current raster API omits transport_generation");
        check(recordHasField(TerminalRasterFrame.SCHEMA, "renderer"),
                "generated raster frame carries defaulted renderer evidence");
        check(recordHasField(TerminalRasterRendererTelemetry.SCHEMA,
                        "gpu_completion_wait_us")
                        && recordHasField(TerminalRasterRendererTelemetry.SCHEMA,
                                "target_reuses")
                        && recordHasField(TerminalRasterRendererTelemetry.SCHEMA,
                                "font_renderer_cache_hits"),
                "generated renderer evidence carries GPU stages and retained cache counters");

        for (TerminalRasterizationOwner owner : TerminalRasterizationOwner.values()) {
            TerminalRasterizationOwner ownerBack = PhonCodec.decode(
                    TerminalRasterizationOwner.ADAPTER,
                    PhonCodec.encode(TerminalRasterizationOwner.ADAPTER,
                            owner, PhonLimits.defaults()),
                    PhonLimits.defaults());
            check(owner == ownerBack, "rasterization owner roundtrip " + owner);
        }
        boolean unknownOwnerRejected = false;
        try {
            PhonCodec.decode(TerminalRasterizationOwner.ADAPTER,
                    new byte[] {2, 0, 0, 0}, PhonLimits.defaults());
        } catch (PhonException expected) {
            unknownOwnerRejected = true;
        }
        check(unknownOwnerRejected, "unknown rasterization owner rejected");
    }

    private static TerminalPresentationMode terminalPresentationMode(
            String rendererId,
            String damageModeId,
            String transportId,
            TerminalFrameEncoding encoding,
            TerminalRasterFrameKind frameKind) {
        return new TerminalPresentationMode(
                rendererId,
                TerminalRasterizationOwner.SERVER,
                damageModeId,
                transportId,
                1,
                encoding,
                frameKind,
                1,
                TerminalFrameOrigin.TOP_LEFT,
                TerminalAlphaMode.STRAIGHT,
                TerminalColorSpace.SRGB,
                4096,
                4096,
                16L * 1024L * 1024L,
                64);
    }

    private static TerminalPresentationUnavailable terminalPresentationUnavailable(
            String damageModeId,
            String transportId,
            TerminalError error) {
        return new TerminalPresentationUnavailable(
                "rust-gpu-slug",
                TerminalRasterizationOwner.SERVER,
                damageModeId,
                transportId,
                1,
                error);
    }

    private static boolean recordHasField(Schema schema, String fieldName) {
        if (!(schema.kind() instanceof Schema.RecordKind record)) return false;
        return record.fields().stream().anyMatch(field -> field.name().equals(fieldName));
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
