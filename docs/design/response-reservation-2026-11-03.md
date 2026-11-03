# Response reservation and schema transaction proposal

Status: design for review. The local response-bookkeeping commit `57772626` is
not a complete implementation. Queue acceptance, a completed local write, and
peer receipt are separate events; neither of the first two proves delivery.

## Current hazards

* Preparing a response reads the incoming request's `(request_id, method_id)`
  without reserving it. Two responses can both be prepared and enqueued. The
  later worker-side remove check does not prevent the second response send.
* A queued old response can match and remove a newer request with the same ID
  and method. The same numeric lane ID can also identify a different lane
  lifetime. Equality of numeric IDs and method is not an identity proof.
* `plan_response_schema_send` can insert into `planned_bindings` via
  `get_or_plan_binding_for_shape` or `get_or_plan_binding_from_tracker` during
  preparation. A rejected or cancelled preparation can leave this cache behind;
  deleting it unconditionally is unsafe if another batch uses the same plan.
  Planning in isolation does not mark `send_tracker` sent, but the separate
  `prepare_response_for_shape` path currently calls `commit_prepared_plan`
  before enqueue or transport send. It can advertise a binding as sent when no
  schema frame was sent. Audit every direct `commit_prepared_plan` caller.
* `ConnectionCoreInner.conns` retains its per-lane send state after ordinary
  `remove_connection_with_reason`; lane close alone is not an identity or
  lifetime boundary for that state.

## Proposed identities and state

Create a fresh lane-lifetime object when a lane becomes active. Every sender,
request binding, prepared batch and queued batch refers to this exact object;
it has an irreversible terminal state. Closing a lane invalidates that object,
wakes its waiters and removes its registry entry only if the entry still names
the same object. A recreated lane gets a new object even if its numeric ID is
reused. A retained old sender must not acquire the new lane object by lookup.

Record each accepted incoming request in its lane-lifetime state as a fresh
identity-bearing object with its request ID and method ID. Identity can be an
`Arc` object compared by `Arc::ptr_eq`, provided each pending reservation and
queued batch holds a strong reference until its terminal outcome; alternatively
use a checked, non-repeating generation allocator and fail closed on exhaustion.
Never replace an active request solely by matching IDs: reject an in-flight ID
reuse as the protocol requires. Any legal later reuse creates a distinct object.
Propagate the original request identity from inbound dispatch through the
handler/request scope into *every* response path, including generated handlers,
forwarding and synthesized error responses. An old handler that holds only a
numeric ID must not look up and reserve the current entry: that could authorize
it against a later request with the same ID. Existing response APIs that accept
only IDs need an identity-bearing internal capability or an explicit fail-closed
compatibility path. Method ID alone is not a replacement for this capability.

Under the lane/request state lock, exactly one responder may transition the
same live request from `Open` to `Reserved(reservation_identity)`. Another
simultaneous response is rejected, not sent without its schema. The reservation
is owned by an RAII guard through encoding, schema planning and admission. If
preparation fails, admission returns full/closed, or the future is cancelled
before enqueue, dropping the guard changes *that exact live reservation* back
to `Open` and releases its private plans. It does not alter a replacement
request, terminal lane, or another reservation. Existing request-cancellation
semantics must determine whether a cancelled request instead becomes terminal.
Panic/unwind, lock poisoning and lane close must fail closed and wake waiters.

At the queue commit point, the reservation transitions once to `Queued` and
ownership moves into the batch in the same serialized admission operation.
There must be no cancellation point between transfer of ownership and enqueue.
If forced stop can overlap that operation, the overlap is an uncertain
pre-admitted operation as documented by the shutdown work; it is not a hard
cutoff. A queued batch validates the exact lane and request identities and its
reservation before use. Dequeuing alone does not authorize a response for a
closed/recreated lane. A stale or duplicate batch must resolve its sender with
an error and must not write schema or response bytes.
Queue closure, worker abort, and dropping a queued batch must also resolve the
reservation and sender exactly once, without restoring an uncertain operation
to `Open`. Avoid holding a synchronous lock across an await or transport call.

Once the first transport write may have begun, a failure, timeout or cancellation
is an uncertain outcome. Do not reopen the reservation or retry the response on
the same live request. Tear down or terminalize the affected connection/lane as
required by the eventual transport shutdown contract; do not report a peer
receipt. A completed local send can retire the request locally but still does
not prove the peer processed it. Failed-send cleanup must not erase a new
request or new lane generation.

The worker check alone cannot exclude a lane-close race between validation and
the actual transport write. Its commit/close handshake must be integrated with
the separately reviewed connection admission and drain design. Until that is
implemented and tested, do not claim that lane termination prevents late bytes.

## Schema preparation and commit

Make schema planning a batch-owned value with no `sent` mutation and no shared
mutable plan ownership during preparation. A discarded reservation then simply
drops its plan. For repeated bindings, either derive the plan independently or
use an explicitly immutable, bounded cache whose lifetime and concurrent owners
are independent of request rollback. Do not delete a shared cache entry as the
rollback of one request. Forwarded schemas and explicit error shapes need the
same ownership and validation rules as ordinary response shapes.

In particular, replace early `commit_prepared_plan` in
`prepare_response_for_shape` with a batch-owned prepared plan. Preview schema
frames in outbound order; mark `send_tracker` only after the corresponding
schema send reports local success. If a send may have written bytes but reports
failure, the connection is terminal/uncertain rather than reusing its tracker.
Define duplicate plan handling and cache/resource bounds before implementation;
do not treat local schema write success as peer processing.

## Required tests before integration

* Deterministically race two response reservations: exactly one is accepted;
  check preparation failure, queue full/closed, cancellation before enqueue,
  cancellation after enqueue, and successful response.
* Replace a completed request ID with the same method and with a different
  method while an older batch/guard remains. Old cleanup and sends must not
  alter the replacement. Reject reuse while the original is still in flight.
* Close and recreate a numeric lane while holding old sender, reservation and
  batch objects. Verify old objects cannot send on or mutate the new lane;
  include both close-before-validation and close-during-send races.
* Force uncertain writes, worker failure, disconnect and shutdown. No duplicate
  retry or success receipt follows an uncertain outcome; all waiters terminate.
* Abandon or fail ordinary, forwarded and explicit-shape response planning.
  Verify no false sent-schema mark, no unsafe shared-plan deletion, bounded
  retained state, and correct schema-before-response ordering when retried.
* Exercise local loopback and supported non-native transports separately; do
  not infer peer processing or crash durability from in-memory tests.

The separate channel-gate failure, lane cleanup and full shutdown diagnostics
remain unresolved. This proposal does not enable a live transfer.
