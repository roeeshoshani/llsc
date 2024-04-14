#![cfg_attr(not(test), no_std)]

use core::sync::atomic::AtomicUsize;

const STORE_POISON_BIT: usize = 1;
const COND_STORE_POISON_BIT: usize = 2;
const POISON_BITS: usize = STORE_POISON_BIT | COND_STORE_POISON_BIT;

pub struct LlscUsize {
    value: AtomicUsize,
    counter: AtomicUsize,
}
impl LlscUsize {
    pub fn new(value: usize) -> Self {
        Self {
            value: AtomicUsize::new(value),
            counter: AtomicUsize::new(0),
        }
    }
    pub fn store(&self, new_value: usize) {
        assert_eq!(new_value & POISON_BITS, 0);

        // reads here will see:
        // value = old_value
        // counter = X

        self.counter
            .fetch_add(1, core::sync::atomic::Ordering::SeqCst);

        // reads here will see:
        // value = old_value
        // counter = X + 1

        self.value.store(
            new_value | STORE_POISON_BIT,
            core::sync::atomic::Ordering::SeqCst,
        );

        // reads here will see:
        // value = poisoned(new_value)
        // counter = X + 1

        self.counter
            .fetch_add(1, core::sync::atomic::Ordering::SeqCst);

        // reads here will see:
        // value = poisoned(new_value)
        // counter = X + 2

        let _ = self.value.compare_exchange(
            new_value | STORE_POISON_BIT,
            new_value,
            core::sync::atomic::Ordering::SeqCst,
            core::sync::atomic::Ordering::SeqCst,
        );

        // reads here will see:
        // value = new_value
        // counter = X + 2
    }
    pub fn load(&self) -> usize {
        loop {
            let value = self.value.load(core::sync::atomic::Ordering::SeqCst);
            if value & COND_STORE_POISON_BIT == 0 {
                return value & (!STORE_POISON_BIT);
            }
        }
    }
    pub fn load_link(&self) -> LoadLink {
        loop {
            let result = LoadLink {
                counter: self.counter.load(core::sync::atomic::Ordering::SeqCst),
                raw_value: self.value.load(core::sync::atomic::Ordering::SeqCst),
            };
            if result.raw_value & COND_STORE_POISON_BIT == 0 {
                return result;
            }
        }
    }
    /// returns whether the store was successfull
    pub fn store_conditional(&self, link: LoadLink, new_value: usize) -> bool {
        // write the new value if the value looks like it didn't change
        if self
            .value
            .compare_exchange(
                link.raw_value,
                new_value | COND_STORE_POISON_BIT,
                core::sync::atomic::Ordering::SeqCst,
                core::sync::atomic::Ordering::SeqCst,
            )
            .is_err()
        {
            return false;
        }
        // make sure that the counter was not modified to verify that the value actually didn't change.
        if self
            .counter
            .compare_exchange(
                link.counter,
                link.counter + 1,
                core::sync::atomic::Ordering::SeqCst,
                core::sync::atomic::Ordering::SeqCst,
            )
            .is_err()
        {
            // restore the overwritten value to cancel the write (only if it wasn't changed once again in between).
            let _ = self.value.compare_exchange(
                new_value | COND_STORE_POISON_BIT,
                link.raw_value,
                core::sync::atomic::Ordering::SeqCst,
                core::sync::atomic::Ordering::SeqCst,
            );
            return false;
        }
        // unpoison it (only if it wasn't changed once again in between)
        let _ = self.value.compare_exchange(
            new_value | COND_STORE_POISON_BIT,
            new_value,
            core::sync::atomic::Ordering::SeqCst,
            core::sync::atomic::Ordering::SeqCst,
        );
        true
    }
}

pub struct LoadLink {
    raw_value: usize,
    counter: usize,
}
impl LoadLink {
    pub fn value(&self) -> usize {
        self.raw_value & (!STORE_POISON_BIT)
    }
}

#[cfg(test)]
mod tests {
    use core::{sync::atomic::AtomicBool, time::Duration};
    use std::{sync::Arc, thread::JoinHandle};

    use super::*;

    #[test]
    fn store_load() {
        let llsc = LlscUsize::new(0);
        for i in 0..3 {
            llsc.store(i * 4);
            assert_eq!(llsc.load(), i * 4);
        }
    }

    #[test]
    fn llsc_no_interruption() {
        let llsc = LlscUsize::new(0);
        for i in 0..3 {
            let link = llsc.load_link();
            assert_eq!(link.value(), i * 4);
            assert!(llsc.store_conditional(link, (i + 1) * 4));
        }
    }

    #[test]
    fn llsc_sync_store_same_value_interruption() {
        let llsc = LlscUsize::new(0);
        for i in 0..3 {
            let link = llsc.load_link();
            assert_eq!(link.value(), i * 4);
            llsc.store(i * 4);
            assert!(!llsc.store_conditional(link, (i + 1) * 4));
            llsc.store((i + 1) * 4);
        }
    }

    #[test]
    fn llsc_sync_store_different_value_interruption() {
        let llsc = LlscUsize::new(0);
        for i in 0..3 {
            let link = llsc.load_link();
            assert_eq!(link.value(), i * 4);
            llsc.store((i + 1) * 4);
            assert!(!llsc.store_conditional(link, (i + 1) * 4));
        }
    }

    #[test]
    fn llsc_sync_llsc_same_value_interruption() {
        let llsc = LlscUsize::new(0);
        for i in 0..3 {
            let link = llsc.load_link();
            assert_eq!(link.value(), i * 4);
            let sub_link = llsc.load_link();
            assert!(llsc.store_conditional(sub_link, i * 4));
            assert!(!llsc.store_conditional(link, (i + 1) * 4));
            llsc.store((i + 1) * 4);
        }
    }

    #[test]
    fn llsc_sync_llsc_different_value_interruption() {
        let llsc = LlscUsize::new(0);
        for i in 0..3 {
            let link = llsc.load_link();
            assert_eq!(link.value(), i * 4);
            let sub_link = llsc.load_link();
            assert!(llsc.store_conditional(sub_link, (i + 1) * 4));
            assert!(!llsc.store_conditional(link, (i + 1) * 4));
        }
    }

    #[test]
    fn llsc_alternating_same_value() {
        let llsc = LlscUsize::new(0);
        for i in 0..3 {
            let link_a = llsc.load_link();
            assert_eq!(link_a.value(), i * 4);
            let link_b = llsc.load_link();
            assert_eq!(link_b.value(), i * 4);
            assert!(llsc.store_conditional(link_a, i * 4));
            assert!(!llsc.store_conditional(link_b, i * 4));
            llsc.store((i + 1) * 4);
        }
    }

    #[test]
    fn llsc_alternating_different_value() {
        let llsc = LlscUsize::new(0);
        for i in 0..3 {
            let link_a = llsc.load_link();
            assert_eq!(link_a.value(), i * 4);
            let link_b = llsc.load_link();
            assert_eq!(link_b.value(), i * 4);
            assert!(llsc.store_conditional(link_a, (i + 1) * 4));
            assert!(!llsc.store_conditional(link_b, i * 4));
        }
    }

    #[test]
    fn llsc_failed_store_not_visible_sync_same_value() {
        let llsc = LlscUsize::new(0);
        for i in 0..3 {
            let link = llsc.load_link();
            assert_eq!(link.value(), i * 4);
            llsc.store(i * 4);
            assert!(!llsc.store_conditional(link, (i + 1) * 4));
            assert_eq!(llsc.load(), i * 4);
            llsc.store((i + 1) * 4);
        }
    }

    #[test]
    fn llsc_failed_store_not_visible_sync_different_value() {
        let llsc = LlscUsize::new(0);
        for i in 0..3 {
            let link = llsc.load_link();
            assert_eq!(link.value(), i * 4);
            llsc.store((i + 1) * 4);
            assert!(!llsc.store_conditional(link, i * 4));
            assert_eq!(llsc.load(), (i + 1) * 4);
        }
    }

    struct AsyncLlscTester {
        llsc: Arc<LlscUsize>,
        stop_running: Arc<AtomicBool>,
        threads: Vec<JoinHandle<()>>,
    }
    impl AsyncLlscTester {
        fn new(value: usize) -> Self {
            Self {
                llsc: Arc::new(LlscUsize::new(value)),
                stop_running: Arc::new(AtomicBool::new(false)),
                threads: Vec::new(),
            }
        }
        fn spawn_loop<F: FnMut(&LlscUsize) + Send + 'static>(&mut self, mut f: F) {
            let llsc = self.llsc.clone();
            let stop_running = self.stop_running.clone();
            let thread = std::thread::spawn(move || {
                while !stop_running.load(core::sync::atomic::Ordering::Relaxed) {
                    f(&llsc)
                }
            });
            self.threads.push(thread);
        }
        fn run_for_duration(&mut self, duration: Duration) {
            std::thread::sleep(duration);
            self.stop_running
                .store(true, core::sync::atomic::Ordering::Relaxed);
            for t in self.threads.drain(..) {
                t.join().unwrap()
            }
        }
        fn run_for_default_duration(&mut self) {
            self.run_for_duration(Duration::from_secs(3));
        }
    }

    #[test]
    fn llsc_failed_store_not_visible_async() {
        let mut llsc_tester = AsyncLlscTester::new(0);
        llsc_tester.spawn_loop(|llsc| llsc.store(4));
        llsc_tester.spawn_loop(|llsc| {
            let link = llsc.load_link();
            std::thread::sleep(Duration::from_millis(100));
            assert!(!llsc.store_conditional(link, 8));
        });
        llsc_tester.spawn_loop(|llsc| assert_ne!(llsc.load(), 8));
        llsc_tester.spawn_loop(|llsc| {
            let link = llsc.load_link();
            assert_ne!(link.value(), 8)
        });
        llsc_tester.run_for_default_duration()
    }
}
