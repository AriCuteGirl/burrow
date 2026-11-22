//! Thumbnail queue on the UI side: a few workers, newest request first (what's on screen
//! now matters most), requests for cells that scrolled away are dropped, and a small
//! in-memory cache makes scrolling back instant.

use crate::thumbs::{self, Size, Source};
use gtk::{gdk, gio, glib};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};

struct Job {
    src: Source,
    size: Size,
    wanted: Box<dyn Fn() -> bool>,
    done: Box<dyn FnOnce(gdk::Texture)>,
}

type CacheKey = (String, i64, Size);

#[derive(Default)]
struct Queue {
    jobs: VecDeque<Job>,
    running: usize,
    cache: HashMap<CacheKey, gdk::Texture>,
    order: VecDeque<CacheKey>,
}

const CACHE: usize = 400;
const QUEUE: usize = 600;

thread_local! {
    static Q: RefCell<Queue> = RefCell::default();
}

fn workers() -> usize {
    std::thread::available_parallelism().map(|n| (n.get() / 2).clamp(1, 4)).unwrap_or(2)
}

fn key(s: &Source, size: Size) -> CacheKey {
    (s.uri.clone(), s.mtime, size)
}

/// `wanted` is asked again right before the work starts; `done` runs on the UI thread.
pub fn request(src: Source, size: Size, wanted: impl Fn() -> bool + 'static, done: impl FnOnce(gdk::Texture) + 'static) {
    let hit = Q.with_borrow(|q| q.cache.get(&key(&src, size)).cloned());
    if let Some(t) = hit {
        done(t);
        return;
    }
    Q.with_borrow_mut(|q| {
        q.jobs.push_front(Job { src, size, wanted: Box::new(wanted), done: Box::new(done) });
        q.jobs.truncate(QUEUE);
    });
    pump();
}

fn pump() {
    loop {
        let job = Q.with_borrow_mut(|q| {
            if q.running >= workers() {
                return None;
            }
            let j = q.jobs.pop_front()?;
            q.running += 1;
            Some(j)
        });
        let Some(job) = job else { return };
        if !(job.wanted)() {
            Q.with_borrow_mut(|q| q.running -= 1);
            continue;
        }
        glib::spawn_future_local(async move {
            let Job { src, size, wanted, done } = job;
            let k = key(&src, size);
            let png = gio::spawn_blocking(move || thumbs::get(&src, size)).await.ok().flatten();
            Q.with_borrow_mut(|q| q.running -= 1);
            if let Some(t) = png.and_then(|b| gdk::Texture::from_bytes(&glib::Bytes::from_owned(b)).ok()) {
                Q.with_borrow_mut(|q| {
                    if q.cache.insert(k.clone(), t.clone()).is_none() {
                        q.order.push_back(k);
                    }
                    while q.order.len() > CACHE {
                        if let Some(old) = q.order.pop_front() {
                            q.cache.remove(&old);
                        }
                    }
                });
                if wanted() {
                    done(t);
                }
            }
            pump();
        });
    }
}
