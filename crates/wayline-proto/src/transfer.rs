//! Optional protocol-v2 UI extension. Progress describes file payloads, never radio frames.
pub const HEADER_LEN: usize = 32;
pub const LABEL_WIDTH: usize = 160;
pub const LABEL_HEIGHT: usize = 14;
pub const LABEL_LEN: usize = LABEL_WIDTH * LABEL_HEIGHT / 2;
pub const FRAME_LEN: usize = HEADER_LEN + LABEL_LEN;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Phase {
    Idle = 0,
    Preparing = 1,
    Waiting = 2,
    Transferring = 3,
    Confirming = 4,
    Complete = 5,
    Failed = 6,
    Cancelled = 7,
}
impl Phase {
    pub fn from_byte(value: u8) -> Option<Self> {
        Some(match value {
            0 => Self::Idle,
            1 => Self::Preparing,
            2 => Self::Waiting,
            3 => Self::Transferring,
            4 => Self::Confirming,
            5 => Self::Complete,
            6 => Self::Failed,
            7 => Self::Cancelled,
            _ => return None,
        })
    }
    pub fn active(self) -> bool {
        matches!(
            self,
            Self::Preparing | Self::Waiting | Self::Transferring | Self::Confirming
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Reason {
    None = 0,
    Declined = 1,
    Connection = 2,
    Storage = 3,
    Interrupted = 4,
    Timeout = 5,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transfer {
    /// 0 = Windows sending, 1 = Windows receiving.
    pub direction: u8,
    pub phase: Phase,
    pub reason: Reason,
    pub known_total: bool,
    pub files: u16,
    pub id: u64,
    pub done: u64,
    pub total: u64,
}
impl Transfer {
    pub const fn idle(direction: u8) -> Self {
        Self {
            direction,
            phase: Phase::Idle,
            reason: Reason::None,
            known_total: false,
            files: 0,
            id: 0,
            done: 0,
            total: 0,
        }
    }
    pub fn encode(self) -> [u8; HEADER_LEN] {
        let mut bytes = [0; HEADER_LEN];
        bytes[..5].copy_from_slice(&[
            1,
            self.direction,
            self.phase as u8,
            self.reason as u8,
            self.known_total as u8,
        ]);
        bytes[6..8].copy_from_slice(&self.files.to_le_bytes());
        bytes[8..16].copy_from_slice(&self.id.to_le_bytes());
        bytes[16..24].copy_from_slice(&self.done.to_le_bytes());
        bytes[24..32].copy_from_slice(&self.total.to_le_bytes());
        bytes
    }
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != HEADER_LEN
            || bytes[0] != 1
            || bytes[1] > 1
            || bytes[4] > 1
            || bytes[5] != 0
        {
            return None;
        }
        let phase = Phase::from_byte(bytes[2])?;
        let reason = match bytes[3] {
            0 => Reason::None,
            1 => Reason::Declined,
            2 => Reason::Connection,
            3 => Reason::Storage,
            4 => Reason::Interrupted,
            5 => Reason::Timeout,
            _ => return None,
        };
        let value = Self {
            direction: bytes[1],
            phase,
            reason,
            known_total: bytes[4] != 0,
            files: u16::from_le_bytes(bytes[6..8].try_into().ok()?),
            id: u64::from_le_bytes(bytes[8..16].try_into().ok()?),
            done: u64::from_le_bytes(bytes[16..24].try_into().ok()?),
            total: u64::from_le_bytes(bytes[24..32].try_into().ok()?),
        };
        if (phase != Phase::Idle && value.id == 0)
            || (value.known_total && (value.total == 0 || value.done > value.total))
        {
            return None;
        }
        Some(value)
    }
    pub fn percent(self) -> Option<u8> {
        if !self.known_total || self.total == 0 {
            return None;
        }
        let value = self
            .done
            .checked_mul(100)
            .map(|n| n / self.total)
            .unwrap_or_else(|| self.done / (self.total / 100));
        Some(value.min(100) as u8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixed_layout_rejects_invalid_direction_phase_and_progress() {
        let mut value = Transfer {
            direction: 1,
            phase: Phase::Transferring,
            reason: Reason::None,
            known_total: true,
            files: 3,
            id: 0x12345678,
            done: 50,
            total: 100,
        };
        assert_eq!(Transfer::decode(&value.encode()), Some(value));
        assert_eq!(value.percent(), Some(50));
        for (offset, byte) in [(0, 2), (1, 2), (2, 8), (3, 6), (4, 2), (5, 1)] {
            let mut bytes = value.encode();
            bytes[offset] = byte;
            assert!(Transfer::decode(&bytes).is_none());
        }
        value.done = 101;
        assert!(Transfer::decode(&value.encode()).is_none());
        value.done = u64::MAX / 2;
        value.total = u64::MAX;
        assert!((49..=50).contains(&value.percent().unwrap()));
        const { assert!(FRAME_LEN <= crate::PROTO_MAX_FRAME) };
    }
}
