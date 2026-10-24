//! Two independent file-transfer lanes with finite leases and late-message protection.
use wayline_proto::transfer::{Phase, Reason, Transfer, FRAME_LEN, HEADER_LEN, LABEL_LEN};

#[derive(Clone, Copy)]
pub struct Lane {
    pub transfer: Transfer,
    pub label: [u8; LABEL_LEN],
    seen: u32,
    terminal_since: u32,
    retired: [u64; 4],
    next_retired: usize,
}
impl Lane {
    const fn new(direction: u8) -> Self {
        Self {
            transfer: Transfer::idle(direction),
            label: [0; LABEL_LEN],
            seen: 0,
            terminal_since: 0,
            retired: [0; 4],
            next_retired: 0,
        }
    }
    pub fn visible(&self, now: u32) -> Option<Transfer> {
        let mut value = self.transfer;
        let age = now.wrapping_sub(self.seen);
        if value.phase == Phase::Idle {
            return None;
        }
        if value.phase.active() {
            if age >= 38_000 {
                return None;
            }
            if age >= 8000 {
                value.phase = Phase::Failed;
                value.reason = Reason::Interrupted;
            }
        } else {
            let retain = if value.phase == Phase::Complete {
                10_000
            } else {
                30_000
            };
            if now.wrapping_sub(self.terminal_since) >= retain {
                return None;
            }
        }
        Some(value)
    }
}

#[derive(Clone, Copy)]
pub struct Feedback {
    pub lanes: [Lane; 2],
}
impl Default for Feedback {
    fn default() -> Self {
        Self::new()
    }
}
impl Feedback {
    pub const fn new() -> Self {
        Self {
            lanes: [Lane::new(0), Lane::new(1)],
        }
    }
    pub fn apply(&mut self, bytes: &[u8], now: u32) -> bool {
        if bytes.len() != FRAME_LEN {
            return false;
        }
        let Some(value) = Transfer::decode(&bytes[..HEADER_LEN]) else {
            return false;
        };
        let lane = &mut self.lanes[usize::from(value.direction)];
        if value.id != lane.transfer.id {
            if value.id == 0
                || lane.retired.contains(&value.id)
                || (lane.transfer.phase.active() && now.wrapping_sub(lane.seen) < 8000)
            {
                return false;
            }
            lane.retired[lane.next_retired] = lane.transfer.id;
            lane.next_retired = (lane.next_retired + 1) % lane.retired.len();
            lane.terminal_since = now;
        } else {
            if !lane.transfer.phase.active()
                && lane.transfer.phase != Phase::Idle
                && value.phase.active()
            {
                return false;
            }
            if lane.transfer.phase.active() && !value.phase.active() {
                lane.terminal_since = now;
            }
            if value.phase == Phase::Transferring
                && lane.transfer.phase == Phase::Transferring
                && value.total == lane.transfer.total
                && value.done < lane.transfer.done
            {
                return false;
            }
        }
        lane.transfer = value;
        lane.seen = now;
        lane.label.copy_from_slice(&bytes[HEADER_LEN..]);
        true
    }
    pub fn header(&self, direction: u8, now: u32) -> [u8; HEADER_LEN] {
        self.lanes[usize::from(direction)]
            .visible(now)
            .unwrap_or(Transfer::idle(direction))
            .encode()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn packet(id: u64, direction: u8, phase: Phase) -> [u8; FRAME_LEN] {
        let mut out = [0; FRAME_LEN];
        let value = Transfer {
            id,
            direction,
            phase,
            ..Transfer::idle(direction)
        };
        out[..HEADER_LEN].copy_from_slice(&value.encode());
        out
    }
    #[test]
    fn lanes_expire_and_retired_tasks_cannot_replace_new_tasks() {
        let mut state = Feedback::new();
        assert!(state.apply(&packet(1, 0, Phase::Waiting), 0));
        assert!(state.apply(&packet(2, 1, Phase::Transferring), 1));
        assert!(!state.apply(&packet(3, 0, Phase::Preparing), 2));
        assert_eq!(
            state.lanes[0].visible(8000).unwrap().reason,
            Reason::Interrupted
        );
        assert_eq!(
            state.lanes[1].visible(8000).unwrap().phase,
            Phase::Transferring
        );
        assert!(state.apply(&packet(1, 0, Phase::Complete), 9000));
        assert!(!state.apply(&packet(1, 0, Phase::Transferring), 9001));
        assert!(state.apply(&packet(3, 0, Phase::Preparing), 9002));
        assert!(state.apply(&packet(3, 0, Phase::Complete), 9003));
        assert!(!state.apply(&packet(1, 0, Phase::Complete), 9004));
        assert!(state.apply(&packet(3, 0, Phase::Complete), 18_000));
        assert!(state.lanes[0].visible(19_003).is_none());
    }
    #[test]
    fn lease_age_handles_timer_wrap() {
        let mut state = Feedback::new();
        let start = u32::MAX - 50;
        assert!(state.apply(&packet(7, 1, Phase::Confirming), start));
        assert_eq!(
            state.lanes[1]
                .visible(start.wrapping_add(7999))
                .unwrap()
                .phase,
            Phase::Confirming
        );
        assert_eq!(
            state.lanes[1]
                .visible(start.wrapping_add(8000))
                .unwrap()
                .reason,
            Reason::Interrupted
        );
        assert!(state.lanes[1].visible(start.wrapping_add(38_000)).is_none());
    }
}
