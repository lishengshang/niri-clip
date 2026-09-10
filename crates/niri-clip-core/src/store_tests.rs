use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

/// 目录级隔离 + 串行化：通过 XDG_* 环境变量把所有持久化位置指进临时目录，
/// 测试互不影响且不会触碰真实用户目录。锁用全局共享的 test_util::ENV_LOCK
/// （store/config/tui 三处测试必须互斥，见 lib.rs 注释）
use crate::test_util::ENV_LOCK;
static SEQ: AtomicUsize = AtomicUsize::new(0);

struct EnvGuard {
    prev_state: Option<String>,
    prev_config: Option<String>,
    prev_cache: Option<String>,
    root: PathBuf,
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        restore_var("XDG_STATE_HOME", &self.prev_state);
        restore_var("XDG_CONFIG_HOME", &self.prev_config);
        restore_var("XDG_CACHE_HOME", &self.prev_cache);
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn restore_var(key: &str, val: &Option<String>) {
    match val {
        Some(v) => std::env::set_var(key, v),
        None => std::env::remove_var(key),
    }
}

fn with_env(f: impl FnOnce(&EnvGuard)) {
    let _g = ENV_LOCK.lock().unwrap();
    let seq = SEQ.fetch_add(1, Ordering::SeqCst);
    let root = std::env::temp_dir().join(format!("niri-clip-ut-{}-{}", std::process::id(), seq));
    std::fs::create_dir_all(root.join("state")).unwrap();

    let prev_state = std::env::var("XDG_STATE_HOME").ok();
    let prev_config = std::env::var("XDG_CONFIG_HOME").ok();
    let prev_cache = std::env::var("XDG_CACHE_HOME").ok();
    std::env::set_var("XDG_STATE_HOME", root.join("state"));
    std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
    std::env::set_var("XDG_CACHE_HOME", root.join("cache"));

    let guard = EnvGuard {
        prev_state,
        prev_config,
        prev_cache,
        root,
    };
    f(&guard);
    drop(guard);
}

fn clear_db() {
    let p = Config::db_path();
    let _ = std::fs::remove_file(&p);
    let _ = std::fs::remove_file(p.with_extension("sqlite-wal"));
    let _ = std::fs::remove_file(p.with_extension("sqlite-shm"));
}

#[test]
fn should_ignore_filters_secrets_and_short_input() {
    let cfg = Config::default();
    assert!(should_ignore("my password is hunter2", &cfg));
    assert!(should_ignore("", &cfg), "空文本应被忽略");
    let cfg2 = Config {
        min_store_length: 5,
        ..Config::default()
    };
    assert!(should_ignore("ab", &cfg2));
    assert!(!should_ignore("hello world", &cfg));
    assert!(!should_ignore("plain text", &cfg));
}

#[test]
fn upsert_dedups_same_hash_atomically() {
    with_env(|_| {
        clear_db();
        assert!(insert("dup-entry-a".into(), None).unwrap());
        assert!(!insert("dup-entry-a".into(), None).unwrap());
        let all = list(100).unwrap();
        let hits = all
            .iter()
            .filter(|c| c.text.contains("dup-entry-a"))
            .count();
        assert_eq!(hits, 1, "相同文本应只占一行");
    });
}

#[test]
fn insert_trims_whitespace_and_dedups_variants() {
    with_env(|_| {
        clear_db();
        // 带首尾空白与纯净版视为同一 hash：消除 watch 管道与
        // try_system_capture 的 trim 语义分歧导致的孪生条目
        assert!(insert("dup-entry-x\n".into(), None).unwrap());
        assert!(!insert("dup-entry-x".into(), None).unwrap());
        assert!(!insert("  dup-entry-x  ".into(), None).unwrap());
        let all = list(100).unwrap();
        let hits = all.iter().filter(|c| c.text == "dup-entry-x").count();
        assert_eq!(hits, 1, "空白变体应只占一行且入库为 trim 后文本");
        // 纯空白不入库
        assert!(!insert("   \n\t ".into(), None).unwrap());
    });
}

#[test]
fn busy_timeout_is_set_on_connection() {
    with_env(|_| {
        clear_db();
        let conn = connect().unwrap();
        let v: i64 = conn
            .query_row("PRAGMA busy_timeout", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, BUSY_TIMEOUT_MS as i64);
        let uv: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(uv, 4, "schema 应迁移到版本 4（FTS5 + blake3 统一）");
    });
}

#[test]
fn image_insert_associates_data_file_and_dedups_by_content_not_length() {
    with_env(|_| {
        clear_db();
        let a = b"\x89PNG\r\n\x1a\n bytes-of-A ".to_vec();
        let img_a = insert_image("image/png", &a)
            .unwrap()
            .expect("first insert");
        let clip_a = get(img_a.id).unwrap();
        assert_eq!(
            clip_a.image_path.as_deref(),
            Some(img_a.path.to_string_lossy().as_ref()),
            "clip 行应记录自身数据文件路径"
        );
        assert!(img_a.path.exists());

        // 相同内容重复拷贝 -> 判重，仅刷时间戳
        let again = insert_image("image/png", &a).unwrap();
        assert!(again.is_none());

        // 相同字节长度但内容不同 -> 不得再因 len 判重（修复点）
        let mut b = a.clone();
        b[10] ^= 0xFF;
        let img_b = insert_image("image/png", &b)
            .unwrap()
            .expect("len equal but different bytes");
        assert_ne!(img_b.id, img_a.id);

        let conn = Connection::open(Config::db_path()).unwrap();
        let total: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM clips WHERE mime LIKE 'image/%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(total, 2);
    });
}

#[test]
fn insert_rejects_oversize_text_at_boundary() {
    with_env(|g| {
        clear_db();
        // 通过测试隔离环境写入小限额配置，Config::load() 在 insert 内生效
        let cfg_dir = g.root.join("config/niri-clip");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(cfg_dir.join("config.toml"), "max_clip_bytes = 64\n").unwrap();

        let at_limit = "x".repeat(64);
        assert!(insert(at_limit, None).unwrap(), "恰好达到上限应入库");
        let over = "y".repeat(65);
        assert!(!insert(over, None).unwrap(), "超限一个字节即拒绝");

        let all = list(10).unwrap();
        assert_eq!(all.len(), 1, "超限条目不得落库");
        assert!(all[0].text.starts_with('x'));
    });
}

#[test]
fn insert_image_rejects_oversize_payload_at_boundary() {
    with_env(|g| {
        clear_db();
        let cfg_dir = g.root.join("config/niri-clip");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(cfg_dir.join("config.toml"), "max_image_bytes = 64\n").unwrap();

        let big = vec![0u8; 65];
        assert!(
            insert_image("image/png", &big).unwrap().is_none(),
            "超限图片应拒绝且不产生数据文件"
        );
        assert!(!Config::images_dir().join("1.bin").exists());

        let ok = vec![0u8; 64];
        let img = insert_image("image/png", &ok)
            .unwrap()
            .expect("恰好达到上限应入库");
        assert!(img.path.exists());
    });
}

#[test]
fn current_pointer_tracks_capture_and_tops_list() {
    with_env(|_| {
        clear_db();
        insert("old-a".into(), None).unwrap();
        insert("old-b".into(), None).unwrap();
        assert!(current_hash().is_some(), "insert 成功即写指针");
        let all = list(10).unwrap();
        assert_eq!(all[0].text, "old-b", "最后捕获者置顶");

        // 星标压不过当前项：当前项永远第 1 行
        let pinned_id = all[1].id; // old-a
        toggle_pin(pinned_id).unwrap();
        assert_eq!(list(10).unwrap()[0].text, "old-b", "星标不得顶掉当前项");

        // 重复捕获（dedup 刷 ts 路径）同样刷新指针
        insert("old-a".into(), None).unwrap();
        let cur = current_hash().unwrap();
        assert_eq!(list(10).unwrap()[0].hash, cur, "▶ 应跟随最后一次捕获");
    });
}

#[test]
fn oversize_or_ignored_capture_does_not_move_current_pointer() {
    with_env(|g| {
        clear_db();
        insert("keep-me".into(), None).unwrap();
        let cur = current_hash().unwrap();

        // 超限拒绝不写指针
        let cfg_dir = g.root.join("config/niri-clip");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(cfg_dir.join("config.toml"), "max_clip_bytes = 8\n").unwrap();
        insert("this is way beyond eight bytes".into(), None).unwrap();
        assert_eq!(current_hash().unwrap(), cur, "超限捕获不得移动 ▶");
        // ignore_regex 命中不写指针
        insert("my password is hunter2".into(), None).unwrap();
        assert_eq!(current_hash().unwrap(), cur, "被过滤捕获不得移动 ▶");
    });
}

#[test]
fn legacy_cache_db_is_snapshotted_into_state_dir() {
    with_env(|g| {
        // 在旧的 ~/.cache 位置构造一个含数据的历史库
        let legacy = Config::legacy_db_path();
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        {
            let lc = Connection::open(&legacy).unwrap();
            lc.execute_batch(
                "CREATE TABLE clips (id INTEGER PRIMARY KEY AUTOINCREMENT, hash TEXT UNIQUE,
                 text TEXT NOT NULL, mime TEXT DEFAULT 'text/plain', ts INTEGER NOT NULL,
                 pinned INTEGER DEFAULT 0, size INTEGER);
                 INSERT INTO clips(hash, text, ts) VALUES ('legacy-1','old entry',1);",
            )
            .unwrap();
        }
        // 环境已把新状态目录指向临时区，任何一次 connect() 都应触发搬迁
        assert!(insert("new entry".into(), None).unwrap());
        let all = list(50).unwrap();
        assert!(
            all.iter().any(|c| c.text == "old entry"),
            "旧库条目应出现在迁移后的新库中"
        );
        assert!(
            Config::db_path().exists(),
            "新库应位于 {:?}",
            g.root.join("state")
        );
    });
}

#[test]
fn pin_orders_first_and_list_respects_limit() {
    with_env(|_| {
        clear_db();
        for i in 0..5 {
            insert(format!("item-{i}"), None).unwrap();
        }
        let all = list(TUI_LIMIT).unwrap();
        let head_id = all[0].id;
        toggle_pin(head_id).unwrap();
        let after = list(TUI_LIMIT).unwrap();
        assert_eq!(after[0].id, head_id, "pinned 应置顶");
        assert!(after[0].pinned);
        let few = list(3).unwrap();
        assert_eq!(few.len(), 3);
    });
}

#[test]
fn delete_and_wipe_remove_image_files() {
    with_env(|_| {
        clear_db();
        let a = insert_image("image/png", b"\x89PNG-a")
            .unwrap()
            .expect("insert a");
        let b = insert_image("image/png", b"\x89PNG-b")
            .unwrap()
            .expect("insert b");
        assert!(a.path.exists() && b.path.exists());
        delete(a.id).unwrap();
        assert!(!a.path.exists(), "delete 应同步删除数据文件");
        assert!(b.path.exists());
        wipe().unwrap();
        assert!(!b.path.exists(), "wipe 应清空所有数据文件");
    });
}

#[test]
fn max_items_eviction_removes_image_files() {
    with_env(|g| {
        clear_db();
        let cfg_dir = g.root.join("config/niri-clip");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(cfg_dir.join("config.toml"), "max_items = 1\n").unwrap();
        let a = insert_image("image/png", b"\x89PNG-a")
            .unwrap()
            .expect("insert a");
        let b = insert_image("image/png", b"\x89PNG-b")
            .unwrap()
            .expect("insert b");
        // b 入库触发淘汰：a（未 pin、更旧）连行带文件一起消失
        assert!(get(a.id).is_err(), "淘汰条目的行应被删除");
        assert!(!a.path.exists(), "淘汰条目的数据文件应被删除");
        assert!(b.path.exists());
    });
}

#[test]
fn prune_orphan_images_removes_unreferenced_files() {
    with_env(|_| {
        clear_db();
        let img = insert_image("image/png", b"\x89PNG-ok")
            .unwrap()
            .expect("insert");
        let dir = Config::images_dir();
        // 旧版本遗留的孤儿：文件在、行不在
        let orphan = dir.join("9999.bin");
        std::fs::write(&orphan, b"orphan").unwrap();
        // 入库中途崩溃的临时文件
        let tmp = dir.join(".tmp-1234.bin");
        std::fs::write(&tmp, b"tmp").unwrap();
        assert_eq!(prune_orphan_images().unwrap(), 2);
        assert!(!orphan.exists() && !tmp.exists());
        assert!(img.path.exists(), "被引用的文件不得误删");
        assert_eq!(prune_orphan_images().unwrap(), 0, "二次清扫应无残留");
    });
}

#[test]
fn gc_images_evicts_oldest_first_and_protects_pinned_and_current() {
    with_env(|_| {
        clear_db();
        // 三张 10 字节小图，间隔毫秒保证 ts 有序（淘汰按 ts ASC, id ASC）
        let a = insert_image("image/png", &[7u8; 10]).unwrap().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(3));
        let b = insert_image("image/png", &[8u8; 10]).unwrap().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(3));
        let c = insert_image("image/png", &[9u8; 10]).unwrap().unwrap();

        // b 星标、c 当前项，均应受保护
        let conn = connect().unwrap();
        conn.execute("UPDATE clips SET pinned=1 WHERE id=?1", params![b.id])
            .unwrap();
        drop(conn);
        assert_eq!(
            current_hash().unwrap(),
            image_content_key("image/png", &[9u8; 10]),
            "c 刚入库应为当前项"
        );

        // 总量 30、配额 25：需释放 ≥5 字节 → 淘汰最旧且未受保护的 a
        assert_eq!(gc_images(25).unwrap(), 1);
        assert!(!a.path.exists(), "被淘汰条目的数据文件应被删除");
        assert!(b.path.exists() && c.path.exists(), "星标/当前项不得误删");
        let conn = connect().unwrap();
        let cnt: i64 = conn
            .query_row("SELECT COUNT(*) FROM clips", [], |r| r.get(0))
            .unwrap();
        assert_eq!(cnt, 2);
    });
}

#[test]
fn gc_images_zero_means_unlimited_and_within_quota_is_noop() {
    with_env(|_| {
        clear_db();
        insert_image("image/png", &[1u8; 10]).unwrap().unwrap();
        // 0 = 不限制；配额内不动任何条目
        assert_eq!(gc_images(0).unwrap(), 0);
        assert_eq!(gc_images(1024).unwrap(), 0);
    });
}

#[test]
fn fts_migration_backfills_and_upgrade_is_lossless() {
    with_env(|_| {
        clear_db();
        // 手工构造 v2 旧库文件（不经过 connect()，否则直接迁满）
        {
            // 裸开不会自建父目录（connect 才会）
            std::fs::create_dir_all(Config::db_path().parent().unwrap()).unwrap();
            let conn = Connection::open(Config::db_path()).unwrap();
            conn.execute_batch(
                "CREATE TABLE clips (id INTEGER PRIMARY KEY AUTOINCREMENT, hash TEXT UNIQUE,
                 text TEXT NOT NULL, mime TEXT DEFAULT 'text/plain', ts INTEGER NOT NULL,
                 pinned INTEGER DEFAULT 0, size INTEGER, image_path TEXT);
                 INSERT INTO clips(hash, text, mime, ts) VALUES
                 ('h1', 'hello legacy world', 'text/plain', 1),
                 ('h2', '旧库存量中文条目', 'text/plain', 2);
                 PRAGMA user_version=2;",
            )
            .unwrap();
        }
        // 下一次 connect() 走 v3 迁移：回填后存量行可搜（中英文）
        assert_eq!(search("legacy world", 10).unwrap().len(), 1);
        assert_eq!(search("存量中文", 10).unwrap().len(), 1);
        let n: i64 = connect()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM clips", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 2, "旧库升级无损：行数不变");
    });
}

#[test]
fn fts_stays_in_sync_with_insert_and_delete() {
    with_env(|_| {
        clear_db();
        insert("the quick brown fox".into(), None).unwrap();
        insert("剪贴板历史管理器条目".into(), None).unwrap();
        assert_eq!(search("quick brown", 10).unwrap().len(), 1);
        let hit = search("板历史", 10).unwrap().pop().unwrap();
        // delete 行删 → 触发器同步出 FTS 索引
        delete(hit.id).unwrap();
        assert!(search("板历史", 10).unwrap().is_empty());
        assert_eq!(search("quick brown", 10).unwrap().len(), 1);
    });
}

#[test]
fn search_short_query_falls_back_to_like_and_escapes_wildcards() {
    with_env(|_| {
        clear_db();
        insert("100% pure path example".into(), None).unwrap();
        insert("ab filler".into(), None).unwrap();
        // <3 字符走 LIKE；通配符按字面匹配（% _ 不展开）
        assert_eq!(search("ab", 10).unwrap().len(), 1);
        assert_eq!(search("%", 10).unwrap().len(), 1, "% 应按字面匹配");
        assert_eq!(
            search("_", 10).unwrap().len(),
            0,
            "_ 不作通配符且数据无字面 _"
        );
        assert!(search("", 10).unwrap().is_empty());
    });
}

#[test]
fn search_match_phrase_syntax_from_user_input_is_safe() {
    with_env(|_| {
        clear_db();
        insert("quoted \"inner\" text".into(), None).unwrap();
        // 双引号在 FTS 查询语法里有含义：翻倍转义后按字面命中且不 panic
        assert_eq!(search("\"inner\"", 10).unwrap().len(), 1);
        assert_eq!(search("inner", 10).unwrap().len(), 1);
        // trigram 按字面索引标点：跨引号的 "inner text" 不命中（符合子串语义）
        assert_eq!(search("inner text", 10).unwrap().len(), 0);
    });
}

/// 任务 2.2（v3→v4）：防翻倍断言。手工构造含"同文本不同 legacy hash"
/// 的 v3 旧库（DefaultHasher 跨编译器不稳定的真实翻倍形态），锁定：
/// 条目数只减不增、文本行全量重算、图片指纹不动、FTS 同步、指针重映射、
/// 幂等、快照落盘（ROADMAP 风险表：迁移出错致数据翻倍/丢失）
#[test]
fn blake3_migration_merges_duplicates_and_never_doubles() {
    with_env(|_| {
        clear_db();
        // 裸开手工建 v3 schema（不经 connect()，否则直接迁满）
        std::fs::create_dir_all(Config::db_path().parent().unwrap()).unwrap();
        let conn = Connection::open(Config::db_path()).unwrap();
        conn.execute_batch(
            "CREATE TABLE clips (id INTEGER PRIMARY KEY AUTOINCREMENT, hash TEXT UNIQUE,
             text TEXT NOT NULL, mime TEXT DEFAULT 'text/plain', ts INTEGER NOT NULL,
             pinned INTEGER DEFAULT 0, size INTEGER, image_path TEXT);
             CREATE VIRTUAL TABLE clips_fts USING fts5(
                 text, content='clips', content_rowid='id', tokenize='trigram');
             CREATE TRIGGER clips_fts_ai AFTER INSERT ON clips BEGIN
                 INSERT INTO clips_fts(rowid, text) VALUES (new.id, new.text); END;
             CREATE TRIGGER clips_fts_ad AFTER DELETE ON clips BEGIN
                 INSERT INTO clips_fts(clips_fts, rowid, text)
                 VALUES ('delete', old.id, old.text); END;
             CREATE TRIGGER clips_fts_au AFTER UPDATE OF text ON clips BEGIN
                 INSERT INTO clips_fts(clips_fts, rowid, text)
                 VALUES ('delete', old.id, old.text);
                 INSERT INTO clips_fts(rowid, text) VALUES (new.id, new.text); END;
             INSERT INTO clips(hash, text, mime, ts, pinned) VALUES
                 ('legacy-a', 'duplicated text', 'text/plain', 100, 1),
                 ('legacy-b', 'duplicated text', 'text/plain', 200, 0),
                 ('legacy-c', 'unique text', 'text/plain', 300, 0),
                 ('legacy-d', '中文存量条目', 'text/plain', 400, 0);
             INSERT INTO clips(hash, text, mime, ts, size) VALUES
                 ('img:image/png:abc-10', '[image image/png 10 bytes]', 'image/png', 500, 10);
             PRAGMA user_version=3;",
        )
        .unwrap();
        // ▶ 指针指向将被合并掉的 legacy-a：迁移后必须重映射到幸存行
        std::fs::create_dir_all(Config::state_dir()).unwrap();
        std::fs::write(Config::state_dir().join("current"), "legacy-a").unwrap();
        drop(conn);

        let conn = connect().unwrap();
        let ver: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(ver, 4, "迁移必须推进到 v4");

        // 防翻倍断言：5 行（4 文本 + 1 图片）合并 1 条重复 → 4 行，只减不增
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM clips", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 4, "合并重复后条目数只减不增");

        // 幸存行 = ts 最大那份（legacy-b），hash 重算为 blake3 且星标取 OR
        let (hash, pinned): (String, i64) = conn
            .query_row(
                "SELECT hash, pinned FROM clips WHERE text='duplicated text'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            hash,
            hash_text("duplicated text"),
            "文本 hash 必须重算为 blake3"
        );
        assert_eq!(pinned, 1, "任一重复行被星标则合并后保留星标");

        // 全部文本行 hash 与 blake3(text) 一致；图片指纹原样不动
        let mut stmt = conn
            .prepare("SELECT hash, text FROM clips WHERE mime='text/plain'")
            .unwrap();
        let mut rows = stmt.query([]).unwrap();
        while let Some(r) = rows.next().unwrap() {
            let h: String = r.get(0).unwrap();
            let t: String = r.get(1).unwrap();
            assert_eq!(h, hash_text(&t), "所有文本行均须重算");
        }
        drop(rows);
        drop(stmt);
        let img: String = conn
            .query_row("SELECT hash FROM clips WHERE mime='image/png'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(img, "img:image/png:abc-10", "图片 FNV 指纹不走 blake3");

        // FTS 由触发器自动同步：被并行出索引，幸存行中英文均可搜
        let hits: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM clips_fts WHERE clips_fts MATCH 'duplicated'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hits, 1, "被并行已从 FTS 删除，幸存行可搜");
        let hits: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM clips_fts WHERE clips_fts MATCH '中文存量'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hits, 1, "中文行迁移后仍可搜");

        // ▶ 指针重映射到幸存行新 hash
        let cur = std::fs::read_to_string(Config::state_dir().join("current")).unwrap();
        assert_eq!(cur.trim(), hash, "指针必须重映射，否则 ▶ 静默失效");

        // 快照必须落盘（事后回滚保险）
        assert!(
            Config::state_dir().join("db.sqlite.pre-blake3").exists(),
            "迁移前 VACUUM INTO 快照必须存在"
        );

        // 幂等：二次 connect 不重复迁移、不再翻倍
        drop(conn);
        let conn = connect().unwrap();
        let n2: i64 = conn
            .query_row("SELECT COUNT(*) FROM clips", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n2, 4, "二次 connect 幂等");
    });
}

// =====================================================================
// 任务 2.3：stats / vacuum / prune
// =====================================================================

#[test]
fn stats_counts_and_sizes_match_disk() {
    with_env(|_| {
        clear_db();
        insert("hello world".to_string(), None).unwrap();
        insert("second entry".to_string(), None).unwrap();
        let img = insert_image("image/png", &[7u8; 100]).unwrap().unwrap();
        let conn = connect().unwrap();
        conn.execute(
            "UPDATE clips SET pinned=1 WHERE hash=?1",
            params![hash_text("hello world")],
        )
        .unwrap();
        drop(conn);

        let s = stats().unwrap();
        assert_eq!(s.total, 3);
        assert_eq!(s.pinned, 1);
        assert_eq!(s.image_entries, 1);
        assert_eq!(
            s.text_bytes,
            ("hello world".len() + "second entry".len()) as i64
        );
        assert!(s.db_bytes > 0, "库体积应为磁盘实测值");
        assert_eq!(
            s.images_disk_bytes,
            std::fs::metadata(&img.path).unwrap().len(),
            "images 目录体积应为实测值"
        );
        assert!(s.oldest_ts.is_some() && s.newest_ts.is_some());
    });
}

#[test]
fn vacuum_runs_and_keeps_db_queryable() {
    with_env(|_| {
        clear_db();
        for i in 0..50 {
            insert(format!("entry {i} payload"), None).unwrap();
        }
        let conn = connect().unwrap();
        conn.execute("DELETE FROM clips WHERE id % 2 = 0", [])
            .unwrap();
        drop(conn);

        let (before, after) = vacuum().unwrap();
        assert!(after <= before, "VACUUM 后库不得变大: {before} -> {after}");
        // 压缩后数据完好、可正常查询
        assert_eq!(list(100).unwrap().len(), 25);
    });
}

#[test]
fn prune_deletes_old_only_and_protects_pinned_and_current() {
    with_env(|_| {
        clear_db();
        let old: i64 = 1_000; // 1970 年，早于任何现实 cutoff
        let img = insert_image("image/png", &[9u8; 10]).unwrap().unwrap();
        for t in ["old plain", "old pinned", "old current", "fresh plain"] {
            insert(t.to_string(), None).unwrap();
        }
        let conn = connect().unwrap();
        for h in ["old plain", "old pinned", "old current"] {
            conn.execute(
                "UPDATE clips SET ts=?1 WHERE hash=?2",
                params![old, hash_text(h)],
            )
            .unwrap();
        }
        conn.execute("UPDATE clips SET ts=?1 WHERE id=?2", params![old, img.id])
            .unwrap();
        conn.execute(
            "UPDATE clips SET pinned=1 WHERE hash=?1",
            params![hash_text("old pinned")],
        )
        .unwrap();
        drop(conn);
        // 入库路径已把指针刷到 "fresh plain"（最后一条），显式指回受保护的旧条目
        touch_current(&hash_text("old current"));

        let cutoff = parse_local_date_ms("2026-08-01").unwrap();
        let out = prune_before(cutoff, false).unwrap();
        // 应删：old plain + 旧图片；受保护：old pinned（星标）/ old current（▶）
        assert_eq!(out.deleted, 2);
        assert_eq!(out.images_deleted, 1);
        assert_eq!(out.freed_bytes, ("old plain".len() + 10) as i64);
        assert!(!img.path.exists(), "被 prune 条目的数据文件应随行删除");

        let conn = connect().unwrap();
        let cnt: i64 = conn
            .query_row("SELECT COUNT(*) FROM clips", [], |r| r.get(0))
            .unwrap();
        assert_eq!(cnt, 3, "幸存行应为 pinned/current/fresh 三条");
        drop(conn);

        // FTS 触发器同步：被删文本不再可搜，幸存者仍可搜
        assert!(search("old plain", 10).unwrap().is_empty());
        assert_eq!(search("old pinned", 10).unwrap().len(), 1);
        assert_eq!(search("old current", 10).unwrap().len(), 1);
        assert_eq!(search("fresh plain", 10).unwrap().len(), 1);

        // 指针不得被 prune 波及
        assert_eq!(current_hash().unwrap(), hash_text("old current"));
    });
}

#[test]
fn prune_dry_run_reports_without_deleting() {
    with_env(|_| {
        clear_db();
        let old: i64 = 1_000;
        let img = insert_image("image/png", &[9u8; 10]).unwrap().unwrap();
        insert("old plain".to_string(), None).unwrap();
        insert("fresh plain".to_string(), None).unwrap();
        let conn = connect().unwrap();
        conn.execute(
            "UPDATE clips SET ts=?1 WHERE hash=?2",
            params![old, hash_text("old plain")],
        )
        .unwrap();
        conn.execute("UPDATE clips SET ts=?1 WHERE id=?2", params![old, img.id])
            .unwrap();
        drop(conn);

        let cutoff = parse_local_date_ms("2026-08-01").unwrap();
        let out = prune_before(cutoff, true).unwrap();
        assert_eq!(out.deleted, 2);
        assert_eq!(out.images_deleted, 1);
        assert_eq!(out.freed_bytes, ("old plain".len() + 10) as i64);

        // 数据原封不动
        let conn = connect().unwrap();
        let cnt: i64 = conn
            .query_row("SELECT COUNT(*) FROM clips", [], |r| r.get(0))
            .unwrap();
        assert_eq!(cnt, 3, "dry-run 不得删行");
        drop(conn);
        assert!(img.path.exists(), "dry-run 不得删数据文件");

        // 预览数字与真删一致
        let out2 = prune_before(cutoff, false).unwrap();
        assert_eq!(out2.deleted, out.deleted);
        assert_eq!(out2.images_deleted, out.images_deleted);
        assert_eq!(out2.freed_bytes, out.freed_bytes);
        assert!(!img.path.exists());
    });
}

#[test]
fn parse_local_date_ms_uses_local_midnight_and_rejects_garbage() {
    use chrono::TimeZone;
    let want = chrono::Local
        .with_ymd_and_hms(2026, 8, 1, 0, 0, 0)
        .single()
        .unwrap()
        .timestamp_millis();
    assert_eq!(parse_local_date_ms("2026-08-01").unwrap(), want);
    assert_eq!(
        parse_local_date_ms(" 2026-08-01 ").unwrap(),
        want,
        "容忍首尾空白"
    );
    assert!(parse_local_date_ms("not-a-date").is_err());
    assert!(parse_local_date_ms("2026-13-01").is_err(), "非法月份应报错");
    assert!(parse_local_date_ms("").is_err());
}

// =====================================================================
// 任务 2.4：导出/回灌（backup.rs，NDJSON v1，格式选型见 ADR-004）
// =====================================================================

use crate::backup::{self, EXPORT_FORMAT, EXPORT_VERSION};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;

fn read_lines(p: &Path) -> Vec<String> {
    std::fs::read_to_string(p)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

fn write_lines(p: &Path, lines: &[String]) {
    std::fs::write(p, lines.join("\n") + "\n").unwrap();
}

#[test]
fn export_import_round_trip() {
    with_env(|g| {
        clear_db();
        insert("rt-alpha".into(), None).unwrap();
        insert("rt-beta\n".into(), None).unwrap();
        let img_bytes = b"\x89PNG\r\n\x1a\n-fake-payload".to_vec();
        insert_image("image/png", &img_bytes).unwrap().unwrap();
        let alpha_id = list(100)
            .unwrap()
            .iter()
            .find(|c| c.text == "rt-alpha")
            .unwrap()
            .id;
        toggle_pin(alpha_id).unwrap();

        let exp = g.root.join("exp.ndjson");
        let out = backup::export_json_file(Some(&exp)).unwrap();
        assert_eq!(out.count, 3);
        assert_eq!(out.images, 1);
        assert_eq!(out.image_bytes, img_bytes.len() as u64);

        let pointer_before = current_hash();
        assert!(pointer_before.is_some(), "捕获路径已刷新 ▶ 指针");
        wipe().unwrap();
        assert!(list(100).unwrap().is_empty());

        let r = backup::import_file(&exp, false).unwrap();
        assert_eq!(
            (r.imported, r.exists, r.invalid),
            (3, 0, 0),
            "全量回灌到空库"
        );
        let all = list(100).unwrap();
        assert_eq!(all.len(), 3);
        assert!(
            all.iter().any(|c| c.text == "rt-alpha" && c.pinned),
            "星标须保留"
        );
        assert!(all.iter().any(|c| c.text == "rt-beta"));
        let restored = all
            .iter()
            .find(|c| c.mime == "image/png")
            .expect("图片条目须回灌");
        let restored_path = restored.image_path.as_deref().expect("image_path 须重建");
        assert_eq!(
            std::fs::read(restored_path).unwrap(),
            img_bytes,
            "图片字节须逐字节一致"
        );
        // ▶ 指针不被 import 触碰（import 不是捕获，不打扰时序）
        assert_eq!(current_hash(), pointer_before);
    });
}

#[test]
fn import_idempotent_no_dupes() {
    with_env(|g| {
        clear_db();
        insert("idem-1".into(), None).unwrap();
        insert("idem-2".into(), None).unwrap();
        let exp = g.root.join("idem.ndjson");
        backup::export_json_file(Some(&exp)).unwrap();
        wipe().unwrap();

        let r1 = backup::import_file(&exp, false).unwrap();
        assert_eq!((r1.imported, r1.exists), (2, 0));
        let r2 = backup::import_file(&exp, false).unwrap();
        assert_eq!((r2.imported, r2.exists), (0, 2), "二次回灌必须全部幂等跳过");
        assert_eq!(list(100).unwrap().len(), 2);
    });
}

#[test]
fn import_preserves_original_ts() {
    with_env(|g| {
        clear_db();
        insert("ts-keep".into(), None).unwrap();
        let conn = connect().unwrap();
        let ts_before: i64 = conn
            .query_row("SELECT ts FROM clips WHERE text='ts-keep'", [], |r| {
                r.get(0)
            })
            .unwrap();
        drop(conn);
        let exp = g.root.join("ts.ndjson");
        backup::export_json_file(Some(&exp)).unwrap();
        wipe().unwrap();
        // 若回灌把 ts 重置为 now，20ms 足以让两者可辨
        std::thread::sleep(std::time::Duration::from_millis(20));
        backup::import_file(&exp, false).unwrap();
        let conn = connect().unwrap();
        let ts_after: i64 = conn
            .query_row("SELECT ts FROM clips WHERE text='ts-keep'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(ts_after, ts_before, "回灌不得重置时间戳");
    });
}

#[test]
fn import_skips_corrupt_but_keeps_good() {
    with_env(|g| {
        clear_db();
        insert("good-entry".into(), None).unwrap();
        let exp = g.root.join("good.ndjson");
        backup::export_json_file(Some(&exp)).unwrap();
        let lines = read_lines(&exp);
        assert_eq!(lines.len(), 2);

        // 篡改 text：blake3 重算不再等于 hash，完整性校验须拦截
        let mut tampered: serde_json::Value = serde_json::from_str(&lines[1]).unwrap();
        tampered["text"] = serde_json::Value::String("tampered!".into());
        let bad = serde_json::to_string(&tampered).unwrap();
        let exp2 = g.root.join("mixed.ndjson");
        write_lines(&exp2, &[lines[0].clone(), lines[1].clone(), bad]);
        wipe().unwrap();

        let r = backup::import_file(&exp2, false).unwrap();
        assert_eq!(r.imported, 1, "好条目照常入库");
        assert_eq!(r.invalid, 1, "被篡改条目须被拦截");
        let all = list(100).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].text, "good-entry");
    });
}

#[test]
fn import_dry_run_stats_match_real_run() {
    with_env(|g| {
        clear_db();
        insert("good-1".into(), None).unwrap();
        insert("good-2".into(), None).unwrap();
        let exp = g.root.join("stats.ndjson");
        backup::export_json_file(Some(&exp)).unwrap();
        let lines = read_lines(&exp);
        assert_eq!(lines.len(), 3);

        // header + good-1 ×2（同文件重复 hash）+ 篡改行（good-2 改文本）
        let mut tampered: serde_json::Value = serde_json::from_str(&lines[2]).unwrap();
        tampered["text"] = serde_json::Value::String("tampered!".into());
        let mixed = vec![
            lines[0].clone(),
            lines[1].clone(),
            lines[1].clone(),
            serde_json::to_string(&tampered).unwrap(),
        ];
        let f = g.root.join("mixed.ndjson");
        write_lines(&f, &mixed);
        wipe().unwrap();

        // dry-run 统计须与实际执行同口径：重复行收敛为 exists、invalid 计数
        let dry = backup::import_file(&f, true).unwrap();
        assert_eq!(
            (dry.imported, dry.exists, dry.invalid),
            (1, 1, 1),
            "dry-run：同文件重复计 exists，篡改行计 invalid"
        );
        let real = backup::import_file(&f, false).unwrap();
        assert_eq!(
            (real.imported, real.exists, real.invalid),
            (1, 1, 1),
            "实际执行与 dry-run 同口径"
        );
        assert_eq!(list(100).unwrap().len(), 1);
    });
}

#[test]
fn import_enforces_max_items_and_protects_pinned() {
    with_env(|g| {
        clear_db();
        // 小上限配置：import_file 内部 Config::load 经 XDG_CONFIG_HOME 读取
        let cfg_dir = g.root.join("config/niri-clip");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(cfg_dir.join("config.toml"), "max_items = 2\n").unwrap();

        // 手工构造 4 条合法条目（1 条星标）：绕开 insert 路径的入库即裁剪，
        // 才能让回灌数据量超过 max_items
        let mut lines = vec![format!(
            "{{\"format\":\"{EXPORT_FORMAT}\",\"version\":{EXPORT_VERSION},\"user_version\":4,\"exported_at\":0,\"count\":4}}"
        )];
        for i in 1..=4 {
            let text = format!("imp-{i}");
            lines.push(format!(
                "{{\"hash\":\"{}\",\"text\":\"{text}\",\"mime\":\"text/plain\",\"ts\":{},\"pinned\":{},\"size\":{}}}",
                hash_text(&text),
                i * 1000,
                i == 1,
                text.len()
            ));
        }
        let exp = g.root.join("many.ndjson");
        write_lines(&exp, &lines);

        let r = backup::import_file(&exp, false).unwrap();
        assert_eq!(r.imported, 4);
        let all = list(100).unwrap();
        assert_eq!(all.len(), 2, "import 结束须按 max_items 裁剪");
        // 幸存者 = 星标 + 最新；最旧的非星标被淘汰（与捕获路径同一语义）
        assert!(all.iter().any(|c| c.text == "imp-1" && c.pinned));
        assert!(all.iter().any(|c| c.text == "imp-4"));
        assert!(!all.iter().any(|c| c.text == "imp-2" || c.text == "imp-3"));
    });
}

#[test]
fn export_header_and_image_entry_schema() {
    with_env(|g| {
        clear_db();
        insert("schema-text".into(), None).unwrap();
        let payload = b"\xff\xd8\xff-schema-jpeg".to_vec();
        insert_image("image/jpeg", &payload).unwrap().unwrap();

        let exp = g.root.join("schema.ndjson");
        backup::export_json_file(Some(&exp)).unwrap();
        let lines = read_lines(&exp);

        let header: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(header["format"], EXPORT_FORMAT);
        assert_eq!(header["version"], EXPORT_VERSION);
        assert_eq!(header["user_version"], 4, "与 migrate.rs 当前 schema 对齐");
        assert_eq!(header["count"], 2);

        let img_line = lines
            .iter()
            .find(|l| l.contains("image_base64"))
            .expect("图片行须内嵌载荷");
        let entry: serde_json::Value = serde_json::from_str(img_line).unwrap();
        assert!(entry["image_path"].is_null(), "本机绝对路径不得导出");
        let decoded = BASE64
            .decode(entry["image_base64"].as_str().unwrap())
            .unwrap();
        assert_eq!(decoded, payload, "载荷字节须无损");
        assert_eq!(
            image_content_key("image/jpeg", &decoded),
            entry["hash"].as_str().unwrap(),
            "hash 须与字节重算一致（回灌幂等键）"
        );
        // 文本行不携带 image_base64 键（skip_serializing_if）
        let text_line = lines.iter().find(|l| l.contains("schema-text")).unwrap();
        assert!(!text_line.contains("image_base64"));
    });
}

#[test]
fn export_sqlite_snapshot_roundtrip_and_no_overwrite() {
    with_env(|g| {
        clear_db();
        insert("snap-1".into(), None).unwrap();
        insert("snap-2".into(), None).unwrap();
        let snap = g.root.join("snap/db.snapshot.sqlite");
        backup::export_sqlite(&snap).unwrap();

        // 物理快照是标准 sqlite 库：直接打开应可见全部行
        let conn = rusqlite::Connection::open(&snap).unwrap();
        let cnt: i64 = conn
            .query_row("SELECT COUNT(*) FROM clips", [], |r| r.get(0))
            .unwrap();
        drop(conn);
        assert_eq!(cnt, 2, "物理快照须包含全部行");

        assert!(
            backup::export_sqlite(&snap).is_err(),
            "已存在目标必须拒绝覆盖（备份命令绝不静默覆盖）"
        );
    });
}
