use std::fmt::Debug;
use std::time::{Duration, Instant};
use tokio::sync::mpsc::Sender;

///time, up, single step, ID
pub type ChannelMsg = (Instant, Direction, bool, Blind);
///time of last change, direction, curr_pos, curr_ang
pub type StateStore = std::collections::HashMap<Blind, (Option<Instant>, Direction, Pos, Angle)>;
pub type GroupWriter = Sender<(u16, Direction)>;

#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum Direction {
    Up = 0,
    Down = 1,
}
/// buss address of a blind
#[repr(transparent)]
#[derive(Eq, PartialEq, Hash, Clone, Copy)]
pub struct Blind(u8);
impl Blind {
    const BUS_START_ADDR: u8 = 0xaa;
    pub const CMD_SINGLE_STEP: u16 = 0x11_00;
    pub const CMD_FULL_MOVE: u16 = 0x10_00;
    /// convert the port name on the KNX controller to a `Blind`
    #[inline]
    pub fn from_port(c: u8) -> Blind {
        Blind(c - b'a' + Self::BUS_START_ADDR)
    }
    /// convert a KNX Group Address to `(Blind, is_single_step)`
    /// if the Blind is in the range `BUS_START_ADDR`..=`BUS_START_ADDR`+'h'-'a'
    #[inline]
    pub fn from_bus_addr(addr: u16) -> Result<(Blind, bool), (u8, u16)> {
        let upper = addr & 0xFF00;
        let lower = (addr & 0xff) as u8;
        if let Some(r) = lower.checked_sub(Self::BUS_START_ADDR) {
            if r <= b'h' - b'a' {
                return Ok((Blind(lower), upper == Self::CMD_SINGLE_STEP));
            }
        }
        Err((lower, upper))
    }
    /// convert the Blind to a KNX Group Address
    #[inline]
    pub fn to_bus_addr(self, single_step: bool) -> u16 {
        if single_step {
            Self::CMD_SINGLE_STEP + self.0 as u16
        } else {
            Self::CMD_FULL_MOVE + self.0 as u16
        }
    }
    #[inline]
    pub fn letter(self) -> char {
        (self.0 - Self::BUS_START_ADDR + b'a') as char
    }
    //special id for wind indicator
    pub fn wind() -> Blind {
        Blind(0)
    }
}
impl Debug for Blind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!("{:x}", self.0))
    }
}
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Angle(u8);
impl Angle {
    const MAX: u8 = 7;
    pub fn up(&mut self, time_moving: Duration) {
        if time_moving >= crate::FULL_TURN_TIME {
            self.0 = Self::MAX;
            return;
        }
        let div = Self::MAX as u128 * time_moving.as_nanos() / crate::FULL_TURN_TIME.as_nanos();
        let t = self.0 + div as u8;
        //t = t.clamp(0, Self::MAX);
        if t > Self::MAX {
            self.0 = Self::MAX;
        } else {
            self.0 = t;
        }
    }

    pub fn down(&mut self, time_moving: Duration) {
        if time_moving >= crate::FULL_TURN_TIME {
            self.0 = 0;
            return;
        }
        let div = Self::MAX as u128 * time_moving.as_nanos() / crate::FULL_TURN_TIME.as_nanos();
        self.0 = self.0.saturating_sub(div as u8);
    }
    pub fn step_up(&mut self, arg: u8) -> bool {
        let t = self.0 + arg;
        if t > Self::MAX {
            self.0 = Self::MAX;
            true
        } else {
            self.0 = t;
            false
        }
    }

    pub fn step_down(&mut self, arg: u8) -> bool {
        if let Some(n) = self.0.checked_sub(arg) {
            self.0 = n;
            false
        } else {
            self.0 = 0;
            true
        }
    }
    pub const TOP: Angle = Angle(Self::MAX);
    pub const BOTTOM: Angle = Angle(0);
    /// move for x ms (x=FULL_TURN/Angle::TOP*steps)
    pub fn step_time(steps: u8) -> Duration {
        crate::FULL_TURN_TIME * (steps as u32) / (Self::MAX as u32)
    }
    /// return the direction and steps between `other` and self.
    /// `other` is the new position, so
    /// Down, if other is smaller
    pub fn delta(&self, other: Angle) -> (Direction, u8) {
        if self.0 > other.0 {
            //go down
            (Direction::Down, self.0 - other.0)
        } else {
            //go up
            (Direction::Up, other.0 - self.0)
        }
    }
    pub fn from_num(n: u8) -> Angle {
        assert!(n <= Self::MAX);
        Angle(n)
    }
}
impl From<Angle> for u8 {
    fn from(val: Angle) -> Self {
        val.0
    }
}
/*impl std::ops::SubAssign<Duration> for Angle {
    fn sub_assign(&mut self, rhs: Duration) {
        self.down(rhs);
    }
}*/
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pos(u8);
impl Pos {
    pub const TOP: Pos = Pos(100);
    pub const BOTTOM: Pos = Pos(0);
    pub fn up(&mut self, time_moving: Duration) {
        if time_moving >= crate::FULL_TRAVEL_TIME {
            self.0 = 100;
            return;
        }
        let div = 100 * time_moving.as_nanos() / crate::FULL_TRAVEL_TIME.as_nanos();
        let t = self.0 + div as u8;
        if t > 100 {
            self.0 = 100;
        } else {
            self.0 = t;
        }
    }

    pub fn down(&mut self, time_moving: Duration) {
        if time_moving >= crate::FULL_TRAVEL_TIME {
            self.0 = 0;
            return;
        }
        let div = 100 * time_moving.as_nanos() / crate::FULL_TRAVEL_TIME.as_nanos();
        self.0 = self.0.saturating_sub(div as u8);
    }
    /// move for x ms (x=FULL_MOVE/Pos::TOP*steps)
    pub fn step_time(steps: u8) -> Duration {
        crate::FULL_TRAVEL_TIME * (steps as u32) / 100u32
    }
    /// return the direction and steps between `other` and self.
    /// `other` is the new position, so
    /// Down, if other is smaller
    pub fn delta(&self, other: Pos) -> (Direction, u8) {
        if self.0 > other.0 {
            //go down
            (Direction::Down, self.0 - other.0)
        } else {
            //go up
            (Direction::Up, other.0 - self.0)
        }
    }
    pub fn from_num(n: u8) -> Pos {
        assert!(n <= 100);
        Pos(n)
    }
}
impl From<Pos> for u8 {
    fn from(val: Pos) -> Self {
        val.0
    }
}
