//! state 読みの short-TTL + single-flight キャッシュ。
//!
//! デバイス名をキーに per-key の `tokio::sync::Mutex` を持ち、同一キーの読みを
//! 直列化する。TTL 内なら再 exec を省き、待機中に他リクエストが計算した値は
//! 共有する（single-flight）。TTL とバックエンド知識は呼び出し側の責務で、
//! この層は「文字列キー → 値 T」を短時間共有するだけ。

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// main.rs での結線は Task 3（App.state_cache として使用）。それまで非 test
// ビルドでは未構築/未使用のため dead_code が出る — 結線後にこの allow は外す。
#[allow(dead_code)]
struct Cached<T> {
    at: Instant,
    value: T,
}

/// 文字列キーごとに値 T を短時間共有する汎用キャッシュ。
// main.rs での結線は Task 3。それまで非 test ビルドでは未構築（dead_code）。
#[allow(dead_code)]
pub struct Cache<T> {
    #[allow(clippy::type_complexity)]
    slots: Mutex<HashMap<String, Arc<tokio::sync::Mutex<Option<Cached<T>>>>>>,
}

impl<T> Default for Cache<T> {
    fn default() -> Self {
        Cache {
            slots: Mutex::new(HashMap::new()),
        }
    }
}

#[allow(dead_code)] // main.rs での結線は Task 3。それまで非 test ビルドでは未使用。
impl<T: Clone + Send + 'static> Cache<T> {
    /// key の per-key ロックを取得（無ければ作る）。保持は一瞬。
    fn slot(&self, key: &str) -> Arc<tokio::sync::Mutex<Option<Cached<T>>>> {
        let mut slots = self.slots.lock().expect("cache slots poisoned");
        slots
            .entry(key.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(None)))
            .clone()
    }

    /// TTL 内 or 待機中に計算された値があればそれを返し、無ければ `fetch` を走らせる。
    /// `fetch` は `(値, キャッシュ可否)` を返す。キャッシュ可否が false の結果は保存しない。
    pub async fn get_or_fetch<F, Fut>(&self, key: &str, ttl: Duration, fetch: F) -> T
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = (T, bool)>,
    {
        let arrival = Instant::now();
        let slot = self.slot(key);
        let mut guard = slot.lock().await;

        if let Some(c) = guard.as_ref() {
            // TTL ヒット、または自分が待つ間に他リクエストが入れた値（single-flight 合流）。
            if c.at.elapsed() < ttl || c.at >= arrival {
                return c.value.clone();
            }
        }

        let (value, cacheable) = fetch().await;
        if cacheable {
            *guard = Some(Cached {
                at: Instant::now(),
                value: value.clone(),
            });
        }
        value
    }

    /// 確定値でキャッシュを上書きする（set 後の再取得結果用）。
    pub async fn store(&self, key: &str, value: T) {
        let slot = self.slot(key);
        let mut guard = slot.lock().await;
        *guard = Some(Cached {
            at: Instant::now(),
            value,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    #[tokio::test]
    async fn ttl_hit_skips_fetch() {
        let cache: Cache<u32> = Cache::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let ttl = Duration::from_millis(500);

        let c = calls.clone();
        let a = cache
            .get_or_fetch("k", ttl, || async move {
                c.fetch_add(1, Ordering::SeqCst);
                (7, true)
            })
            .await;
        let c = calls.clone();
        let b = cache
            .get_or_fetch("k", ttl, || async move {
                c.fetch_add(1, Ordering::SeqCst);
                (99, true)
            })
            .await;

        assert_eq!(a, 7);
        assert_eq!(b, 7, "TTL 内は 1 回目の値を返す");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "fetch は 1 回だけ");
    }

    #[tokio::test]
    async fn ttl_expiry_refetches() {
        let cache: Cache<u32> = Cache::default();
        let ttl = Duration::from_millis(30);
        let a = cache.get_or_fetch("k", ttl, || async { (1, true) }).await;
        tokio::time::sleep(Duration::from_millis(60)).await;
        let b = cache.get_or_fetch("k", ttl, || async { (2, true) }).await;
        assert_eq!(a, 1);
        assert_eq!(b, 2, "TTL 経過後は再 exec");
    }

    #[tokio::test]
    async fn non_cacheable_is_not_stored() {
        let cache: Cache<u32> = Cache::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let ttl = Duration::from_millis(500);
        for _ in 0..2 {
            let c = calls.clone();
            cache
                .get_or_fetch("k", ttl, || async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    (0, false) // cacheable=false（失敗相当）
                })
                .await;
        }
        assert_eq!(calls.load(Ordering::SeqCst), 2, "失敗はキャッシュされず毎回 fetch");
    }

    #[tokio::test]
    async fn single_flight_coalesces_with_zero_ttl() {
        // ttl=0 でも、同時に走る同一キー読みは 1 fetch に合流する。
        let cache: Arc<Cache<u32>> = Arc::new(Cache::default());
        let calls = Arc::new(AtomicUsize::new(0));
        let ttl = Duration::ZERO;

        let mut handles = vec![];
        for _ in 0..5 {
            let cache = cache.clone();
            let c = calls.clone();
            handles.push(tokio::spawn(async move {
                cache
                    .get_or_fetch("k", ttl, || async move {
                        // 1 発目が握っている間に他が到着するよう、少し待つ。
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        c.fetch_add(1, Ordering::SeqCst);
                        (42, true)
                    })
                    .await
            }));
        }
        for h in handles {
            assert_eq!(h.await.unwrap(), 42);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1, "同時読みは 1 fetch に合流");
    }

    #[tokio::test]
    async fn store_overwrites_for_subsequent_reads() {
        let cache: Cache<u32> = Cache::default();
        let ttl = Duration::from_millis(500);
        cache.store("k", 5).await;
        let v = cache
            .get_or_fetch("k", ttl, || async { (999, true) })
            .await;
        assert_eq!(v, 5, "store 済みの確定値を TTL 内は返す");
    }

    #[tokio::test]
    async fn distinct_keys_are_independent() {
        let cache: Cache<u32> = Cache::default();
        let ttl = Duration::from_millis(500);
        let a = cache.get_or_fetch("a", ttl, || async { (1, true) }).await;
        let b = cache.get_or_fetch("b", ttl, || async { (2, true) }).await;
        assert_eq!(a, 1);
        assert_eq!(b, 2);
    }
}
