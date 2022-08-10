use std::collections::HashMap;

use priority_queue::PriorityQueue;
use slab::Slab;

use super::index_allocator::SlotId;

type Range = (SlotId, SlotId);

/// LazyPool maintains dirty ranges and clean slots.
/// To reduce madvise cost, we want to merge continues slots
/// and do madvise in batch.
#[derive(Clone, Debug)]
pub(crate) struct LazyPool {
    max_instances: usize,
    // slab id -> range
    dirty_ranges_slab: Slab<Range>,
    // begin -> slab id
    dirty_begin_mapping: HashMap<SlotId, usize>,
    // end -> slab id
    dirty_end_mapping: HashMap<SlotId, usize>,
    // slab id with priority len_hint
    dirty_ranges: PriorityQueue<usize, usize>,
    clean: Vec<SlotId>,
    // TODO: more fields for calling decommit, for example, how to calcalate
    // pointer and length.
}

impl LazyPool {
    /// Create LazyPool with given clean slots.
    pub(crate) fn new(ids: Vec<SlotId>, max_instances: usize) -> Self {
        Self {
            max_instances,
            dirty_ranges_slab: Slab::with_capacity(max_instances),
            dirty_begin_mapping: HashMap::new(),
            dirty_end_mapping: HashMap::new(),
            dirty_ranges: PriorityQueue::new(),
            clean: ids,
        }
    }

    /// Check if the LazyPool is empty.
    pub(crate) fn is_empty(&self) -> bool {
        self.clean.is_empty() && self.dirty_ranges.is_empty()
    }

    /// Alloc a clean slot id. Must make sure not empty.
    pub(crate) fn alloc(&mut self) -> SlotId {
        debug_assert!(!self.is_empty());

        // try to alloc from clean directly
        if let Some(id) = self.clean.pop() {
            return id;
        }
        // get largest range
        let (slab_id, _) = self.dirty_ranges.pop().unwrap();
        let (left, right) = self.dirty_ranges_slab.remove(slab_id);
        self.dirty_begin_mapping.remove(&left);
        self.dirty_end_mapping.remove(&right);

        // clean it with madvise
        // todo!();

        // put them to clean
        let ret = left;
        for id in left.0 + 1..=right.0 {
            self.clean.push(SlotId(id));
        }
        ret
    }

    /// Free a slot id.
    pub(crate) fn free(&mut self, index: SlotId) {
        let (mut slab_left, mut slab_right) = (None, None);
        // check prev and next
        if index.0 > 0 {
            let prev = SlotId(index.0 - 1);
            slab_left = self.dirty_end_mapping.remove(&prev);
        }

        if index.0 + 1 < self.max_instances {
            let next = SlotId(index.0 + 1);
            slab_right = self.dirty_begin_mapping.remove(&next);
        }

        match (slab_left, slab_right) {
            (None, None) => {
                // unable to merge
                let slab_id = self.dirty_ranges_slab.insert((index, index));
                self.dirty_begin_mapping.insert(index, slab_id);
                self.dirty_end_mapping.insert(index, slab_id);
                self.dirty_ranges.push(slab_id, 1);
            }
            (Some(slab_id), None) => {
                // merge with left
                self.dirty_end_mapping.insert(index, slab_id);
                let range = self.dirty_ranges_slab.get_mut(slab_id).unwrap();
                range.1 = index;
                let size = range.1 .0 - range.0 .0;
                if size & 0x1111 == 0 {
                    self.dirty_ranges.change_priority(&slab_id, size);
                }
            }
            (None, Some(slab_id)) => {
                // merge with right
                self.dirty_begin_mapping.insert(index, slab_id);
                let range = self.dirty_ranges_slab.get_mut(slab_id).unwrap();
                range.0 = index;
                let size = range.1 .0 - range.0 .0;
                if size & 0x1111 == 0 {
                    self.dirty_ranges.change_priority(&slab_id, size);
                }
            }
            (Some(left_slab_id), Some(right_slab_id)) => {
                // merge with left and right
                let right_range = self.dirty_ranges_slab.remove(right_slab_id);
                let range = self.dirty_ranges_slab.get_mut(left_slab_id).unwrap();
                range.1 = right_range.1;
                let size = range.1 .0 - range.0 .0;
                if size & 0x1111 == 0 {
                    self.dirty_ranges.change_priority(&left_slab_id, size);
                }
                self.dirty_ranges.remove(&right_slab_id);
            }
        }
    }
}
