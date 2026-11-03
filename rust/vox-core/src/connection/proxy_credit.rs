//! Unintegrated accounting prototype for a proxied lane's channel credit.
//!
//! The proxy must apply a transition before forwarding each frame, and must
//! terminate both connections on a forwarding failure or cancellation. This
//! module does not perform I/O, enforce its own deadline, or authorize a live
//! proxy. Early channel frames are retained as bounded provisional entries.

use std::collections::{HashMap, HashSet};
use vox_types::{ChannelId, Parity, RequestId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Side {
    Left,
    Right,
}

impl Side {
    fn other(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
        }
    }
}

/// One direction's advertised initial credit and the destination receiver's
/// initial credit and maximum outstanding peer-granted credit.
#[derive(Clone, Copy, Debug)]
pub(super) struct FlowLimits {
    pub sender_initial: u32,
    pub receiver_initial: u32,
    pub receiver_max: u32,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Limits {
    pub left_to_right: FlowLimits,
    pub right_to_left: FlowLimits,
    pub left_caller_parity: Parity,
    pub right_caller_parity: Parity,
    /// Lifetime distinct IDs; terminal entries remain as bounded tombstones.
    pub max_channel_ids: usize,
    /// Early channel IDs not yet associated with a forwarded Call.
    pub max_provisional: usize,
    /// Lifetime Call, channel-message and request-completion transitions.
    pub max_events: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Event {
    Item,
    Grant(u32),
    Close,
    Reset,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CreditError {
    InvalidLimits,
    EventLimit,
    ChannelLimit,
    ProvisionalLimit,
    InvalidChannel,
    DuplicateDeclaration,
    ConflictingDirection,
    TerminalOrAmbiguous,
    Exhausted,
    CreditOverflow,
}

#[derive(Clone, Debug, Default)]
struct ChannelState {
    declared: Option<(Side, RequestId)>,
    sender: Option<Side>,
    sender_credit: u32,
    receiver_credit: u32,
    terminal: bool,
}

pub(super) struct ProxyCreditGate {
    limits: Limits,
    channels: HashMap<ChannelId, ChannelState>,
    provisional: usize,
    events: usize,
}

impl ProxyCreditGate {
    pub(super) fn new(limits: Limits) -> Result<Self, CreditError> {
        let valid = |flow: FlowLimits| {
            flow.sender_initial > 0
                && flow.sender_initial <= flow.receiver_initial
                && flow.receiver_initial <= flow.receiver_max
        };
        if !valid(limits.left_to_right)
            || !valid(limits.right_to_left)
            || limits.left_caller_parity == limits.right_caller_parity
            || limits.max_channel_ids == 0
            || limits.max_provisional > limits.max_channel_ids
            || limits.max_events == 0
        {
            return Err(CreditError::InvalidLimits);
        }
        Ok(Self {
            limits,
            channels: HashMap::new(),
            provisional: 0,
            events: 0,
        })
    }

    fn remaining_event(&self) -> Result<(), CreditError> {
        if self.events >= self.limits.max_events {
            Err(CreditError::EventLimit)
        } else {
            Ok(())
        }
    }

    /// Bind a call's channel IDs before forwarding it. Validate the entire
    /// declaration before mutating any state, including early-frame entries.
    pub(super) fn declare(
        &mut self,
        caller: Side,
        request: RequestId,
        ids: &[ChannelId],
    ) -> Result<(), CreditError> {
        self.remaining_event()?;
        if ids.len() > self.limits.max_channel_ids {
            return Err(CreditError::ChannelLimit);
        }
        let parity = match caller {
            Side::Left => self.limits.left_caller_parity,
            Side::Right => self.limits.right_caller_parity,
        };
        if !request.has_parity(parity) {
            return Err(CreditError::InvalidChannel);
        }
        let mut seen = HashSet::new();
        let mut new_count = 0_usize;
        for &id in ids {
            if id.0 == 0 || !id.has_parity(parity) || !seen.insert(id) {
                return Err(CreditError::InvalidChannel);
            }
            match self.channels.get(&id) {
                Some(state) if state.declared.is_some() => {
                    return Err(CreditError::DuplicateDeclaration);
                }
                Some(state) if state.terminal => return Err(CreditError::TerminalOrAmbiguous),
                Some(_) => {}
                None => new_count += 1,
            }
        }
        if new_count
            > self
                .limits
                .max_channel_ids
                .saturating_sub(self.channels.len())
        {
            return Err(CreditError::ChannelLimit);
        }
        for &id in ids {
            let state = self.channels.entry(id).or_default();
            if state.sender.is_some() {
                self.provisional -= 1;
            }
            state.declared = Some((caller, request));
        }
        self.events += 1;
        Ok(())
    }

    /// Account for a frame received from `from` before forwarding it. Failed
    /// checks leave the prior state intact. Close/reset races are conservatively
    /// terminal, not automatically proof that the peer violated the protocol.
    pub(super) fn observe(
        &mut self,
        from: Side,
        id: ChannelId,
        event: Event,
    ) -> Result<(), CreditError> {
        self.remaining_event()?;
        if id.0 == 0 {
            return Err(CreditError::InvalidChannel);
        }
        let is_new = !self.channels.contains_key(&id);
        if is_new && self.channels.len() >= self.limits.max_channel_ids {
            return Err(CreditError::ChannelLimit);
        }
        if is_new && self.provisional >= self.limits.max_provisional {
            return Err(CreditError::ProvisionalLimit);
        }
        let sender = match event {
            Event::Item | Event::Close => from,
            Event::Grant(_) | Event::Reset => from.other(),
        };
        let flow = match sender {
            Side::Left => self.limits.left_to_right,
            Side::Right => self.limits.right_to_left,
        };
        let mut state = self.channels.get(&id).cloned().unwrap_or_default();
        if state.terminal {
            return Err(CreditError::TerminalOrAmbiguous);
        }
        match state.sender {
            Some(existing) if existing != sender => return Err(CreditError::ConflictingDirection),
            Some(_) => {}
            None => {
                state.sender = Some(sender);
                state.sender_credit = flow.sender_initial;
                state.receiver_credit = flow.receiver_initial;
            }
        }
        match event {
            Event::Item => {
                state.sender_credit = state
                    .sender_credit
                    .checked_sub(1)
                    .ok_or(CreditError::Exhausted)?;
                state.receiver_credit = state
                    .receiver_credit
                    .checked_sub(1)
                    .ok_or(CreditError::Exhausted)?;
            }
            Event::Grant(additional) => {
                let sender_credit = state
                    .sender_credit
                    .checked_add(additional)
                    .ok_or(CreditError::CreditOverflow)?;
                let receiver_credit = state
                    .receiver_credit
                    .checked_add(additional)
                    .ok_or(CreditError::CreditOverflow)?;
                if receiver_credit > flow.receiver_max {
                    return Err(CreditError::CreditOverflow);
                }
                state.sender_credit = sender_credit;
                state.receiver_credit = receiver_credit;
            }
            Event::Close | Event::Reset => state.terminal = true,
        }
        if is_new {
            self.provisional += 1;
        }
        self.channels.insert(id, state);
        self.events += 1;
        Ok(())
    }

    /// Request completion closes every declared sideband while retaining a
    /// tombstone to prevent late traffic or numeric-ID aliasing.
    pub(super) fn finish_request(
        &mut self,
        caller: Side,
        request: RequestId,
    ) -> Result<(), CreditError> {
        self.remaining_event()?;
        for state in self.channels.values_mut() {
            if state.declared == Some((caller, request)) {
                state.terminal = true;
            }
        }
        self.events += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> Limits {
        Limits {
            left_to_right: FlowLimits {
                sender_initial: 2,
                receiver_initial: 3,
                receiver_max: 5,
            },
            right_to_left: FlowLimits {
                sender_initial: 1,
                receiver_initial: 1,
                receiver_max: 3,
            },
            left_caller_parity: Parity::Odd,
            right_caller_parity: Parity::Even,
            max_channel_ids: 4,
            max_provisional: 2,
            max_events: 20,
        }
    }

    #[test]
    fn incompatible_initial_credit_is_rejected() {
        let mut config = limits();
        config.left_to_right.sender_initial = 4;
        assert!(matches!(
            ProxyCreditGate::new(config),
            Err(CreditError::InvalidLimits)
        ));
        let mut config = limits();
        config.right_caller_parity = Parity::Odd;
        assert!(matches!(
            ProxyCreditGate::new(config),
            Err(CreditError::InvalidLimits)
        ));
    }

    #[test]
    fn repeated_grants_are_bounded_by_accumulated_destination_credit() {
        let mut gate = ProxyCreditGate::new(limits()).unwrap();
        let id = ChannelId(3);
        gate.declare(Side::Left, RequestId(1), &[id]).unwrap();
        gate.observe(Side::Right, id, Event::Grant(2)).unwrap();
        assert_eq!(
            gate.observe(Side::Right, id, Event::Grant(1)),
            Err(CreditError::CreditOverflow)
        );
        gate.observe(Side::Left, id, Event::Item).unwrap();
        gate.observe(Side::Right, id, Event::Grant(1)).unwrap();
        let state = gate.channels.get(&id).unwrap();
        assert_eq!((state.sender_credit, state.receiver_credit), (4, 5));
    }

    #[test]
    fn source_credit_cannot_exceed_what_the_proxy_advertised() {
        let mut gate = ProxyCreditGate::new(limits()).unwrap();
        let id = ChannelId(3);
        gate.declare(Side::Left, RequestId(1), &[id]).unwrap();
        gate.observe(Side::Left, id, Event::Item).unwrap();
        gate.observe(Side::Left, id, Event::Item).unwrap();
        assert_eq!(
            gate.observe(Side::Left, id, Event::Item),
            Err(CreditError::Exhausted)
        );
        gate.observe(Side::Right, id, Event::Grant(1)).unwrap();
        gate.observe(Side::Left, id, Event::Item).unwrap();
    }

    #[test]
    fn reverse_direction_and_conflicting_orientation_are_distinguished() {
        let mut gate = ProxyCreditGate::new(limits()).unwrap();
        let id = ChannelId(2);
        gate.declare(Side::Right, RequestId(2), &[id]).unwrap();
        gate.observe(Side::Left, id, Event::Grant(1)).unwrap();
        gate.observe(Side::Right, id, Event::Item).unwrap();
        assert_eq!(
            gate.observe(Side::Left, id, Event::Item),
            Err(CreditError::ConflictingDirection)
        );
    }

    #[test]
    fn early_frames_are_bounded_then_bound_to_the_call() {
        let mut gate = ProxyCreditGate::new(limits()).unwrap();
        let id = ChannelId(3);
        gate.observe(Side::Left, id, Event::Item).unwrap();
        assert_eq!(gate.provisional, 1);
        gate.declare(Side::Left, RequestId(1), &[id]).unwrap();
        assert_eq!(gate.provisional, 0);
        assert_eq!(
            gate.declare(Side::Left, RequestId(3), &[id]),
            Err(CreditError::DuplicateDeclaration)
        );
        gate.finish_request(Side::Left, RequestId(1)).unwrap();
        assert_eq!(
            gate.observe(Side::Right, id, Event::Grant(1)),
            Err(CreditError::TerminalOrAmbiguous)
        );
    }

    #[test]
    fn failed_declaration_and_overflow_do_not_modify_prior_state() {
        let mut gate = ProxyCreditGate::new(limits()).unwrap();
        let id = ChannelId(3);
        assert_eq!(
            gate.declare(Side::Left, RequestId(1), &[id, id]),
            Err(CreditError::InvalidChannel)
        );
        assert!(gate.channels.is_empty());
        assert_eq!(gate.events, 0);
        gate.declare(Side::Left, RequestId(1), &[id]).unwrap();
        assert_eq!(
            gate.observe(Side::Right, id, Event::Grant(u32::MAX)),
            Err(CreditError::CreditOverflow)
        );
        assert!(gate.channels.get(&id).unwrap().sender.is_none());
        gate.observe(Side::Right, id, Event::Grant(1)).unwrap();
    }

    #[test]
    fn provisional_and_lifetime_event_limits_fail_closed() {
        let mut config = limits();
        config.max_provisional = 1;
        config.max_events = 2;
        let mut gate = ProxyCreditGate::new(config).unwrap();
        gate.observe(Side::Left, ChannelId(3), Event::Item).unwrap();
        assert_eq!(
            gate.observe(Side::Left, ChannelId(5), Event::Item),
            Err(CreditError::ProvisionalLimit)
        );
        gate.declare(Side::Left, RequestId(1), &[ChannelId(3)])
            .unwrap();
        assert_eq!(
            gate.observe(Side::Left, ChannelId(3), Event::Item),
            Err(CreditError::EventLimit)
        );
    }

    #[test]
    fn identical_channel_ids_on_different_lanes_have_independent_credit() {
        let mut first = ProxyCreditGate::new(limits()).unwrap();
        let mut second = ProxyCreditGate::new(limits()).unwrap();
        let id = ChannelId(3);
        first.declare(Side::Left, RequestId(1), &[id]).unwrap();
        second.declare(Side::Left, RequestId(1), &[id]).unwrap();
        first.observe(Side::Left, id, Event::Item).unwrap();
        first.observe(Side::Left, id, Event::Item).unwrap();
        assert_eq!(
            first.observe(Side::Left, id, Event::Item),
            Err(CreditError::Exhausted)
        );
        second.observe(Side::Left, id, Event::Item).unwrap();
    }

    #[test]
    fn post_reset_traffic_is_reported_as_ambiguous() {
        let mut gate = ProxyCreditGate::new(limits()).unwrap();
        let id = ChannelId(3);
        gate.declare(Side::Left, RequestId(1), &[id]).unwrap();
        gate.observe(Side::Right, id, Event::Reset).unwrap();
        assert_eq!(
            gate.observe(Side::Left, id, Event::Item),
            Err(CreditError::TerminalOrAmbiguous)
        );
        // The item may have been in flight before the reset reached its
        // sender; this result is not automatically a peer protocol violation.
    }
}
