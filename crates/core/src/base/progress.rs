use std::ops::Range;

pub type Progress = Range<u64>;

pub trait Spliceable {
    fn can_splice(&self, other: &Self) -> bool;
}

impl Spliceable for Progress {
    fn can_splice(&self, b: &Self) -> bool {
        self.start == b.end || b.start == self.end
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new() {
        let progress = 0..100;
        assert_eq!(progress.start, 0);
        assert_eq!(progress.end, 100);

        let progress = 0..1;
        assert_eq!(progress.start, 0);
        assert_eq!(progress.end, 1);
    }

    #[test]
    fn test_can_merge_adjacent() {
        let a = 0..5;
        let b = 5..10;
        assert!(a.can_splice(&b));
        assert!(b.can_splice(&a));
    }

    #[test]
    fn test_cannot_merge_non_adjacent_non_overlapping() {
        let a = 0..5;
        let b = 7..10;
        assert!(!a.can_splice(&b));
        assert!(!b.can_splice(&a));
    }

    #[test]
    fn test_cannot_merge_disjoint() {
        let a = 0..5;
        let b = 6..15;
        assert!(!a.can_splice(&b));
        assert!(!b.can_splice(&a));
    }
}
