# Preserved channel-credit behaviors

The integration keeps `VoxConnection.java` and the runtime/subject sources from
`e76bef6ade844985e98d8799d139c905fba28c10` byte-for-byte. That source also keeps
the reproducible JDK package guard. The distinct older implementation remains on
`migration/preserved-java-late-credit-4a079ac` at
`82dc657bfc28321ba5f2336252912985a4ccf834`, mapped from
`4a079ac1c8a8bb8a914811ef55945bc1d9a9fef3`.

## Test method and results

The migration's complete Java gate first passed with the pinned Temurin
21.0.8+9 JDK and Java 17 target. For comparison, the exact historical
`ChannelCreditRetirementTest.java` was compiled against those classes and run
one case at a time. A separate class directory overlaid the exact historical
`VoxConnection.java` and `VoxRuntimeTest.java` for the older baseline. No runtime
sources were changed to make either baseline pass.

The older baseline passes all 19 cases and its complete runtime suite. The e76
baseline passes 15 of those assertions. Its four differences are:

| Historical assertion | e76 result | 4a result | Interpretation |
| --- | --- | --- | --- |
| `credit_on_zero_channel_fails` | No exception | Rejects | e76 accepts valid credit for missing channel ID zero on a known lane |
| `credit_on_opening_lane_fails` | No exception | Rejects | e76 accepts valid late credit on a known outbound lane still opening |
| `credit_on_control_lane_fails` | Rejects with `message for unknown lane 0` | Rejects with `message for unknown channel 0:1` | Diagnostic-only difference in this test |
| `credit_on_unopened_lane_fails` | Rejects with `message for unknown lane 99` | Rejects with `message for unknown channel 99:1` | Diagnostic-only difference in this test |

Both baselines pass valid active credit, credit after channel close, credit for
a missing channel, old credit beyond a fixed channel-history window, and valid
credit after local lane close. Both reject zero credit on active/missing/closed
channels, overflow credit, missing or wrongly typed credit fields, credit to a
locally receiving channel, and invalid item/close/reset messages for missing or
retired channels. The complete case results are in
[credit-matrix.tsv](credit-matrix.tsv).

Code inspection also shows a validation-order difference on locally closed
lanes: e76 returns before decoding the credit amount, whereas 4a validates the
credit first. The 19-case matrix tests valid credit after local lane close; it
does not test malformed credit after local lane close.

## Recommendation

Reconcile the stricter nonzero-channel, open-lane and malformed-credit checks
in a separate focused fix after reviewing their intended protocol behavior.
Retain e76's SDK guard and valid late-credit behavior. Add explicit malformed
credit tests for locally closed lanes during that reconciliation. The migration
does not silently replace the SFM-proven baseline with the older branch.

These are connection-local credit rules. They do not establish automatic RPC
replay, durable transfer resumption, or permission to retry a call. Vox 0.10
receive failure ends the link; any future mover resume/idempotency protocol
must be application-level work on a new authenticated connection.
