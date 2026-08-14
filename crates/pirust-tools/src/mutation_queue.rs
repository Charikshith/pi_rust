//! Port of `core/tools/file-mutation-queue.ts` — per-path write serialization
//! used by `edit` and `write`.
//!
//! # Faithful global state
//!
//! Pi keeps two pieces of *module-global* state (file-mutation-queue.ts:4-5): a
//! `Map<string, Promise<void>>` holding the tail of each file's queue, and a
//! single `registrationQueue` promise chain. This port keeps that state global
//! too — deliberately, not by accident. `createEditTool` / `createWriteTool`
//! build independent tool objects, yet a write must still queue behind an edit
//! of the same file (test/file-mutation-queue.test.ts:131 "shares the queue
//! between edit and write"), so the queue cannot live on a tool instance.
//!
//! # How the promise chain is modelled
//!
//! Pi's chain (file-mutation-queue.ts:35-42) is:
//!
//! ```text
//! currentQueue = map.get(key) ?? resolved      // predecessor's tail
//! chainedQueue = currentQueue.then(() => nextQueue)
//! map.set(key, chainedQueue)
//! ```
//!
//! Each tail is awaited by exactly one successor (Pi reads the entry once per
//! registrant and immediately overwrites it), so the Rust tail is a single
//! [`oneshot::Receiver`] that fires when the owning registrant's `QueueSlot`
//! is dropped. `chainedQueue = currentQueue.then(...)` composition is not
//! reproduced literally because it is unobservable: a registrant only reaches
//! its own release after awaiting its predecessor, so "wait for my predecessor's
//! tail" already transitively covers every earlier registrant.
//!
//! `REGISTRATION_QUEUE` is the `registrationQueue` chain: key computation
//! (async, because of `realpath`) plus slot acquisition happen while it is held,
//! which is what makes acquisition atomic (file-mutation-queue.ts:33-51).
//! `tokio::sync::Mutex` is FIFO-fair, which is what reproduces the
//! promise-chain's arrival ordering. Pi swallows registration errors so a bad
//! registration cannot poison the chain (file-mutation-queue.ts:46-49); the
//! Rust equivalent is free — the guard is released by RAII on the `?` path, and
//! `tokio::sync::Mutex` has no poisoning at all.
//!
//! # Panic / error safety
//!
//! Pi releases the slot in a `finally` (file-mutation-queue.ts:55-60). This port
//! uses a **guard type**, not an explicit `catch_unwind`: `QueueSlot``::drop`
//! performs both the release and the map cleanup, so the chain survives an
//! operation that returns `Err`, panics, or (see below) is cancelled. There is
//! no `catch_unwind` anywhere — the panic keeps propagating exactly as Pi's
//! rejection does, it just cannot wedge the queue.
//!
//! # Documented divergences from Pi
//!
//! 1. **Ordering is by first poll, not by call site.** TS promises start
//!    eagerly, so in Pi the registration order is the order the calls are
//!    written. Rust futures are lazy, so the order here is the order the
//!    returned futures are first polled. `tokio::join!(a, b)` polls in argument
//!    order and therefore reproduces Pi's call-order behaviour; independently
//!    spawned tasks do not have a defined order in either language.
//! 2. **Dropping the future releases the slot.** A `Promise` cannot be
//!    cancelled, so Pi's `finally` always eventually runs. Here, dropping the
//!    returned future (including while queued behind a predecessor) runs
//!    `QueueSlot``::drop` early. This is strictly more robust and matches Pi
//!    on every path Pi can express. It is also why `edit`/`write` must keep
//!    checking their cancellation token *after* each await instead of racing the
//!    token against this future: cancellation must never release the slot while
//!    an in-flight filesystem operation could still complete
//!    (write.ts:204-210, edit.ts:313-315). Cancellation is deliberately **not**
//!    built into this API.
//! 3. **`resolve` is a local lexical port.** `resolve_from_cwd` reimplements
//!    Node's `path.resolve(filePath)` (file-mutation-queue.ts:17) rather than
//!    calling `std::path::absolute`, which does not collapse `..` on Unix.

use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex, MutexGuard};

use tokio::sync::oneshot;
use tokio::sync::Mutex as AsyncMutex;

// ===========================================================================
// Module-global state (file-mutation-queue.ts:4-5)
// ===========================================================================

/// Tail of one file's queue — Pi's `chainedQueue` map value
/// (file-mutation-queue.ts:41-42).
struct QueueTail {
    /// Identity of the registrant owning the tail, standing in for Pi's
    /// `get(key) === chainedQueue` reference comparison
    /// (file-mutation-queue.ts:57).
    id: u64,
    /// Pi's `nextQueue` (file-mutation-queue.ts:38-40): fires when the owning
    /// [`QueueSlot`] is dropped. `Err(RecvError)` — the sender dropped without
    /// sending — *is* the release signal, so awaiting it never fails.
    finished: oneshot::Receiver<()>,
}

/// Pi's `fileMutationQueues` (file-mutation-queue.ts:4).
///
/// Keyed by [`PathBuf`] rather than Pi's `string`: equality is the same
/// byte-wise comparison Node's string keys give, without a lossy UTF-8
/// conversion. `std::sync::Mutex` (not the async one) is load-bearing — every
/// critical section is a pair of `HashMap` operations with no await inside, so
/// [`QueueSlot`]`::drop` can do the `finally` work without an executor.
static FILE_MUTATION_QUEUES: LazyLock<Mutex<HashMap<PathBuf, QueueTail>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Pi's `registrationQueue` (file-mutation-queue.ts:5) — serializes key
/// computation + slot acquisition so the acquisition is atomic.
static REGISTRATION_QUEUE: LazyLock<AsyncMutex<()>> = LazyLock::new(|| AsyncMutex::new(()));

/// Source of [`QueueTail::id`] values.
static NEXT_SLOT_ID: AtomicU64 = AtomicU64::new(0);

/// Locks [`FILE_MUTATION_QUEUES`], recovering from poisoning.
///
/// A JS `Map` has no poisoning concept, so a panicking operation must not wedge
/// every later mutation. Nothing panics inside a critical section, so the
/// recovered map is always consistent.
fn lock_queues() -> MutexGuard<'static, HashMap<PathBuf, QueueTail>> {
    FILE_MUTATION_QUEUES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

// ===========================================================================
// Queue key derivation (file-mutation-queue.ts:7-26)
// ===========================================================================

/// Pi's `isMissingPathError` (file-mutation-queue.ts:7-14) — `ENOENT` /
/// `ENOTDIR`.
fn is_missing_path_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    )
}

/// Lexical port of Node's `path.resolve(filePath)` (file-mutation-queue.ts:17):
/// join against the process cwd, then collapse `.` / `..` / redundant
/// separators without touching the filesystem.
///
/// `Path::join` already reproduces Node's platform rules — an absolute argument
/// replaces the cwd entirely, and on Windows a rooted-but-prefixless argument
/// (`/tmp`) keeps the cwd's drive letter.
fn resolve_from_cwd(file_path: &Path) -> PathBuf {
    let joined = std::env::current_dir().unwrap_or_default().join(file_path);
    let mut resolved = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::CurDir => {}
            // `..` above the root is dropped, as in Node's resolve.
            Component::ParentDir => {
                resolved.pop();
            }
            other => resolved.push(other.as_os_str()),
        }
    }
    resolved
}

/// Pi's `getMutationQueueKey` (file-mutation-queue.ts:16-26): `realpath` of the
/// resolved path, falling back to the resolved path itself when the path does
/// not exist yet. Any other error propagates.
async fn mutation_queue_key(file_path: &Path) -> io::Result<PathBuf> {
    let resolved_path = resolve_from_cwd(file_path);
    match tokio::fs::canonicalize(&resolved_path).await {
        Ok(real_path) => Ok(real_path),
        Err(error) if is_missing_path_error(&error) => Ok(resolved_path),
        Err(error) => Err(error),
    }
}

// ===========================================================================
// Queue slot (the `finally` of file-mutation-queue.ts:55-60)
// ===========================================================================

/// Ownership of one file's queue slot. Dropping it is Pi's `finally`.
struct QueueSlot {
    key: PathBuf,
    id: u64,
    /// Pi's `releaseNext` (file-mutation-queue.ts:37-40). `Option` only so the
    /// release can be ordered before the map cleanup, as in Pi.
    release: Option<oneshot::Sender<()>>,
}

impl Drop for QueueSlot {
    fn drop(&mut self) {
        // file-mutation-queue.ts:56 `releaseNext()` — dropping the sender wakes
        // the successor waiting on this slot's tail.
        drop(self.release.take());
        // file-mutation-queue.ts:57-59 `if (get(key) === chainedQueue) delete`:
        // only the *last* registrant clears the entry, so a queued successor is
        // never orphaned while an idle file leaves nothing behind.
        let mut queues = lock_queues();
        if queues.get(&self.key).is_some_and(|tail| tail.id == self.id) {
            queues.remove(&self.key);
        }
    }
}

/// Pi's `registration` step (file-mutation-queue.ts:33-51): compute the key and
/// claim the tail of that key's queue, all under the global registration chain.
///
/// Returns the slot (release it by dropping) and the predecessor's tail, if any.
async fn register(file_path: &Path) -> io::Result<(QueueSlot, Option<oneshot::Receiver<()>>)> {
    // Held across the `realpath` await — that is the entire point of Pi's
    // `registrationQueue`. Released by RAII even on the `?` path below, which is
    // Pi's `registration.then(() => undefined, () => undefined)`.
    let _registration = REGISTRATION_QUEUE.lock().await;

    let key = mutation_queue_key(file_path).await?;
    let id = NEXT_SLOT_ID.fetch_add(1, Ordering::Relaxed);
    let (release, finished) = oneshot::channel();

    // `insert` returning the old value is Pi's `get(key) ?? resolved` followed
    // by `set(key, chainedQueue)` (file-mutation-queue.ts:35-42).
    let predecessor = lock_queues()
        .insert(key.clone(), QueueTail { id, finished })
        .map(|tail| tail.finished);

    Ok((
        QueueSlot {
            key,
            id,
            release: Some(release),
        },
        predecessor,
    ))
}

// ===========================================================================
// Public API (file-mutation-queue.ts:28-61)
// ===========================================================================

/// Serialize file mutation operations targeting the same file. Operations for
/// different files still run in parallel.
///
/// Port of `withFileMutationQueue` (file-mutation-queue.ts:28-61). Both Pi
/// callers wrap their whole read-modify-write body in it:
///
/// ```ignore
/// return withFileMutationQueue(absolutePath, async () => { /* ... */ });   // write.ts:203
/// ```
///
/// The Rust shape mirrors that single error channel: the operation's own failure
/// and a propagating `realpath` error (file-mutation-queue.ts:24) both surface
/// as `Err`, which is why `E: From<std::io::Error>` is required.
/// `pirust_agent_core::types::ToolError` satisfies it, so `edit`/`write` can
/// write `with_file_mutation_queue(&absolute_path, || async move { .. }).await`
/// as the tail of `execute`.
///
/// Cancellation is intentionally absent from this signature; see the
/// module-level "Documented divergences" §2.
pub async fn with_file_mutation_queue<T, E, F, Fut>(
    file_path: impl AsRef<Path>,
    operation: F,
) -> Result<T, E>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<T, E>>,
    E: From<io::Error>,
{
    let (slot, predecessor) = register(file_path.as_ref()).await?;

    // file-mutation-queue.ts:52 `await currentQueue`. Held *with* the slot, so
    // dropping this future while queued still heals the chain.
    if let Some(finished) = predecessor {
        let _ = finished.await;
    }

    // file-mutation-queue.ts:53-60. The explicit `drop` documents the ordering;
    // `QueueSlot::drop` is what covers the `Err` and panic paths.
    let result = operation().await;
    drop(slot);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    /// Hang guard only — never the mechanism of proof. Every interleaving below
    /// is forced with channels; this just turns a deadlock into a failure.
    const HANG_GUARD: Duration = Duration::from_secs(10);

    type Log = Arc<Mutex<Vec<String>>>;

    fn log() -> Log {
        Arc::new(Mutex::new(Vec::new()))
    }

    fn push(log: &Log, entry: &str) {
        log.lock().unwrap().push(entry.to_string());
    }

    fn entries(log: &Log) -> Vec<String> {
        log.lock().unwrap().clone()
    }

    /// Id of the registrant currently owning `key`'s tail, or `None` when the
    /// map entry has been cleaned up.
    fn tail_id(key: &Path) -> Option<u64> {
        lock_queues().get(key).map(|tail| tail.id)
    }

    /// Yields until `key`'s tail owner differs from `previous`, i.e. until a
    /// later caller has finished the registration step and is blocked on its
    /// predecessor. Deterministic: the awaited task is already runnable, so the
    /// loop only has to let the executor make progress.
    async fn await_tail_change(key: &Path, previous: Option<u64>) {
        let changed = async {
            while tail_id(key) == previous {
                tokio::task::yield_now().await;
            }
        };
        tokio::time::timeout(HANG_GUARD, changed)
            .await
            .expect("a later registrant should have claimed the tail");
    }

    /// Joins a test task under the hang guard, so a slot that is never released
    /// surfaces as a failure instead of a deadlock.
    async fn join<T>(handle: tokio::task::JoinHandle<T>) -> Result<T, tokio::task::JoinError> {
        tokio::time::timeout(HANG_GUARD, handle)
            .await
            .expect("queued task should finish")
    }

    async fn recv(rx: oneshot::Receiver<()>) {
        tokio::time::timeout(HANG_GUARD, rx)
            .await
            .expect("signal should arrive")
            .expect("sender should not be dropped");
    }

    /// Unix only: creating a symlink on Windows needs Developer Mode or
    /// elevation, so the Windows-side alias test uses a junction instead.
    #[cfg(unix)]
    fn symlink_file(target: &Path, link: &Path) -> io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    /// Ports `test/file-mutation-queue.test.ts:38` "serializes operations for
    /// the same file", and additionally pins the `get(key) === chainedQueue`
    /// cleanup rule (file-mutation-queue.ts:57-59): the entry must survive the
    /// first registrant's release and disappear only after the last one.
    #[tokio::test]
    async fn serializes_operations_for_the_same_file() {
        let dir = tempfile::tempdir().unwrap();
        // Missing path => key is the resolved path (file-mutation-queue.ts:21-22).
        let path = dir.path().join("same.txt");
        let order = log();

        let (first_in_tx, first_in_rx) = oneshot::channel();
        let (release_first_tx, release_first_rx) = oneshot::channel();
        let first = tokio::spawn({
            let order = order.clone();
            let path = path.clone();
            async move {
                with_file_mutation_queue::<_, io::Error, _, _>(path, || async {
                    push(&order, "first:start");
                    first_in_tx.send(()).unwrap();
                    let _ = release_first_rx.await;
                    push(&order, "first:end");
                    Ok(())
                })
                .await
                .unwrap();
            }
        });

        recv(first_in_rx).await;
        let first_id = tail_id(&path);
        assert!(first_id.is_some(), "first registrant should own the tail");

        let (second_in_tx, second_in_rx) = oneshot::channel();
        let (release_second_tx, release_second_rx) = oneshot::channel();
        let second = tokio::spawn({
            let order = order.clone();
            let path = path.clone();
            async move {
                with_file_mutation_queue::<_, io::Error, _, _>(path, || async {
                    push(&order, "second:start");
                    second_in_tx.send(()).unwrap();
                    let _ = release_second_rx.await;
                    push(&order, "second:end");
                    Ok(())
                })
                .await
                .unwrap();
            }
        });

        // Second has registered and is now blocked on the first's tail.
        await_tail_change(&path, first_id).await;
        let second_id = tail_id(&path);
        assert_ne!(first_id, second_id);
        assert_eq!(
            entries(&order),
            vec!["first:start"],
            "second must not run while the first holds the slot"
        );

        release_first_tx.send(()).unwrap();
        recv(second_in_rx).await;
        // file-mutation-queue.ts:57-59: the first registrant released, but the
        // entry belongs to the second now, so it must still be there.
        assert_eq!(tail_id(&path), second_id);

        release_second_tx.send(()).unwrap();
        join(first).await.unwrap();
        join(second).await.unwrap();

        assert_eq!(
            entries(&order),
            vec!["first:start", "first:end", "second:start", "second:end"]
        );
        assert_eq!(tail_id(&path), None, "settled queue must leave no entry");
    }

    /// Ports `test/file-mutation-queue.test.ts:56` "allows different files to
    /// proceed in parallel". Pi asserts `b:start` precedes `a:end`; holding both
    /// operations inside their bodies simultaneously is the airtight form of
    /// that claim.
    #[tokio::test]
    async fn allows_different_files_to_proceed_in_parallel() {
        let dir = tempfile::tempdir().unwrap();
        let path_a = dir.path().join("a.txt");
        let path_b = dir.path().join("b.txt");
        let order = log();

        let (a_in_tx, a_in_rx) = oneshot::channel();
        let (b_in_tx, b_in_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel::<()>();
        let release = Arc::new(tokio::sync::Notify::new());

        let notify_on_release = {
            let release = release.clone();
            tokio::spawn(async move {
                let _ = release_rx.await;
                release.notify_waiters();
            })
        };

        let run = |path: PathBuf, name: &'static str, in_tx: oneshot::Sender<()>| {
            let order = order.clone();
            let release = release.clone();
            tokio::spawn(async move {
                with_file_mutation_queue::<_, io::Error, _, _>(path, || async {
                    push(&order, &format!("{name}:start"));
                    let waiter = release.notified();
                    in_tx.send(()).unwrap();
                    waiter.await;
                    push(&order, &format!("{name}:end"));
                    Ok(())
                })
                .await
                .unwrap();
            })
        };

        let a = run(path_a.clone(), "a", a_in_tx);
        let b = run(path_b.clone(), "b", b_in_tx);

        // Both bodies are executing at the same time => different keys do not
        // serialize.
        recv(a_in_rx).await;
        recv(b_in_rx).await;
        let started = entries(&order);
        assert!(started.contains(&"a:start".to_string()));
        assert!(started.contains(&"b:start".to_string()));
        assert!(
            !started.iter().any(|entry| entry.ends_with(":end")),
            "neither operation has finished yet"
        );

        release_tx.send(()).unwrap();
        join(a).await.unwrap();
        join(b).await.unwrap();
        join(notify_on_release).await.unwrap();

        let final_order = entries(&order);
        for name in ["a", "b"] {
            let start = final_order
                .iter()
                .position(|e| e == &format!("{name}:start"))
                .unwrap();
            let end = final_order
                .iter()
                .position(|e| e == &format!("{name}:end"))
                .unwrap();
            assert!(start < end);
        }
        assert_eq!(tail_id(&path_a), None);
        assert_eq!(tail_id(&path_b), None);
    }

    /// Body of `test/file-mutation-queue.test.ts:77` "uses the same queue for
    /// symlink aliases": two spellings of the same *existing* file collapse onto
    /// one queue because the key is `realpath`'d (file-mutation-queue.ts:19).
    ///
    /// `target_path` runs first and is held open until `alias_path` has
    /// registered, so the assertion is on the queue, never on timing. The two
    /// spellings must be lexically distinct (otherwise plain `resolve` would
    /// explain the collapse) and must canonicalize to the same file.
    async fn assert_aliases_share_one_queue(target_path: PathBuf, symlink_path: PathBuf) {
        assert_ne!(target_path, symlink_path);
        // Independent expectation: the key is the canonical target, not either
        // spelling the callers passed.
        let key = std::fs::canonicalize(&target_path).unwrap();
        assert_eq!(key, std::fs::canonicalize(&symlink_path).unwrap());
        let order = log();

        let (target_in_tx, target_in_rx) = oneshot::channel();
        let (release_target_tx, release_target_rx) = oneshot::channel();
        let target = tokio::spawn({
            let order = order.clone();
            let target_path = target_path.clone();
            async move {
                with_file_mutation_queue::<_, io::Error, _, _>(target_path, || async {
                    push(&order, "target:start");
                    target_in_tx.send(()).unwrap();
                    let _ = release_target_rx.await;
                    push(&order, "target:end");
                    Ok(())
                })
                .await
                .unwrap();
            }
        });

        recv(target_in_rx).await;
        let target_id = tail_id(&key);
        assert!(
            target_id.is_some(),
            "queue must be keyed on the realpath, not the spelling"
        );

        let alias = tokio::spawn({
            let order = order.clone();
            async move {
                with_file_mutation_queue::<_, io::Error, _, _>(symlink_path, || async {
                    push(&order, "alias:start");
                    push(&order, "alias:end");
                    Ok(())
                })
                .await
                .unwrap();
            }
        });

        // The alias joined the *same* queue: it changed the tail of `key`.
        await_tail_change(&key, target_id).await;
        assert_eq!(entries(&order), vec!["target:start"]);

        release_target_tx.send(()).unwrap();
        join(target).await.unwrap();
        join(alias).await.unwrap();

        assert_eq!(
            entries(&order),
            vec!["target:start", "target:end", "alias:start", "alias:end"]
        );
        assert_eq!(tail_id(&key), None);
    }

    /// Direct port of `test/file-mutation-queue.test.ts:77` — a file symlink
    /// alias shares the target's queue.
    #[cfg(unix)]
    #[tokio::test]
    async fn uses_the_same_queue_for_symlink_aliases() {
        let dir = tempfile::tempdir().unwrap();
        let target_path = dir.path().join("target.txt");
        let symlink_path = dir.path().join("alias.txt");
        std::fs::write(&target_path, "hello\n").unwrap();
        symlink_file(&target_path, &symlink_path).unwrap();
        assert_aliases_share_one_queue(target_path, symlink_path).await;
    }

    /// Windows twin of the symlink-alias port. `symlink_file` needs Developer
    /// Mode or elevation, so the alias is a *directory junction* instead — also a
    /// reparse point, also resolved by `realpath`/`canonicalize`, but creatable
    /// unprivileged. Same assertion, so the behaviour is verified on Windows
    /// rather than skipped.
    #[cfg(windows)]
    #[tokio::test]
    async fn uses_the_same_queue_for_junction_aliases() {
        let dir = tempfile::tempdir().unwrap();
        let real_dir = dir.path().join("real");
        let junction_dir = dir.path().join("link");
        std::fs::create_dir(&real_dir).unwrap();
        let target_path = real_dir.join("target.txt");
        std::fs::write(&target_path, "hello\n").unwrap();

        let status = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&junction_dir)
            .arg(&real_dir)
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "mklink /J should not need elevation");

        assert_aliases_share_one_queue(target_path, junction_dir.join("target.txt")).await;
    }

    /// A path that does not exist yet keys on the *resolved* path
    /// (file-mutation-queue.ts:21-22), so two lexically different spellings of
    /// one missing file still share a queue.
    #[tokio::test]
    async fn missing_path_falls_back_to_the_resolved_key() {
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("missing.txt");
        // Same file, spelled through a `..` hop that `resolve` collapses.
        let indirect = dir.path().join("sub").join("..").join("missing.txt");
        assert_ne!(plain, indirect);
        let order = log();

        let (first_in_tx, first_in_rx) = oneshot::channel();
        let (release_first_tx, release_first_rx) = oneshot::channel();
        let first = tokio::spawn({
            let order = order.clone();
            let plain = plain.clone();
            async move {
                with_file_mutation_queue::<_, io::Error, _, _>(plain, || async {
                    push(&order, "plain:start");
                    first_in_tx.send(()).unwrap();
                    let _ = release_first_rx.await;
                    push(&order, "plain:end");
                    Ok(())
                })
                .await
                .unwrap();
            }
        });

        recv(first_in_rx).await;
        // Independent expectation: the key is exactly the resolved path.
        let first_id = tail_id(&plain);
        assert!(
            first_id.is_some(),
            "missing path must key on its resolved path"
        );

        let second = tokio::spawn({
            let order = order.clone();
            async move {
                with_file_mutation_queue::<_, io::Error, _, _>(indirect, || async {
                    push(&order, "indirect:start");
                    push(&order, "indirect:end");
                    Ok(())
                })
                .await
                .unwrap();
            }
        });

        await_tail_change(&plain, first_id).await;
        assert_eq!(entries(&order), vec!["plain:start"]);

        release_first_tx.send(()).unwrap();
        join(first).await.unwrap();
        join(second).await.unwrap();

        assert_eq!(
            entries(&order),
            vec!["plain:start", "plain:end", "indirect:start", "indirect:end"]
        );
        assert_eq!(tail_id(&plain), None);
    }

    /// The `finally` (file-mutation-queue.ts:55-60) releases the slot even when
    /// the operation fails, so a queued successor still runs.
    #[tokio::test]
    async fn releases_the_slot_when_the_operation_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("failing.txt");
        let order = log();

        let (first_in_tx, first_in_rx) = oneshot::channel();
        let (release_first_tx, release_first_rx) = oneshot::channel();
        let first = tokio::spawn({
            let order = order.clone();
            let path = path.clone();
            async move {
                with_file_mutation_queue::<(), io::Error, _, _>(path, || async {
                    push(&order, "first:start");
                    first_in_tx.send(()).unwrap();
                    let _ = release_first_rx.await;
                    Err(io::Error::other("operation failed"))
                })
                .await
            }
        });

        recv(first_in_rx).await;
        let first_id = tail_id(&path);

        let second = tokio::spawn({
            let order = order.clone();
            let path = path.clone();
            async move {
                with_file_mutation_queue::<_, io::Error, _, _>(path, || async {
                    push(&order, "second:ran");
                    Ok(())
                })
                .await
                .unwrap();
            }
        });

        await_tail_change(&path, first_id).await;
        release_first_tx.send(()).unwrap();

        let first_error = join(first)
            .await
            .unwrap()
            .expect_err("operation should fail");
        assert_eq!(first_error.to_string(), "operation failed");
        join(second).await.unwrap();

        assert_eq!(entries(&order), vec!["first:start", "second:ran"]);
        assert_eq!(tail_id(&path), None, "failed slot must be cleaned up");
    }

    /// Rust-only counterpart of the `finally`: unwinding out of the operation
    /// drops [`QueueSlot`], so a panic cannot break the chain either. The panic
    /// itself still propagates (no `catch_unwind`).
    #[tokio::test]
    async fn releases_the_slot_when_the_operation_panics() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("panicking.txt");
        let order = log();

        let (first_in_tx, first_in_rx) = oneshot::channel();
        let (release_first_tx, release_first_rx) = oneshot::channel();
        let first = tokio::spawn({
            let path = path.clone();
            async move {
                with_file_mutation_queue::<(), io::Error, _, _>(path, || async {
                    first_in_tx.send(()).unwrap();
                    let _ = release_first_rx.await;
                    panic!("operation panicked");
                })
                .await
            }
        });

        recv(first_in_rx).await;
        let first_id = tail_id(&path);

        let second = tokio::spawn({
            let order = order.clone();
            let path = path.clone();
            async move {
                with_file_mutation_queue::<_, io::Error, _, _>(path, || async {
                    push(&order, "second:ran");
                    Ok(())
                })
                .await
                .unwrap();
            }
        });

        await_tail_change(&path, first_id).await;
        release_first_tx.send(()).unwrap();

        assert!(
            join(first).await.unwrap_err().is_panic(),
            "the panic must keep propagating"
        );
        join(second).await.unwrap();

        assert_eq!(entries(&order), vec!["second:ran"]);
        assert_eq!(tail_id(&path), None, "panicked slot must be cleaned up");
    }

    /// `isMissingPathError` (file-mutation-queue.ts:7-14): only `ENOENT` and
    /// `ENOTDIR` fall back to the resolved path.
    #[test]
    fn missing_path_errors_are_classified_like_pi() {
        assert!(is_missing_path_error(&io::Error::from(
            io::ErrorKind::NotFound
        )));
        assert!(is_missing_path_error(&io::Error::from(
            io::ErrorKind::NotADirectory
        )));
        assert!(!is_missing_path_error(&io::Error::from(
            io::ErrorKind::PermissionDenied
        )));
        assert!(!is_missing_path_error(&io::Error::other("boom")));
    }

    /// Node's `path.resolve` semantics relied on at file-mutation-queue.ts:17.
    #[test]
    fn resolve_from_cwd_matches_node_path_resolve() {
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(resolve_from_cwd(Path::new("a.txt")), cwd.join("a.txt"));
        assert_eq!(resolve_from_cwd(Path::new("./a.txt")), cwd.join("a.txt"));
        assert_eq!(
            resolve_from_cwd(Path::new("sub/../a.txt")),
            cwd.join("a.txt")
        );
        assert_eq!(resolve_from_cwd(&cwd.join("a.txt")), cwd.join("a.txt"));
        assert_eq!(
            resolve_from_cwd(&cwd.join("sub").join("..").join("a.txt")),
            cwd.join("a.txt")
        );
        // `..` above the root is dropped rather than escaping it.
        let root = cwd.ancestors().last().unwrap().to_path_buf();
        assert_eq!(resolve_from_cwd(&root.join("..").join("a.txt")), {
            let mut expected = root.clone();
            expected.push("a.txt");
            expected
        });
    }

    /// A `realpath` failure that is not a missing path propagates
    /// (file-mutation-queue.ts:24) and, because Pi swallows registration
    /// failures (file-mutation-queue.ts:46-49), the global chain keeps working
    /// afterwards. Unix-only: a symlink cycle is the portable way to make
    /// `realpath` fail with something other than `ENOENT`/`ENOTDIR`.
    #[cfg(unix)]
    #[tokio::test]
    async fn non_missing_path_errors_propagate_without_poisoning_the_chain() {
        let dir = tempfile::tempdir().unwrap();
        let loop_a = dir.path().join("loop-a");
        let loop_b = dir.path().join("loop-b");
        symlink_file(&loop_b, &loop_a).unwrap();
        symlink_file(&loop_a, &loop_b).unwrap();

        let error = with_file_mutation_queue::<(), io::Error, _, _>(&loop_a, || async {
            panic!("operation must not run when registration fails");
        })
        .await
        .expect_err("a symlink cycle is not a missing-path error");
        assert!(!is_missing_path_error(&error), "got {error:?}");
        assert_eq!(tail_id(&loop_a), None, "no slot may be left behind");

        // Registration chain still usable.
        let path = dir.path().join("after.txt");
        with_file_mutation_queue::<_, io::Error, _, _>(&path, || async { Ok(()) })
            .await
            .unwrap();
        assert_eq!(tail_id(&path), None);
    }
}
