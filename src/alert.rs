/// Hold/cooldown state machine. Pure; caller supplies a monotonic clock in ms.
pub struct AlertGate {
    hold_ms: u64,
    cooldown_ms: u64,
    over_since: Option<u64>,
    cooldown_until: u64,
}

impl AlertGate {
    pub fn new(hold_ms: u64, cooldown_ms: u64) -> Self {
        Self {
            hold_ms,
            cooldown_ms,
            over_since: None,
            cooldown_until: 0,
        }
    }

    /// Feed the current over-threshold flag; returns true when an alert should fire.
    pub fn update(&mut self, over: bool, now_ms: u64) -> bool {
        if !over {
            self.over_since = None;
            return false;
        }
        if now_ms < self.cooldown_until {
            // Still cooling down. Latch the hold start to when cooldown ends so a
            // signal that stays over threshold through the cooldown fires as soon
            // as it's been over for hold_ms past cooldown_until, rather than
            // restarting the hold clock at whatever moment we happen to poll.
            if self.over_since.is_none() {
                self.over_since = Some(self.cooldown_until);
            }
            return false;
        }
        match self.over_since {
            None => {
                self.over_since = Some(now_ms);
                false
            }
            Some(start) if now_ms - start >= self.hold_ms => {
                self.over_since = None;
                self.cooldown_until = now_ms + self.cooldown_ms;
                true
            }
            Some(_) => false,
        }
    }

    /// Forget any in-progress hold; keep the cooldown.
    pub fn reset(&mut self) {
        self.over_since = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn does_not_fire_before_hold_time() {
        let mut g = AlertGate::new(300, 3000);
        assert!(!g.update(true, 0));
        assert!(!g.update(true, 100));
        assert!(!g.update(true, 299));
    }

    #[test]
    fn fires_after_hold_time() {
        let mut g = AlertGate::new(300, 3000);
        assert!(!g.update(true, 0));
        assert!(g.update(true, 300));
    }

    #[test]
    fn a_dip_resets_the_hold() {
        let mut g = AlertGate::new(300, 3000);
        assert!(!g.update(true, 0));
        assert!(!g.update(false, 200)); // dropped under threshold
        assert!(!g.update(true, 250)); // hold restarts here
        assert!(!g.update(true, 500));
        assert!(g.update(true, 550));
    }

    #[test]
    fn cooldown_blocks_refire() {
        let mut g = AlertGate::new(300, 3000);
        g.update(true, 0);
        assert!(g.update(true, 300));
        assert!(!g.update(true, 700)); // still over, but cooling down
        assert!(!g.update(true, 3200)); // cooldown ends at 3300
        assert!(g.update(true, 3600)); // over continuously since 3300 for 300ms
    }
}
