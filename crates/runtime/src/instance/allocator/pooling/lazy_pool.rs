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
    stack_size: usize,
    base: usize,

    // slab id -> range
    dirty_ranges_slab: Slab<Range>,
    // begin -> slab id
    dirty_begin_mapping: Vec<Option<usize>>,
    // end -> slab id
    dirty_end_mapping: Vec<Option<usize>>,
    // slab id with priority len_hint
    dirty_ranges: PriorityQueue<usize, usize>,
    clean: Vec<SlotId>,
}

impl LazyPool {
    /// Create LazyPool with given clean slots.
    pub(crate) fn new(
        ids: Vec<SlotId>,
        max_instances: usize,
        stack_size: usize,
        base: usize,
    ) -> Self {
        Self {
            max_instances,
            stack_size,
            base,

            dirty_ranges_slab: Slab::with_capacity(max_instances),
            dirty_begin_mapping: vec![None; max_instances],
            dirty_end_mapping: vec![None; max_instances],
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
        self.dirty_begin_mapping[left.0] = None;
        self.dirty_end_mapping[right.0] = None;

        // clean it with madvise
        let begin = left.0 * self.stack_size + self.base;
        let len = (right.0 + 1 - left.0) * self.stack_size;
        let tick = std::time::Instant::now();
        crate::instance::allocator::pooling::decommit_stack_pages(begin as *mut u8, len).unwrap();
        // println!("DEBUG: decommit in batch size {}, time {}ms", right.0 + 1 - left.0, tick.elapsed().as_millis());

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
            let prev = index.0 - 1;
            slab_left = self.dirty_end_mapping[prev].take();
        }

        if index.0 + 1 < self.max_instances {
            let next = index.0 + 1;
            slab_right = self.dirty_begin_mapping[next].take();
        }

        match (slab_left, slab_right) {
            (None, None) => {
                // unable to merge
                let slab_id = self.dirty_ranges_slab.insert((index, index));
                self.dirty_begin_mapping[index.0] = Some(slab_id);
                self.dirty_end_mapping[index.0] = Some(slab_id);
                self.dirty_ranges.push(slab_id, 1);
            }
            (Some(slab_id), None) => {
                // merge with left
                self.dirty_end_mapping[index.0] = Some(slab_id);
                let range = unsafe { self.dirty_ranges_slab.get_unchecked_mut(slab_id) };
                range.1 = index;
                let size = range.1 .0 - range.0 .0;
                if size & 0x11111 == 0 {
                    self.dirty_ranges.change_priority(&slab_id, size);
                }
            }
            (None, Some(slab_id)) => {
                // merge with right
                self.dirty_begin_mapping[index.0] = Some(slab_id);
                let range = unsafe { self.dirty_ranges_slab.get_unchecked_mut(slab_id) };
                range.0 = index;
                let size = range.1 .0 - range.0 .0;
                if size & 0x11111 == 0 {
                    self.dirty_ranges.change_priority(&slab_id, size);
                }
            }
            (Some(left_slab_id), Some(right_slab_id)) => {
                // merge with left and right
                let right_range = self.dirty_ranges_slab.remove(right_slab_id);
                let range = unsafe { self.dirty_ranges_slab.get_unchecked_mut(left_slab_id) };
                range.1 = right_range.1;
                let size = range.1 .0 - range.0 .0;
                if size & 0x11111 == 0 {
                    self.dirty_ranges.change_priority(&left_slab_id, size);
                }
                self.dirty_ranges.remove(&right_slab_id);
            }
        }
    }
}
