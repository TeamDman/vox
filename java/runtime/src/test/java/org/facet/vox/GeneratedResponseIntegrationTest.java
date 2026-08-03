package org.facet.vox;

import java.util.Arrays;
import org.facet.phon.PhonCodec;
import org.facet.phon.PhonLimits;
import org.facet.vox.generated.DivideByZero;
import org.facet.vox.generated.DivideResponse;
import org.facet.vox.generated.JavaFixtureDivideResponse;
import org.facet.vox.generated.JavaFixtureEchoResponse;
import org.facet.vox.generated.TerminalCapabilities;
import org.facet.vox.generated.TerminalConnectArgs;
import org.facet.vox.generated.TerminalConnectRequest;

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

    private static void check(boolean condition, String description) {
        if (!condition) throw new AssertionError(description);
    }
}
