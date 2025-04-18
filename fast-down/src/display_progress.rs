use crate::progress::{ProgresTrait, Progress};
extern crate alloc;
use alloc::{string::String, vec::Vec};

pub fn display_progress(progress: &[Progress]) -> String {
    progress
        .iter()
        .map(|p| p.format())
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn test_display_progress() {
        let progress = vec![0..10, 20..30];
        assert_eq!(display_progress(&progress), "0-9,20-29");
    }

    #[test]
    fn test_display_progress_empty() {
        let progress: Vec<Progress> = vec![];
        assert_eq!(display_progress(&progress), "");
    }

    #[test]
    fn test_display_progress_single() {
        let progress = vec![5..15];
        assert_eq!(display_progress(&progress), "5-14");
    }
}
