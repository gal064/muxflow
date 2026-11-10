//! Output that arrives while pane recovery is deciding its cutoff. Port of
//! `DeferredTerminalOutputQueue.ts`.

pub const MAX_DEFERRED_OUTPUT_BYTES: usize = 1024 * 1024;
pub const MAX_DEFERRED_OUTPUT_RECORDS: usize = 1024;

#[derive(Debug, PartialEq, Eq)]
pub struct DeferredOutput {
    pub data: Vec<u8>,
    pub generation: u64,
    pub terminal_epoch: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeferredOutputAdmission {
    Accepted,
    Overflow,
    Blocked,
}

/// Bytes alone are not a bound, because an empty record still retains a
/// generation and a slot.
#[derive(Default)]
pub struct DeferredTerminalOutputQueue {
    items: Vec<DeferredOutput>,
    bytes: usize,
    blocked: bool,
}

impl DeferredTerminalOutputQueue {
    pub fn enqueue(&mut self, item: DeferredOutput) -> DeferredOutputAdmission {
        if self.blocked {
            return DeferredOutputAdmission::Blocked;
        }
        if self.items.len() >= MAX_DEFERRED_OUTPUT_RECORDS
            || self.bytes + item.data.len() > MAX_DEFERRED_OUTPUT_BYTES
        {
            self.items = Vec::new();
            self.bytes = 0;
            self.blocked = true;
            return DeferredOutputAdmission::Overflow;
        }
        self.bytes += item.data.len();
        self.items.push(item);
        DeferredOutputAdmission::Accepted
    }

    pub fn drain(&mut self) -> Vec<DeferredOutput> {
        let items = std::mem::take(&mut self.items);
        self.reset();
        items
    }

    pub fn reset(&mut self) {
        self.items = Vec::new();
        self.bytes = 0;
        self.blocked = false;
    }

    pub fn record_count(&self) -> usize {
        self.items.len()
    }

    pub fn byte_length(&self) -> usize {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty(generation: u64) -> DeferredOutput {
        DeferredOutput {
            data: Vec::new(),
            generation,
            terminal_epoch: None,
        }
    }

    #[test]
    fn bounds_empty_recovery_records_and_latches_one_overflow_episode() {
        let mut queue = DeferredTerminalOutputQueue::default();
        for generation in 1..=MAX_DEFERRED_OUTPUT_RECORDS as u64 {
            assert_eq!(
                queue.enqueue(empty(generation)),
                DeferredOutputAdmission::Accepted
            );
        }
        assert_eq!(
            queue.enqueue(empty(1025)),
            DeferredOutputAdmission::Overflow
        );
        assert_eq!(queue.record_count(), 0);
        assert_eq!(queue.byte_length(), 0);
        assert_eq!(queue.enqueue(empty(1026)), DeferredOutputAdmission::Blocked);
    }

    #[test]
    fn resets_the_overflow_latch_only_at_a_new_authoritative_recovery_boundary() {
        let mut queue = DeferredTerminalOutputQueue::default();
        for generation in 0..=MAX_DEFERRED_OUTPUT_RECORDS as u64 {
            queue.enqueue(empty(generation));
        }
        queue.reset();
        assert_eq!(
            queue.enqueue(DeferredOutput {
                data: vec![1],
                generation: 1,
                terminal_epoch: None
            }),
            DeferredOutputAdmission::Accepted
        );
        assert_eq!(queue.drain().len(), 1);
    }
}
