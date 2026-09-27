//! Stable, resumable TimSort implementation used by `table.sort`.
//!
//! Comparisons are surfaced to the caller one at a time so Lua comparators
//! and `__lt` metamethods can suspend on the runtime's frame trampoline.

#[derive(Clone, Copy, Debug)]
struct Run {
    base: usize,
    len: usize,
}

struct Merge<T> {
    run_index: usize,
    left: Vec<T>,
    right: Vec<T>,
    left_index: usize,
    right_index: usize,
    destination: usize,
    force_collapse: bool,
}

enum Phase {
    StartRun,
    DetectDirection {
        base: usize,
    },
    ScanAscending {
        base: usize,
        end: usize,
    },
    ScanDescending {
        base: usize,
        end: usize,
    },
    ValidateDescending {
        base: usize,
        end: usize,
    },
    Insertion {
        base: usize,
        target: usize,
        end: usize,
    },
    InsertionSearch {
        base: usize,
        target: usize,
        end: usize,
        left: usize,
        right: usize,
    },
    Collapse,
    ForceCollapse,
    Merge,
    Done,
}

pub(super) enum TimSortStep<T> {
    Done,
    NeedsComparison { left: T, right: T },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct InvalidOrder;

/// Incremental TimSort state. The implementation detects natural runs,
/// extends short runs with stable binary insertion sort, maintains TimSort's
/// run-stack invariants, and stably merges adjacent runs.
pub(super) struct TimSort<T> {
    values: Vec<T>,
    min_run: usize,
    next: usize,
    runs: Vec<Run>,
    phase: Phase,
    merge: Option<Merge<T>>,
}

impl<T: Clone> TimSort<T> {
    pub(super) fn new(values: Vec<T>) -> Self {
        let min_run = min_run_length(values.len());
        let phase = if values.len() > 1 {
            Phase::StartRun
        } else {
            Phase::Done
        };
        Self {
            values,
            min_run,
            next: 0,
            runs: Vec::new(),
            phase,
            merge: None,
        }
    }

    pub(super) fn into_values(self) -> Vec<T> {
        self.values
    }

    pub(super) fn values(&self) -> &[T] {
        &self.values
    }

    /// Advance until another comparison is required or sorting is complete.
    /// `comparison` is the result requested by the preceding call.
    pub(super) fn step(
        &mut self,
        mut comparison: Option<bool>,
    ) -> Result<TimSortStep<T>, InvalidOrder> {
        loop {
            match self.phase {
                Phase::StartRun => {
                    if self.next == self.values.len() {
                        self.phase = Phase::ForceCollapse;
                    } else if self.next + 1 == self.values.len() {
                        self.finish_run(self.next, self.values.len());
                    } else {
                        self.phase = Phase::DetectDirection { base: self.next };
                    }
                }
                Phase::DetectDirection { base } => {
                    let descending = match comparison.take() {
                        Some(less) => less,
                        None => {
                            return Ok(self.compare_indices(base + 1, base));
                        }
                    };
                    self.phase = if descending {
                        Phase::ScanDescending {
                            base,
                            end: base + 2,
                        }
                    } else {
                        Phase::ScanAscending {
                            base,
                            end: base + 2,
                        }
                    };
                }
                Phase::ScanAscending { base, end } => {
                    if end == self.values.len() {
                        self.extend_or_finish_run(base, end);
                        continue;
                    }
                    let descends = match comparison.take() {
                        Some(less) => less,
                        None => return Ok(self.compare_indices(end, end - 1)),
                    };
                    if descends {
                        self.extend_or_finish_run(base, end);
                    } else {
                        self.phase = Phase::ScanAscending { base, end: end + 1 };
                    }
                }
                Phase::ScanDescending { base, end } => {
                    if end == self.values.len() {
                        if end - base > 2 {
                            self.phase = Phase::ValidateDescending { base, end };
                        } else {
                            self.values[base..end].reverse();
                            self.extend_or_finish_run(base, end);
                        }
                        continue;
                    }
                    let descends = match comparison.take() {
                        Some(less) => less,
                        None => return Ok(self.compare_indices(end, end - 1)),
                    };
                    if descends {
                        self.phase = Phase::ScanDescending { base, end: end + 1 };
                    } else {
                        self.values[base..end].reverse();
                        self.extend_or_finish_run(base, end);
                    }
                }
                Phase::ValidateDescending { base, end } => {
                    // A strict order cannot report both every adjacent pair
                    // in this run and its two endpoints as descending. This
                    // catches reflexive/always-true comparators before any
                    // values are moved, without adding a probe to the common
                    // two-element sort path.
                    let wraps = match comparison.take() {
                        Some(less) => less,
                        None => return Ok(self.compare_indices(base, end - 1)),
                    };
                    if wraps {
                        return Err(InvalidOrder);
                    }
                    self.values[base..end].reverse();
                    self.extend_or_finish_run(base, end);
                }
                Phase::Insertion { base, target, end } => {
                    if target == end {
                        self.finish_run(base, end);
                    } else {
                        self.phase = Phase::InsertionSearch {
                            base,
                            target,
                            end,
                            left: base,
                            right: target,
                        };
                    }
                }
                Phase::InsertionSearch {
                    base,
                    target,
                    end,
                    left,
                    right,
                } => {
                    if left == right {
                        self.values[left..=target].rotate_right(1);
                        self.phase = Phase::Insertion {
                            base,
                            target: target + 1,
                            end,
                        };
                        continue;
                    }
                    let middle = left + (right - left) / 2;
                    let before = match comparison.take() {
                        Some(less) => less,
                        None => return Ok(self.compare_indices(target, middle)),
                    };
                    self.phase = if before {
                        Phase::InsertionSearch {
                            base,
                            target,
                            end,
                            left,
                            right: middle,
                        }
                    } else {
                        Phase::InsertionSearch {
                            base,
                            target,
                            end,
                            left: middle + 1,
                            right,
                        }
                    };
                }
                Phase::Collapse => match collapse_index(&self.runs) {
                    Some(index) => self.begin_merge(index, false),
                    None => self.phase = Phase::StartRun,
                },
                Phase::ForceCollapse => {
                    if self.runs.len() <= 1 {
                        self.phase = Phase::Done;
                    } else {
                        let count = self.runs.len();
                        let index =
                            if count >= 3 && self.runs[count - 3].len < self.runs[count - 1].len {
                                count - 3
                            } else {
                                count - 2
                            };
                        self.begin_merge(index, true);
                    }
                }
                Phase::Merge => {
                    let merge = self
                        .merge
                        .as_mut()
                        .expect("merge phase without merge state");
                    if merge.left_index == merge.left.len()
                        || merge.right_index == merge.right.len()
                    {
                        self.finish_merge();
                        continue;
                    }
                    let take_right = match comparison.take() {
                        Some(less) => less,
                        None => {
                            return Ok(TimSortStep::NeedsComparison {
                                left: merge.right[merge.right_index].clone(),
                                right: merge.left[merge.left_index].clone(),
                            });
                        }
                    };
                    let value = if take_right {
                        let value = merge.right[merge.right_index].clone();
                        merge.right_index += 1;
                        value
                    } else {
                        // Equal elements come from the left run first, which
                        // is the stability guarantee.
                        let value = merge.left[merge.left_index].clone();
                        merge.left_index += 1;
                        value
                    };
                    self.values[merge.destination] = value;
                    merge.destination += 1;
                }
                Phase::Done => return Ok(TimSortStep::Done),
            }
        }
    }

    fn compare_indices(&self, left: usize, right: usize) -> TimSortStep<T> {
        TimSortStep::NeedsComparison {
            left: self.values[left].clone(),
            right: self.values[right].clone(),
        }
    }

    fn extend_or_finish_run(&mut self, base: usize, natural_end: usize) {
        let end = self.values.len().min(base + self.min_run);
        if natural_end < end {
            self.phase = Phase::Insertion {
                base,
                target: natural_end,
                end,
            };
        } else {
            self.finish_run(base, natural_end);
        }
    }

    fn finish_run(&mut self, base: usize, end: usize) {
        self.runs.push(Run {
            base,
            len: end - base,
        });
        self.next = end;
        self.phase = Phase::Collapse;
    }

    fn begin_merge(&mut self, run_index: usize, force_collapse: bool) {
        let left = self.runs[run_index];
        let right = self.runs[run_index + 1];
        debug_assert_eq!(left.base + left.len, right.base);
        self.merge = Some(Merge {
            run_index,
            left: self.values[left.base..left.base + left.len].to_vec(),
            right: self.values[right.base..right.base + right.len].to_vec(),
            left_index: 0,
            right_index: 0,
            destination: left.base,
            force_collapse,
        });
        self.phase = Phase::Merge;
    }

    fn finish_merge(&mut self) {
        let merge = self.merge.take().expect("merge state disappeared");
        let mut destination = merge.destination;
        for value in &merge.left[merge.left_index..] {
            self.values[destination] = value.clone();
            destination += 1;
        }
        for value in &merge.right[merge.right_index..] {
            self.values[destination] = value.clone();
            destination += 1;
        }

        let right = self.runs.remove(merge.run_index + 1);
        self.runs[merge.run_index].len += right.len;
        self.phase = if merge.force_collapse {
            Phase::ForceCollapse
        } else {
            Phase::Collapse
        };
    }
}

fn min_run_length(mut length: usize) -> usize {
    let mut remainder = 0;
    while length >= 64 {
        remainder |= length & 1;
        length >>= 1;
    }
    length + remainder
}

fn collapse_index(runs: &[Run]) -> Option<usize> {
    let count = runs.len();
    if count >= 3 && runs[count - 3].len <= runs[count - 2].len + runs[count - 1].len {
        Some(if runs[count - 3].len < runs[count - 1].len {
            count - 3
        } else {
            count - 2
        })
    } else if count >= 2 && runs[count - 2].len <= runs[count - 1].len {
        Some(count - 2)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sort_by<T: Clone>(values: Vec<T>, less: impl Fn(&T, &T) -> bool) -> Vec<T> {
        let mut sort = TimSort::new(values);
        let mut comparison = None;
        loop {
            match sort.step(comparison.take()).unwrap() {
                TimSortStep::Done => return sort.into_values(),
                TimSortStep::NeedsComparison { left, right } => {
                    comparison = Some(less(&left, &right));
                }
            }
        }
    }

    #[test]
    fn sorts_multiple_natural_runs() {
        let values = (0..160)
            .map(|index| (index * 37 + 11) % 53)
            .collect::<Vec<_>>();
        let mut expected = values.clone();
        expected.sort();
        assert_eq!(sort_by(values, |left, right| left < right), expected);
    }

    #[test]
    fn preserves_equal_value_order() {
        let values = vec![(2, 0), (1, 1), (2, 2), (1, 3), (2, 4)];
        let sorted = sort_by(values, |left, right| left.0 < right.0);
        assert_eq!(
            sorted.into_iter().map(|item| item.1).collect::<Vec<_>>(),
            vec![1, 3, 0, 2, 4]
        );
    }

    #[test]
    fn rejects_always_less_order_before_mutating_values() {
        let values = (1..=20).collect::<Vec<_>>();
        let mut sort = TimSort::new(values.clone());
        let mut comparison = None;
        loop {
            match sort.step(comparison.take()) {
                Ok(TimSortStep::NeedsComparison { .. }) => comparison = Some(true),
                Err(InvalidOrder) => break,
                Ok(TimSortStep::Done) => panic!("always-less comparator was accepted"),
            }
        }
        assert_eq!(sort.into_values(), values);
    }
}
