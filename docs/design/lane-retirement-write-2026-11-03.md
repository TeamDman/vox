# Lane retirement and write ownership proposal

Status: design for review; incomplete. The preserved
`lane_close_between_worker_check_and_write_is_not_serialized` regression
exercises the earlier close/write gap.

Local WIP adds a nonblocking retirement attempt and retains the numeric ID in
a retiring slot when the lane is busy, poisoned, quarantined or has an active
lease. That path requests connection termination rather than claiming a clean
close. Focused tests passed for the previously validated revision; this is not
transport teardown proof. In
particular, dropping a connection and invoking a transport's `abort` do not
prove that independently owned writer tasks have stopped, and a replacement
connection must not reuse the transport while its teardown is uncertain. The
local WIP worker acquires a lease for each lane batch. Successful sends on a
transport without a local-settlement guarantee leave a sticky unconfirmed-write
state: retiring that lane terminates the connection rather than allowing reuse.
This conservative behavior changes lane-close semantics for such transports
and is not evidence that their writers have stopped. Cancellation can
still leave an independently owned transport writer running, the teardown
deadline is incomplete. The WIP worker accepts an optional queued response
capability and checks its lane, request ID and method. Ordinary driver and
synthesized-error replies supply it on transports that declare local write
settlement; forwarded replies and legacy paths still do not. This WIP does not
establish the full close/write guarantee or
authorize live transfer.

A later local WIP routes terminal control requests through a small pump that
signals admission independently of message processing. The receive loop can
then cancel stalled message, local-open, local-close, nonterminal-control, and
keepalive handlers and terminate the connection. Focused tests exercise a
stalled message handler and local open; interrupted operations have uncertain
outcomes. This is
not a completed absolute-deadline shutdown: a synchronous lock, scheduler
stall, writer clone or unsupported transport abort can still prevent proven
teardown. A focused test now observes detachment of the core sender on an
unsupported-close path; it does not prove transport teardown. The pump and its
cancellation paths require further tests and independent review.

## Scope and identities

Each accepted lane owns one non-reusable lifetime object. Senders, declaration
gates, requests and admitted batches retain that exact object; numeric lane IDs
are wire addresses and are not lifetime capabilities. A replacement cannot
reuse the same numeric wire ID while an older lifetime has an unsettled write.
If retirement cannot be settled within the deadline, retire the connection and
retain the old generation's ownership until teardown is confirmed or the
process terminates. A bounded one-connection failure budget must prevent
unbounded quarantined workers, frames or lane generations.
If abort is unsupported or the writer never settles, reject further work and
fail callers, quarantine at most the bounded connection and its existing
ownership, and do not start a replacement connection or reuse its lane IDs as
a workaround. Explicitly report teardown as incomplete until writer termination
is observed or the process ends.

The lifetime has a synchronized state: `Open`, `Retiring` or `Retired`, plus
ownership of at most one currently executing outbound batch. The outbound
worker is already serial, but the ownership must remain explicit: transport
clones and spawned writer tasks can survive dropping a single future.

## Admission, write and retirement

* A sender reserves queue capacity, then under the admission synchronization
  validates the exact lane generation and `Open` state. Enqueue and transfer of
  the response/channel reservation happen without an intervening await. Work
  rejected before enqueue resolves as definitely not queued. Forced-stop
  overlap remains uncertain unless its commit handshake is strengthened.
  The final `Open` validation and enqueue must linearize against `Open` to
  `Retiring` under the same lock, or under a separately reviewed commit
  handshake with an explicit lock order. Two independent checks leave an
  enqueue-after-retirement gap. Acquiring queue capacity may happen before the
  lock only if the lifetime is revalidated at that commit point.
* Before beginning a batch, the worker acquires a lifetime-bound write lease
  only if that exact lane is `Open`. A queued old-generation batch that cannot
  obtain a lease fails its result and releases its owned resources without
  writing. The lease must remain owned until schema and payload writes have
  settled; it cannot be released just because a timeout cancelled a wrapper
  future while a spawned writer may still be active.
  The current generic `ConduitTx::send_prepared` contract only promises an
  enqueue path. A successful return is not itself proof that the local writer
  has settled. Any opt-in local-settlement capability must be verified for the
  exact transport; unsupported transports fail closed for this proof. Even a
  positive success capability says nothing about error, cancellation or peer
  receipt, and does not establish complete transport teardown.
* Retirement linearizes by changing the exact lifetime from `Open` to
  `Retiring`, rejecting future leases and admissions and waking pending
  channel and lane waiters. A batch that already has a write lease is
  **in flight**, not retroactively unsent. The retirement operation waits for
  that lease to settle before declaring the lifetime fully retired or allowing
  wire-ID reuse. It must use one absolute deadline shared with any queue fence,
  protocol close, transport flush and teardown; a fresh timeout at each step is
  not equivalent.
  Write-lease acquisition and the `Retiring` transition must be mutually
  exclusive under that same state synchronization; an independent atomic
  check before acquisition does not establish this ordering.
  Response-capability reservation and its enqueue commit must participate in
  this ordering as well. The current standalone request book uses a separate
  mutex; merely placing it inside a lane object does not make reservation,
  retirement and enqueue atomic. Define and test a shared lock or an explicit
  lock-order/commit protocol before wiring those paths together.
* Do not hold a synchronous mutex across an await or transport call. State
  transitions and lease acquisition use short critical sections; waiting uses
  a persistent notification or async synchronization that cannot lose wakeups.
  Ownership of the actual transport writer must be tracked through completion.

The existing synchronous `remove_connection_with_reason` and early
`close_all_connections` calls cannot by themselves implement this protocol.
Audit every local close, dropped-handle, rejected-open, received `LaneClose`,
protocol-error, EOF and resource-exhaustion path. Refactor state retirement and
numeric-ID reuse together; do not add another unsynchronized check and call it
a barrier. Driver/control messages and failure notices must carry the original
lifetime and request identity rather than looking up the current numeric ID.

## Protocol and failure distinctions

For a local graceful lane close, stop admission, resolve or fence work already
admitted to that lane, then order `LaneClose` after the eligible writes using a
private control path. The local close result means only the stated local drain
and write outcome; it is not a peer receipt. Preserve the spec's prohibition on
sending lane traffic after `LaneClose`.

For a received `LaneClose`, stop new lane traffic immediately. If a write was
already admitted or started and cannot be proven stopped before it crosses the
wire, the outcome is uncertain: terminate or quarantine the connection rather
than assert that no bytes were sent. Local close initiation, peer EOF,
close-notify, reset/truncation and unknown partitions are distinct outcomes.

On deadline expiry, abort all relevant transport owners and fail unresolved
results. An abort request alone, especially the default no-op `abort()`, does
not prove teardown. Await actual writer completion where the transport supports
it; otherwise report incomplete teardown and prevent reuse. A late success from
an old write cannot authorize a new lane or restore a failed request. Dropped
queue entries and pending permits must resolve exactly once.

The current gate and admission-stop fallbacks catch failed watch notifications
and retry only best-effort. They do not prove that already parked waiters wake
if notification repeatedly fails. Treat that case as a liveness blocker until
an independently guaranteed teardown/wakeup path is implemented and exercised;
an atomic failed bit alone is not a wakeup.

The explicit connection shutdown must interrupt a stalled handler as well as
bound admission, queue drain, protocol close and transport close. It must not
start its deadline only after the receive loop eventually observes the signal.
The existing stalled-handler diagnostic remains a required regression.

## Required verification

Use deterministic barriers before lease acquisition, after lease acquisition
but before write, during partial writes, and during lease release. Cover local
and remote close, simultaneous close/send, queued and prepared producers,
held lane and conduit clones, reused numeric IDs, failed/unsupported abort,
worker panic, deadline expiry and late completion. Verify all sender results
and waiters, no old-generation mutation, and no new generation admission while
old ownership is unsettled. Exercise real loopback TLS and each supported
transport separately; local flush or close-notify is not proof of peer receipt.
Also exercise injected notification failures, poison while a waiter is parked,
nested/reentrant encoding capture, cancellation, weak-gate reclamation and
retained sender/gate references across multiple lane generations.

This proposal does not complete response reservation, gate lifetime cleanup or
connection shutdown, and does not authorize live transfer.

### Local lane close review follow-up

A review of `6303b9f1` found that local close retired a lane before its queued
admitted batches ran, then could report successful `LaneClose` even though
those batches failed. A deterministic test reproduced this. The local WIP now
introduces a `Draining` state: the lane lifetime lock rejects new queue commits
while allowing batches committed before that transition to take a write
lease. A connection-worker fence waits for prior queue work before the lane is
retired and `LaneClose` is sent. The focused regression checks two queued
batches, their results, a rejected post-cutoff send, and wire order. Tests also
cover an active lease, deadline failure, caller cancellation, and a transport
that cannot prove local settlement.

This is an unreviewed prototype. The fence is connection-wide and retains a
sticky failure for an earlier rejected batch, so an unrelated prior rejection
can conservatively fail a lane close. A timeout, cancellation, unavailable
lane lock, or unqualified transport requests connection abort and reports an
error; abort is not proof of writer termination. The local success result is
not a peer receipt. Remote close, other transport implementations, real
concurrent traffic, cancellation at every boundary, and the one-second budget
need independent review and further qualification.

## Forwarded request identity: unresolved ingress boundary

The connection currently inserts `(request ID, method ID)` into a mutable
per-lane map before it routes the incoming call. A second call with the same
ID can replace that binding before either the driver or the proxy handles the
first call. `duplicate_ingress_does_not_replace_live_request_binding` is a
preserved diagnostic for this gap. It first failed: the second method (`9`)
replaced the first (`7`) before routing completed. A local partial fix now
rejects a duplicate while the original pre-write binding is present, before
recording inline schema. The regression and the existing protocol-close test
pass. The worker currently
removes that binding before attempting the response write, so this check does
not address response-in-flight reuse or late replies.
The proxy also forwards a response by numeric ID without the original request
capability. The existing driver-side reservation cannot establish proxy safety.

The ingress path needs to create and carry the original lane-bound request
capability, including its method and lifetime, through `RecvMessage` to both
the driver and the proxy. Duplicate detection must precede any mutation of a
live request's method/schema binding. A forwarded response must use that exact
capability and preserve forwarded schemas and descriptor behavior; every
failure, cancellation, lane retirement and uncertain write must resolve or
quarantine the matching generation. Opposite directions need independent,
bounded request maps. The existing opt-in lifetime quotas must bound all
additional retained state; an unrestricted legacy connection must not silently
acquire an unbounded tombstone table.

The wire carries a numeric request ID, not an opaque request generation. The
RPC spec prohibits reuse while a call is in flight but does not prohibit reuse
after completion. A second call can therefore be legitimate once the peer has
received the first response, even if the local write future has not yet
returned. A simple `HashMap::entry` rejection until local completion can reject
that valid interleaving; removing the old entry before sending can instead
attach an old response to a new call. Treat overlap with a write as uncertain
and bound any wait for the old local outcome; do not assert peer protocol
violation merely from the local write still being active. If settlement cannot
be established, terminate or quarantine rather than replace the old binding.

For proxy egress, allocating a fresh, non-reused wire ID for each forwarded
request is a candidate way to distinguish a late duplicate response from a
later request that reuses the source ID. The proxy would translate calls,
cancellations and responses in both directions, respect the destination
parity, stop on integer exhaustion, and retain exact capability and schema
bindings. Before implementation, audit every request-ID reference and
bidirectional/channel interaction, bound lifetime ID and active-map state,
and define how a transport without local write settlement fails closed. The
local WIP now rewrites IDs on locally settled transports. An in-memory proxy
test passes with two consecutive calls that reuse the same source ID, and a
request-map test rejects a late response for the earlier destination ID. The
implementation still needs independent review and broader end-to-end tests. Transports without
the local-settlement capability use the legacy unqualified path. Neither path
establishes teardown or peer receipt.

The current capability is captured when the proxy consumes a call, not at
connection ingress. The pre-write duplicate guard protects its original
method binding only while that binding remains present. Ingress/consumer
races, overlapping response writes, descriptor-bearing messages, actual
late-response injection, and cancellation during a real proxy write still
require focused tests and review.

Each proxy direction would own an active mapping from the source request's
exact ingress capability to a fresh destination request ID, plus the reverse
mapping from that destination ID to the same capability and method. A call
installs both mappings before sending; cancellation translates through the
still-live mapping; a response reserves and sends the original source
capability and removes the mappings only after its defined local settlement.
A definitely pre-enqueue failure may discard a mapping; an ambiguous send
must terminate or quarantine it. Never reuse a destination ID within that
proxy lane, and fail on allocation exhaustion rather than wrap. Both
directions require independent parity-correct allocators and bounds. Audit
channel ownership, schema tracker ordering and request lifecycle first. The
current `send_owned` path passes schemas and messages but does not pass
`RecvMessage.fds`; descriptor forwarding needs an explicit supported-or-
rejected contract, not a claim that request mapping covers descriptors.

## Connection shutdown: proposed queue fence

The current atomic admission-stop signal is useful for waking stalled
handlers, but it is not the admission cutoff. In particular, the cleanup path
currently skips acquiring the admission mutex if the signal is already set.
A producer may have passed its final check and still be committing a batch.
Dropping `core.inner.tx` or calling the conduit close at that point would not
order that batch before close. The retained-conduit diagnostic must not be
fixed by merely taking this reference or by treating `abort()` as proof of
teardown.

A candidate connection-level sequence is:

1. Record a single absolute deadline at the terminal request's submission,
   including explicit and internal shutdown sources. Propagate that deadline
   through the control pump and every shutdown phase; observing the request
   later must not restart the clock. Wake handlers immediately, while treating
   interrupted operations as uncertain.
2. Acquire the admission mutex within the remaining budget even if the atomic
   signal is already set, set its terminal state, and finish any commit that
   linearized before the lock was acquired. If the mutex cannot be acquired,
   skip graceful success, request abort and retain/quarantine ownership whose
   settlement is not known.
3. Enqueue an internal worker fence after that cutoff, under the same absolute
   deadline. Its acknowledgment means preceding accepted batches have reached
   their defined local worker outcomes. It is not a peer receipt; for a
   transport whose send success does not settle local writes, it is not proof
   of transport settlement. Cancellation, queue closure, worker panic, or an
   unsuccessful batch must fail the fence rather than report a successful
   drain.
4. Only after the fence is acknowledged, invoke the transport's bounded
   shared close with the remaining budget. Unsupported, timeout, cancellation
   or error is an incomplete shutdown and initiates best-effort abort. Retain
   or quarantine the necessary ownership until supported transport teardown
   is observed; a dropped core reference or abort request is not that proof.
   Remove the retained core sender without waiting indefinitely on a poisoned
   or busy synchronous lock, and wake all unresolved callers.

The local prototype now captures the shutdown deadline before enqueueing the
request, and the explicit shutdown handler uses a worker fence followed by
bounded shared transport close with the remaining budget. A scoped guard
requests abort if that handler fails or is cancelled after the guard is
installed. Twelve focused shutdown tests pass, including ordered local write
before supported shared close, an already-expired deadline, cancellation
during close, and detaching the core sender on an unsupported transport. The
latter two outcomes are not proof that an independently owned writer stopped.

This remains an unreviewed prototype, not a qualified complete shutdown.
The local WIP adds connection-owner cleanup before the scoped guard is
installed. Tests pass for a never-polled run future and a busy sender lock.
It requests abort and uses `try_lock` for detachment; a busy or
poisoned lock, unsupported abort, or independently owned writer remains
uncertain. An independent owner-liveness channel now wakes a registered
admission waiter when the owner drops on native targets; the previously
failing regression passed when run separately. This path is disabled on WASM
because that target's watch implementation needs separate waiter-lifecycle
qualification. The channel does not guarantee progress for an
arbitrary panicking or blocking waker or a stalled synchronous callback. The receive loop may take an abort path
before consuming a terminal request. Synchronous wakeups and lane cleanup are not preempted by
the deadline. The public API does not yet report whether close succeeded or
was unsupported. A connection-level TLS loopback test now observes clean peer
EOF in the idle case; it does not prove all transport owners settle or peer
RPC receipt. Guarded proxy identity remains under review. Do not
enable live transfer based on these tests.

The existing public `shutdown()` is a synchronous request, and `run()` returns
unit; neither currently provides an outcome for unsupported or incomplete
teardown. A separately specified completion result or observer is needed
before callers can distinguish local graceful completion from an abort or
unknown state. The overall sequence remains a design candidate.
Tests must cover the reserved-permit/final-check race, a stalled pre-cutoff
batch, an accepted queued batch, cancellation at each phase, full queues,
worker failure and panic, held clones, unsupported close/abort, delayed
transport shutdown and absolute-budget exhaustion. Exercise loopback TLS as
well as the supported non-native transports before claiming coverage.

## Real-stream qualification plan

`vox-stream` has `shared_close_sends_tls_close_notify_over_loopback` and a TLS
truncation test, which exercise `StreamLinkTx` directly. A separate
`connection_shutdown_sends_tls_close_notify_over_loopback` test now uses the
existing ephemeral test CA, a listener bound only to `127.0.0.1:0`, and
`initiator_on_link` / `acceptor_on_link` over TLS-wrapped `StreamLink`. It
retains a public handle and confirms clean peer TLS EOF after an idle
connection shutdown; it passed outside the sandbox after the sandbox denied
loopback binding. This is not evidence of peer RPC processing. Repeat with a queued write held
behind a deterministic barrier, cancellation during close, a TCP reset or
truncation, and a half-closed peer. Check pending send results and retain any
writer whose termination is not observed. Also test unsupported shared close
without inventing teardown success. Duplex TLS is useful for deterministic
ordering but does not replace actual loopback TCP; report platform bind denial
and use the supported approval path when appropriate.
