//! Scoped thread workers for parallelizing independent I/O and CPU operations.
//!
//! Preserves deterministic sequential order by indexing each task and restoring
//! the exact input ordering in the final collected output.
//! Closures must be independent: side effects do not run in input order.
//! Workers are scoped per call, not pooled or bounded across concurrent calls.
//! This std-only helper does not apply `horizon_threads` scheduling hints.

use std::sync::{
    OnceLock,
    atomic::{AtomicUsize, Ordering},
};

/// The worker limit per call, honoring `HORIZON_WORKER_THREADS`
/// if provided, or falling back to `available_parallelism`.
pub fn worker_count() -> usize {
    std::env::var("HORIZON_WORKER_THREADS")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or_else(|| {
            static DEFAULT: OnceLock<usize> = OnceLock::new();
            *DEFAULT.get_or_init(|| std::thread::available_parallelism().map_or(1, |n| n.get()))
        })
}

/// Applies `f` to each item in `items` across scoped worker threads, collecting
/// into a `Vec<R>` preserving the exact original index order.
pub fn parallel_map_slice<T, R, F>(items: &[T], f: F) -> Vec<R>
where
    T: Sync,
    R: Send,
    F: Fn(&T) -> R + Sync + Send,
{
    parallel_map_slice_with_workers(items, f, worker_count())
}

/// [`parallel_map_slice`] with an explicit worker thread limit.
pub fn parallel_map_slice_with_workers<T, R, F>(items: &[T], f: F, workers: usize) -> Vec<R>
where
    T: Sync,
    R: Send,
    F: Fn(&T) -> R + Sync + Send,
{
    parallel_filter_map_slice_with_workers(items, |item| Some(f(item)), workers)
}

/// Applies `f` to each item in `items` across scoped worker threads, keeping
/// only items where `f` returns `Some(r)` in the exact original relative order.
pub fn parallel_filter_map_slice<T, R, F>(items: &[T], f: F) -> Vec<R>
where
    T: Sync,
    R: Send,
    F: Fn(&T) -> Option<R> + Sync + Send,
{
    parallel_filter_map_slice_with_workers(items, f, worker_count())
}

/// [`parallel_filter_map_slice`] with an explicit worker thread limit.
pub fn parallel_filter_map_slice_with_workers<T, R, F>(items: &[T], f: F, workers: usize) -> Vec<R>
where
    T: Sync,
    R: Send,
    F: Fn(&T) -> Option<R> + Sync + Send,
{
    let count = items.len();
    if count == 0 {
        return Vec::new();
    }
    if workers <= 1 || count < 4 {
        return items.iter().filter_map(&f).collect();
    }

    let batch_size = (count / workers.saturating_mul(4)).clamp(1, 32);
    let next_index = AtomicUsize::new(0);
    let thread_count = (workers - 1).min(count - 1);

    let mut results = std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(thread_count);
        for _ in 0..thread_count {
            let Ok(handle) = std::thread::Builder::new().spawn_scoped(scope, || {
                let mut local = Vec::new();
                loop {
                    let start = next_index.fetch_add(batch_size, Ordering::Relaxed);
                    if start >= count {
                        break;
                    }
                    let end = (start + batch_size).min(count);
                    for idx in start..end {
                        if let Some(val) = f(&items[idx]) {
                            local.push((idx, val));
                        }
                    }
                }
                local
            }) else {
                // The caller drains the queue even when another OS thread
                // cannot be created; do not turn a load into a panic.
                break;
            };
            handles.push(handle);
        }

        let mut all_results = Vec::with_capacity(count);
        loop {
            let start = next_index.fetch_add(batch_size, Ordering::Relaxed);
            if start >= count {
                break;
            }
            let end = (start + batch_size).min(count);
            for idx in start..end {
                if let Some(val) = f(&items[idx]) {
                    all_results.push((idx, val));
                }
            }
        }

        for handle in handles {
            let mut local = handle
                .join()
                .unwrap_or_else(|e| std::panic::resume_unwind(e));
            all_results.append(&mut local);
        }
        all_results
    });

    results.sort_unstable_by_key(|(idx, _)| *idx);
    results.into_iter().map(|(_, val)| val).collect()
}

/// Applies `f` to each owned item in `items` across scoped worker threads,
/// collecting into a `Vec<R>` preserving the exact original index order.
pub fn parallel_map<T, R, F>(items: Vec<T>, f: F) -> Vec<R>
where
    T: Send,
    R: Send,
    F: Fn(T) -> R + Sync + Send,
{
    parallel_map_with_workers(items, f, worker_count())
}

/// [`parallel_map`] with an explicit worker thread limit.
pub fn parallel_map_with_workers<T, R, F>(items: Vec<T>, f: F, workers: usize) -> Vec<R>
where
    T: Send,
    R: Send,
    F: Fn(T) -> R + Sync + Send,
{
    parallel_filter_map_with_workers(items, |item| Some(f(item)), workers)
}

/// Applies `f` to each owned item in `items` across scoped worker threads,
/// keeping only items where `f` returns `Some(r)` in the exact original order.
pub fn parallel_filter_map<T, R, F>(items: Vec<T>, f: F) -> Vec<R>
where
    T: Send,
    R: Send,
    F: Fn(T) -> Option<R> + Sync + Send,
{
    parallel_filter_map_with_workers(items, f, worker_count())
}

/// [`parallel_filter_map`] with an explicit worker thread limit.
pub fn parallel_filter_map_with_workers<T, R, F>(items: Vec<T>, f: F, workers: usize) -> Vec<R>
where
    T: Send,
    R: Send,
    F: Fn(T) -> Option<R> + Sync + Send,
{
    let count = items.len();
    if count == 0 {
        return Vec::new();
    }
    if workers <= 1 || count < 4 {
        return items.into_iter().filter_map(f).collect();
    }

    let batch_size = (count / workers.saturating_mul(4)).clamp(1, 32);
    let mut chunks = Vec::new();
    let mut current_idx = 0;
    let mut it = items.into_iter();
    while current_idx < count {
        let take = batch_size.min(count - current_idx);
        let batch: Vec<T> = it.by_ref().take(take).collect();
        chunks.push((current_idx, batch));
        current_idx += take;
    }
    chunks.reverse();

    let queue = std::sync::Mutex::new(chunks);
    let thread_count = (workers - 1).min(count - 1);

    let mut results = std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(thread_count);
        for _ in 0..thread_count {
            let Ok(handle) = std::thread::Builder::new().spawn_scoped(scope, || {
                let mut local = Vec::new();
                loop {
                    let next = queue.lock().unwrap_or_else(|e| e.into_inner()).pop();
                    let Some((start_idx, batch)) = next else {
                        break;
                    };
                    for (i, item) in batch.into_iter().enumerate() {
                        if let Some(val) = f(item) {
                            local.push((start_idx + i, val));
                        }
                    }
                }
                local
            }) else {
                break;
            };
            handles.push(handle);
        }

        let mut all_results = Vec::with_capacity(count);
        loop {
            let next = queue.lock().unwrap_or_else(|e| e.into_inner()).pop();
            let Some((start_idx, batch)) = next else {
                break;
            };
            for (i, item) in batch.into_iter().enumerate() {
                if let Some(val) = f(item) {
                    all_results.push((start_idx + i, val));
                }
            }
        }

        for handle in handles {
            let mut local = handle
                .join()
                .unwrap_or_else(|e| std::panic::resume_unwind(e));
            all_results.append(&mut local);
        }
        all_results
    });

    results.sort_unstable_by_key(|(idx, _)| *idx);
    results.into_iter().map(|(_, val)| val).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_limits_preserve_filtered_order_without_overflow() {
        let items: Vec<usize> = (0..9).collect();
        for workers in [0, 1, 2, 4, usize::MAX] {
            let select = |n: usize| n.is_multiple_of(2).then_some(n * 3);
            assert_eq!(
                parallel_filter_map_slice_with_workers(&items, |n| select(*n), workers),
                [0, 6, 12, 18, 24]
            );
            assert_eq!(
                parallel_filter_map_with_workers(items.clone(), select, workers),
                [0, 6, 12, 18, 24]
            );
        }
    }

    #[test]
    fn parallel_map_slice_preserves_order() {
        let numbers: Vec<usize> = (0..500).collect();
        let expected: Vec<String> = numbers.iter().map(|n| format!("item_{n}")).collect();
        let actual = parallel_map_slice(&numbers, |n| format!("item_{n}"));
        assert_eq!(actual, expected);
    }

    #[test]
    fn parallel_filter_map_slice_preserves_order() {
        let numbers: Vec<usize> = (0..500).collect();
        let expected: Vec<usize> = numbers.iter().copied().filter(|n| n % 3 == 0).collect();
        let actual = parallel_filter_map_slice(&numbers, |n| (n % 3 == 0).then_some(*n));
        assert_eq!(actual, expected);
    }

    #[test]
    fn parallel_map_owned_preserves_order() {
        let items: Vec<String> = (0..500).map(|i| format!("orig_{i}")).collect();
        let expected: Vec<String> = items.iter().map(|s| format!("{s}_modified")).collect();
        let actual = parallel_map(items, |s| format!("{s}_modified"));
        assert_eq!(actual, expected);
    }

    #[test]
    fn parallel_filter_map_owned_preserves_order() {
        let items: Vec<usize> = (0..500).collect();
        let expected: Vec<String> = items
            .iter()
            .filter(|n| *n % 2 == 0)
            .map(|n| format!("even_{n}"))
            .collect();
        let actual = parallel_filter_map(items, |n| (n % 2 == 0).then(|| format!("even_{n}")));
        assert_eq!(actual, expected);
    }

    #[test]
    fn empty_and_single_elements() {
        let empty: Vec<i32> = Vec::new();
        assert!(parallel_map_slice(&empty, |x| x * 2).is_empty());
        assert!(parallel_map(empty, |x| x * 2).is_empty());

        let single = vec![42];
        assert_eq!(parallel_map_slice(&single, |x| x * 2), vec![84]);
        assert_eq!(parallel_map(single, |x| x * 2), vec![84]);
    }

    #[test]
    fn explicit_worker_counts_preserve_resource_order() {
        let items: Vec<Vec<u8>> = (0..2000)
            .map(|i| {
                format!(
                    r#"{{"id":"item_{i:04}","type":"crafting_shaped","values":[1,2,3,4,{i}],"meta":{{"tag":"group_{}","active":true}}}}"#,
                    i % 10
                )
                .into_bytes()
            })
            .collect();

        let decode = |b: &Vec<u8>| -> (String, usize) {
            let s = std::str::from_utf8(b.as_slice()).unwrap();
            let mut sum = 0usize;
            for byte in s.bytes() {
                sum = sum.wrapping_add(byte as usize);
            }
            (s[7..16].to_owned(), sum)
        };

        // Output equivalence, not a startup or JSON decoding benchmark.
        let serial = parallel_map_slice_with_workers(&items, decode, 1);
        let parallel_2 = parallel_map_slice_with_workers(&items, decode, 2);
        let parallel_4 = parallel_map_slice_with_workers(&items, decode, 4);

        assert_eq!(serial, parallel_2);
        assert_eq!(serial, parallel_4);
    }
}
