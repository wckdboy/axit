use std::{
    collections::HashSet,
    fs,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use wckd_axit::{
    index::{self, Index, Progress},
    page::Request,
    query::{self, Query, Sort},
};
fn scan(root: &Path, state: &Path) -> Index {
    Index::open(
        &index::scan(
            root,
            state,
            Arc::new(Mutex::new(Progress::default())),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap(),
    )
    .unwrap()
}
fn fixture() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("files");
    let state = t.path().join("state");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&state).unwrap();
    (
        t,
        root.canonicalize().unwrap(),
        state.canonicalize().unwrap(),
    )
}
#[test]
fn every_result_is_reachable_in_both_directions_with_stable_ties_and_bounded_pages() {
    let (_t, root, state) = fixture();
    for n in 0..10057 {
        let p = root.join(format!("entry-{n:05}.txt"));
        let f = fs::File::create(p).unwrap();
        f.set_modified(UNIX_EPOCH + Duration::from_secs(1_600_000_000 + n % 7))
            .unwrap();
    }
    for name in ["a", "b"] {
        fs::create_dir(root.join(name)).unwrap();
        fs::write(root.join(name).join("same.txt"), b"same size").unwrap();
    }
    fs::write(root.join("entry-10055.txt"), vec![7; 16384]).unwrap();
    let index = scan(&root, &state);
    let q = Query::parse("ext:txt").unwrap();
    let hidden = HashSet::new();
    for sort in [Sort::Name, Sort::Size, Sort::Modified] {
        let mut expected: Vec<_> = (1..index.count)
            .filter(|id| !index.entry(*id).unwrap().is_dir())
            .collect();
        expected.sort_by(|a, b| {
            let (a, b) = (index.entry(*a).unwrap(), index.entry(*b).unwrap());
            let primary = match sort {
                Sort::Name => std::cmp::Ordering::Equal,
                Sort::Size => b.allocated.cmp(&a.allocated),
                Sort::Modified => b.modified.cmp(&a.modified),
            };
            primary
                .then(index.name(a).cmp(index.name(b)))
                .then(a.id.cmp(&b.id))
        });
        let mut cursor = None;
        let mut seen = vec![];
        let mut pages = vec![];
        loop {
            let page = query::search_page(
                &index,
                &q,
                sort,
                Request {
                    limit: 337,
                    cursor: cursor.as_deref(),
                },
                &hidden,
                None,
            )
            .unwrap();
            assert!(!page.canceled);
            assert!(page.ids.len() <= 337);
            assert_eq!(page.matched, expected.len() as u64);
            assert_eq!(page.paging.offset, seen.len() as u64);
            seen.extend_from_slice(&page.ids);
            cursor = page.paging.next_cursor.clone();
            pages.push(page);
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(seen, expected);
        assert!(pages.len() > 29);
        let mut current = pages.pop().unwrap();
        while let Some(previous) = current.paging.previous_cursor {
            current = query::search_page(
                &index,
                &q,
                sort,
                Request {
                    limit: 337,
                    cursor: Some(&previous),
                },
                &hidden,
                None,
            )
            .unwrap();
            let start = current.paging.offset as usize;
            assert_eq!(current.ids, expected[start..start + current.ids.len()]);
        }
        assert_eq!(current.paging.offset, 0);
        let first = query::search_page(
            &index,
            &q,
            sort,
            Request {
                limit: 337,
                cursor: None,
            },
            &hidden,
            None,
        )
        .unwrap();
        let last = query::search_page(
            &index,
            &q,
            sort,
            Request {
                limit: 337,
                cursor: first.paging.last_cursor.as_deref(),
            },
            &hidden,
            None,
        )
        .unwrap();
        assert_eq!(last.ids, expected[expected.len() - 337..]);
        assert!(last.paging.next_cursor.is_none());
    }
    // Directory Name/Modified paging sees all children, not just the largest subset.
    let mut cursor = None;
    let mut names = vec![];
    loop {
        let page = index
            .children_page(
                0,
                Sort::Name,
                Request {
                    limit: 400,
                    cursor: cursor.as_deref(),
                },
                &hidden,
                None,
            )
            .unwrap();
        assert!(page.largest.len() <= 8);
        names.extend(page.ids);
        cursor = page.paging.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(names.len(), 10059);
    let unique: HashSet<_> = names.iter().collect();
    assert_eq!(unique.len(), names.len());
}
#[test]
fn cursors_reject_query_scope_sort_hidden_and_generation_changes() {
    let (_t, root, state) = fixture();
    for name in ["one.txt", "two.txt", "three.txt"] {
        fs::write(root.join(name), b"x").unwrap();
    }
    fs::create_dir(root.join("private")).unwrap();
    fs::write(root.join("private/secret.txt"), b"private").unwrap();
    let index = scan(&root, &state);
    let empty = HashSet::new();
    let q = Query::parse("ext:txt").unwrap();
    let first = query::search_page(
        &index,
        &q,
        Sort::Name,
        Request {
            limit: 1,
            cursor: None,
        },
        &empty,
        None,
    )
    .unwrap();
    let token = first.paging.next_cursor.unwrap();
    for bad in ["", "not-a-cursor", &token[..100]] {
        assert!(
            query::search_page(
                &index,
                &q,
                Sort::Name,
                Request {
                    limit: 1,
                    cursor: Some(bad)
                },
                &empty,
                None
            )
            .is_err()
        );
    }
    let mut modified = token.clone();
    modified.replace_range(152..168, "ffffffffffffffff");
    assert!(
        query::search_page(
            &index,
            &q,
            Sort::Name,
            Request {
                limit: 1,
                cursor: Some(&modified)
            },
            &empty,
            None
        )
        .is_err()
    );
    let other = Query::parse("one").unwrap();
    assert!(
        query::search_page(
            &index,
            &other,
            Sort::Name,
            Request {
                limit: 1,
                cursor: Some(&token)
            },
            &empty,
            None
        )
        .is_err()
    );
    assert!(
        query::search_page(
            &index,
            &q,
            Sort::Size,
            Request {
                limit: 1,
                cursor: Some(&token)
            },
            &empty,
            None
        )
        .is_err()
    );
    let mut scoped = Query::parse("ext:txt").unwrap();
    scoped.set_scope(index.find_path(&root.join("private")).unwrap());
    assert!(
        query::search_page(
            &index,
            &scoped,
            Sort::Name,
            Request {
                limit: 1,
                cursor: Some(&token)
            },
            &empty,
            None
        )
        .is_err()
    );
    assert!(
        query::search_page(
            &index,
            &q,
            Sort::Name,
            Request {
                limit: 1,
                cursor: Some(&token)
            },
            &HashSet::from([first.ids[0]]),
            None
        )
        .is_err()
    );
    fs::write(root.join("new.txt"), b"new").unwrap();
    let fresh = scan(&root, &state);
    assert!(
        query::search_page(
            &fresh,
            &q,
            Sort::Name,
            Request {
                limit: 1,
                cursor: Some(&token)
            },
            &empty,
            None
        )
        .is_err()
    );
    assert!(
        query::search_page(
            &index,
            &q,
            Sort::Name,
            Request {
                limit: 1,
                cursor: Some(&token)
            },
            &empty,
            None
        )
        .is_ok(),
        "Old immutable readers remain valid"
    );
    let generation = AtomicU64::new(2);
    let canceled = query::search_page(
        &index,
        &q,
        Sort::Name,
        Request {
            limit: 2,
            cursor: None,
        },
        &empty,
        Some((&generation, 1)),
    )
    .unwrap();
    assert!(canceled.canceled && canceled.ids.is_empty() && canceled.paging.next_cursor.is_none());
}
#[test]
fn relative_age_is_frozen_for_a_page_sequence() {
    let (_t, root, state) = fixture();
    let now = SystemTime::now();
    for name in ["a.txt", "b.txt", "c.txt"] {
        fs::write(root.join(name), b"x").unwrap();
        fs::File::open(root.join(name))
            .unwrap()
            .set_modified(now - Duration::from_secs(5))
            .unwrap();
    }
    fs::write(root.join("future.txt"), b"later").unwrap();
    fs::File::open(root.join("future.txt"))
        .unwrap()
        .set_modified(now + Duration::from_secs(3))
        .unwrap();
    let index = scan(&root, &state);
    let hidden = HashSet::new();
    let first = query::search_page(
        &index,
        &Query::parse("older:0d ext:txt").unwrap(),
        Sort::Name,
        Request {
            limit: 1,
            cursor: None,
        },
        &hidden,
        None,
    )
    .unwrap();
    assert_eq!(first.matched, 3);
    std::thread::sleep(Duration::from_millis(3200));
    let q = Query::parse("older:0d ext:txt").unwrap();
    let next = query::search_page(
        &index,
        &q,
        Sort::Name,
        Request {
            limit: 1,
            cursor: first.paging.next_cursor.as_deref(),
        },
        &hidden,
        None,
    )
    .unwrap();
    assert_eq!(next.matched, 3);
    assert_eq!(
        query::search(&index, &q, Sort::Name, 100, &hidden, None).matched,
        4
    );
}
