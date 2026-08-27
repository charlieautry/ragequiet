/// Rolling median over the last `capacity` pushed values.
/// All storage preallocated; push() and median() never allocate.
pub struct RollingMedian {
    buf: Vec<f32>,
    scratch: Vec<f32>,
    pos: usize,
    filled: usize,
}

impl RollingMedian {
    pub fn new(capacity: usize) -> Self {
        Self {
            buf: vec![0.0; capacity],
            scratch: vec![0.0; capacity],
            pos: 0,
            filled: 0,
        }
    }

    pub fn push(&mut self, v: f32) {
        self.buf[self.pos] = v;
        self.pos = (self.pos + 1) % self.buf.len();
        self.filled = (self.filled + 1).min(self.buf.len());
    }

    pub fn median(&mut self) -> Option<f32> {
        if self.filled == 0 {
            return None;
        }
        let n = self.filled;
        self.scratch[..n].copy_from_slice(&self.buf[..n]);
        let mid = n / 2;
        self.scratch[..n].select_nth_unstable_by(mid, |a, b| a.total_cmp(b));
        Some(self.scratch[mid])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_has_no_median() {
        let mut m = RollingMedian::new(8);
        assert_eq!(m.median(), None);
    }

    #[test]
    fn median_of_odd_count() {
        let mut m = RollingMedian::new(8);
        for v in [5.0, 1.0, 3.0] {
            m.push(v);
        }
        assert_eq!(m.median(), Some(3.0));
    }

    #[test]
    fn old_values_fall_out_of_the_window() {
        let mut m = RollingMedian::new(4);
        for v in [100.0, 100.0, 100.0, 100.0] {
            m.push(v);
        }
        assert_eq!(m.median(), Some(100.0));
        // window is 4; four new values fully replace the old ones
        for v in [1.0, 2.0, 3.0, 2.0] {
            m.push(v);
        }
        assert_eq!(m.median(), Some(2.0));
    }

    #[test]
    fn is_robust_to_outliers() {
        let mut m = RollingMedian::new(16);
        for _ in 0..10 {
            m.push(-40.0);
        }
        m.push(0.0); // one shout
        assert_eq!(m.median(), Some(-40.0));
    }
}
